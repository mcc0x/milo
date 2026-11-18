//! The project as a strip, coloured by what is not yet committed or pushed.
//!
//! A tall pane shows the file tree. A short one shows only the pending files,
//! flowed across its width. A wide one adds what happened to each file.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::git::{FileChange, Kind, Repo};
use crate::icons;
use crate::term::{self, Key, Screen, Style};
use crate::tree::Tree;
use crate::ui::{self, DIM};

/// At or below this height the pane is a horizontal strip.
const STRIP_ROWS: usize = 7;
/// From this width each file also says what happened to it.
const WIDE_COLS: usize = 44;
/// From this height the last row lists the keys.
const HINT_ROWS: usize = 12;
/// Columns for "edited", "removed", … and for "+1234 −1234" beside them.
const WORD_W: usize = 8;
const SIZE_W: usize = 11;
/// How often the repository is checked for changes.
const CHECK: Duration = Duration::from_secs(2);
/// How long a row that just changed stays marked.
const FRESH: Duration = Duration::from_secs(4);

/// What is pending beneath one directory.
#[derive(Default, Clone, Copy)]
struct Sum {
    files: u32,
    add: u32,
    del: u32,
}

/// What a pending file looked like at one load, to tell when it changes.
type Seen = (Kind, u32, u32, Option<SystemTime>);

pub struct Outline {
    repo: Repo,
    /// Pending files; one both committed and edited again appears twice,
    /// the uncommitted state first.
    files: Vec<FileChange>,
    tree: Tree,
    /// Directories the user opened, and ones they closed again.
    expanded: HashSet<String>,
    collapsed: HashSet<String>,
    /// Pending work beneath each directory; these open unless closed by hand.
    sums: HashMap<String, Sum>,
    /// Paths, and the directories above them, with work not committed yet.
    uncommitted: HashSet<String>,
    /// Pending files, each once, most recently written first.
    recent: Vec<usize>,
    /// Files that changed while the outline was open, and when.
    fresh: HashMap<String, Instant>,
    checked: Instant,
    changed_only: bool,
    /// Every directory is open, until toggled off again.
    all_open: bool,
    /// Whether this pane has the keyboard, as far as the terminal says.
    focused: bool,
    sel: usize,
    top: usize,
    /// First column shown when the strip has more than fits.
    strip_col: usize,
    horizontal: bool,
    /// The rows between heading and hint: (first row, height).
    body: (usize, usize),
}

impl Outline {
    pub fn new(root: PathBuf) -> Outline {
        let mut outline = Outline {
            repo: Repo::open(root),
            files: Vec::new(),
            tree: Tree::default(),
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            sums: HashMap::new(),
            uncommitted: HashSet::new(),
            recent: Vec::new(),
            fresh: HashMap::new(),
            checked: Instant::now(),
            changed_only: false,
            all_open: false,
            focused: true,
            sel: 0,
            top: 0,
            strip_col: 0,
            horizontal: false,
            body: (0, 0),
        };
        outline.load();
        // Start where the work is, not at the top of the tree.
        let first = outline.tree.rows.iter().position(|&(n, _)| {
            let n = &outline.tree.nodes[n as usize];
            !n.dir && n.kind != Kind::None
        });
        outline.sel = first.unwrap_or(0);
        outline
    }

    fn seen(&self) -> HashMap<(String, bool), Seen> {
        let of = |(i, f): (usize, &FileChange)| {
            ((f.path.clone(), f.committed), (f.kind, f.add, f.del, self.repo.modified(i)))
        };
        self.files.iter().enumerate().map(of).collect()
    }

    /// Loads afresh, and marks what differs from the previous load.
    fn reload(&mut self) {
        let before = self.seen();
        self.load();
        let now = Instant::now();
        for (key, seen) in self.seen() {
            if before.get(&key) != Some(&seen) {
                self.fresh.insert(key.0, now);
            }
        }
    }

    fn load(&mut self) {
        let selected = self.selected_path();
        self.files = self.repo.load();
        self.checked = Instant::now();
        let all = self.repo.files();
        let pending = self.files.iter().map(|f| f.path.as_str());
        self.tree = Tree::build(all.iter().map(String::as_str).chain(pending));
        // Reversed, so a file both committed and edited again shows its newest state.
        self.tree.colour(self.files.iter().rev().map(|f| (f.path.as_str(), f.kind)));

        self.sums.clear();
        self.uncommitted.clear();
        self.recent.clear();
        let mut once = HashSet::new();
        for (i, f) in self.files.iter().enumerate() {
            let first = once.insert(f.path.as_str());
            if first {
                self.recent.push(i);
            }
            if !f.committed {
                self.uncommitted.insert(f.path.clone());
            }
            let mut p = f.path.as_str();
            while let Some((dir, _)) = p.rsplit_once('/') {
                if first {
                    let sum = self.sums.entry(dir.to_string()).or_default();
                    sum.files += 1;
                    sum.add += f.add;
                    sum.del += f.del;
                }
                // A folder with pending work opens, and then stays open: a
                // commit must not fold the tree away under the reader.
                self.expanded.insert(dir.to_string());
                if !f.committed {
                    self.uncommitted.insert(dir.to_string());
                }
                p = dir;
            }
        }
        let repo = &self.repo;
        self.recent.sort_by_key(|&i| std::cmp::Reverse(repo.modified(i)));
        self.relayout(selected);
    }

    /// Called when nothing has happened for `idle_ms`.
    pub fn tick(&mut self) {
        self.fresh.retain(|_, at| at.elapsed() < FRESH);
        if self.checked.elapsed() >= CHECK {
            self.checked = Instant::now();
            if self.repo.stale(&self.files) {
                self.reload();
            }
        }
    }

    /// How long to wait for input before the next `tick`.
    pub fn idle_ms(&self) -> i32 {
        let check = CHECK.saturating_sub(self.checked.elapsed());
        let fade = self.fresh.values().map(|at| FRESH.saturating_sub(at.elapsed())).min();
        fade.map_or(check, |f| f.min(check)).as_millis() as i32 + 1
    }

    fn selected_path(&self) -> Option<String> {
        self.tree.row(self.sel).map(|n| n.path.clone())
    }

    fn is_open(&self, path: &str) -> bool {
        !self.collapsed.contains(path) && (self.all_open || self.expanded.contains(path))
    }

    /// Recomputes the visible rows, keeping the cursor on `selected`.
    fn relayout(&mut self, selected: Option<String>) {
        let (expanded, collapsed, all) = (&self.expanded, &self.collapsed, self.all_open);
        let open = |p: &str| !collapsed.contains(p) && (all || expanded.contains(p));
        self.tree.layout(open, self.changed_only);
        let at = selected.and_then(|p| self.tree.row_of(&p)).unwrap_or(self.sel);
        self.sel = at.min(self.tree.rows.len().saturating_sub(1));
    }

    fn set_open(&mut self, path: String, open: bool) {
        if open {
            self.collapsed.remove(&path);
            self.expanded.insert(path.clone());
        } else {
            self.expanded.remove(&path);
            self.collapsed.insert(path.clone());
        }
        self.relayout(Some(path));
    }

    pub fn draw(&mut self, s: &mut Screen) {
        s.clear();
        let (w, h) = (s.w, s.h);
        if w < 12 || h < 1 {
            return;
        }
        self.horizontal = h <= STRIP_ROWS;
        // A strip of two or three rows has none to spare for a heading.
        let head = (h >= 4) as usize;
        let hint = (h >= HINT_ROWS) as usize;
        self.body = (head, h - head - hint);
        if head == 1 {
            self.header(s);
        }
        if self.horizontal {
            self.draw_strip(s);
        } else {
            self.draw_tree(s);
        }
        if hint == 1 && self.focused {
            self.hint(s);
        }
    }

    /// The project and branch, and how much is waiting: in bold what is not
    /// committed, in regular weight what is only not pushed, as in the rows.
    fn header(&self, s: &mut Screen) {
        let w = s.w;
        let commit = self.files.iter().filter(|f| !f.committed).count();
        let push = self.files.len() - commit;
        // Spelled out where it fits beside the name; "●3 ↑2" where it does not.
        let name_w = term::str_width(&self.repo.name) + term::str_width(&self.repo.branch) + 5;
        let summary = |long: bool| {
            let mut parts = Vec::new();
            if commit + push == 0 {
                let text = if long { "✓ all committed and pushed" } else { "✓ clean" };
                parts.push((text.to_string(), Style::fg(term::GREEN)));
            }
            if commit > 0 {
                let text = if long { format!("{commit} to commit") } else { format!("●{commit}") };
                parts.push((text, Style::PLAIN.bold()));
            }
            if commit > 0 && push > 0 {
                parts.push((if long { " · " } else { " " }.to_string(), DIM));
            }
            if push > 0 {
                let text = if long { format!("{push} to push") } else { format!("↑{push}") };
                parts.push((text, Style::PLAIN));
            }
            let width: usize = parts.iter().map(|(t, _)| term::str_width(t)).sum();
            (parts, width)
        };
        let (mut parts, mut sum_w) = summary(true);
        if name_w + sum_w > w {
            (parts, sum_w) = summary(false);
        }
        if sum_w + 12 > w {
            (parts, sum_w) = (Vec::new(), 0);
        }
        let mut x = w.saturating_sub(sum_w + 1);
        for (text, st) in &parts {
            x += s.put(x, 0, (w - 1).saturating_sub(x), text, *st);
        }
        let room = w.saturating_sub(sum_w + 3);
        let x = 1 + s.put_fit(1, 0, room, &self.repo.name, Style::PLAIN.bold());
        s.put_fit(x + 2, 0, room.saturating_sub(x + 1), &self.repo.branch, Style::fg(term::MAGENTA));
    }

    /// The keys, worded for what each would do right now.
    fn hint(&self, s: &mut Screen) {
        let (w, y) = (s.w, s.h - 1);
        let text = if w >= 60 {
            let e = if self.all_open { "e close folders" } else { "e open all" };
            let c = if self.changed_only { "c all files" } else { "c changed only" };
            format!("enter open · {e} · {c} · q quit")
        } else if w >= 30 {
            "e open all · c changed · q quit".to_string()
        } else {
            "e all · c changed".to_string()
        };
        s.put_fit(1, y, w - 2, &text, DIM);
    }

    fn draw_tree(&mut self, s: &mut Screen) {
        let (w, (y0, h)) = (s.w, self.body);
        let len = self.tree.rows.len();
        if len == 0 {
            let text = if self.changed_only { "Nothing to commit or push." } else { "No files." };
            s.put_fit(1, y0, w - 2, text, DIM);
            return;
        }
        // Keep a little of the tree in view on both sides of the cursor.
        let margin = 2.min(h.saturating_sub(1) / 2);
        if self.sel < self.top + margin {
            self.top = self.sel.saturating_sub(margin);
        } else if self.sel + margin >= self.top + h {
            self.top = self.sel + margin + 1 - h;
        }
        self.top = self.top.min(len.saturating_sub(h));

        let wide = w >= WIDE_COLS;
        let quiet = self.files.is_empty();
        let rows = self.tree.rows.iter().enumerate().skip(self.top).take(h);
        for (y, (i, &(node, depth))) in (y0..).zip(rows) {
            let n = &self.tree.nodes[node as usize];
            let pending = n.kind != Kind::None;
            let open = n.dir && self.is_open(&n.path);
            let sum = self.sums.get(&n.path).copied().unwrap_or_default();
            let colour = ui::kind_style(n.kind);

            // The last column is the scroll thumb's; text stops before it.
            let mut right = w - 1;
            if wide && pending {
                let (add, del) = if n.dir {
                    (sum.add, sum.del)
                } else {
                    self.files.iter().find(|f| f.path == n.path).map_or((0, 0), |f| (f.add, f.del))
                };
                ui::size_right(s, right, y, add, del);
                right -= SIZE_W + WORD_W;
                if !n.dir {
                    s.put(right, y, WORD_W, ui::kind_word(n.kind), colour);
                }
                right -= 1;
            }

            let indent = (depth as usize * 2).min(w / 3);
            for guide in (0..indent).step_by(2) {
                s.put(1 + guide, y, 1, "│", DIM);
            }
            let mut x = 1 + indent;
            let icon = if n.dir { icons::dir(open) } else { icons::file(&n.name) };
            x += ui::icon(s, x, y, right.saturating_sub(x), icon);

            // Bold is work not committed yet; committed-but-unpushed stays
            // regular. Everything untouched recedes once anything is pending.
            let st = if self.uncommitted.contains(&n.path) {
                colour.bold()
            } else if !pending && !quiet {
                colour.faint()
            } else {
                colour
            };
            let room = right.saturating_sub(x);
            if n.dir {
                x += s.put_fit(x, y, room, &n.name, st);
                // A closed folder still says how much is inside it.
                if !open && sum.files > 0 {
                    s.put(x + 1, y, right.saturating_sub(x + 1), &sum.files.to_string(), DIM);
                }
            } else {
                s.put_middle(x, y, room, &n.name, st);
            }
            if self.fresh.contains_key(&n.path) {
                s.put(0, y, 1, "●", colour);
            }
            // Grey rather than a solid bar, so the row keeps its own colour;
            // and no bar at all while the keyboard is in another pane.
            if i == self.sel && self.focused {
                s.tint(0, y, w - 1);
            }
        }
        if len > h {
            let thumb = (h * h / len).max(1);
            let at = self.top * (h - thumb) / (len - h);
            for y in y0 + at..y0 + at + thumb {
                s.put(w - 1, y, 1, "▐", DIM);
            }
        }
    }

    /// Pending files only, most recent first, filling columns top to bottom.
    fn draw_strip(&mut self, s: &mut Screen) {
        let (w, (y0, h)) = (s.w, self.body);
        if self.recent.is_empty() {
            // With a heading, that already says so.
            if y0 == 0 {
                s.put_fit(1, 0, w - 2, "Nothing to commit or push.", DIM);
            }
            return;
        }
        let file = |i: usize| self.recent.get(i).map(|&f| &self.files[f]);
        let name_w = |&f: &usize| {
            let path = &self.files[f].path;
            term::str_width(path.rsplit_once('/').map_or(path, |(_, n)| n))
        };
        let col_w = (self.recent.iter().map(name_w).max().unwrap_or(0) + 12).clamp(16, 34).min(w);
        let shown = (w / col_w).max(1);
        let total = self.recent.len().div_ceil(h);
        let first = self.strip_col.min(total.saturating_sub(shown));
        for col in 0..shown {
            for row in 0..h {
                let Some(f) = file((first + col) * h + row) else { break };
                ui::file_name(s, col * col_w + 1, y0 + row, col_w - 2, f);
            }
        }
        if first > 0 {
            s.put(0, y0, 1, "‹", DIM);
        }
        let after = self.recent.len().saturating_sub((first + shown) * h);
        if after > 0 {
            let more = format!("  +{after} more ›");
            s.put(w.saturating_sub(term::str_width(&more) + 1), y0 + h - 1, w, &more, DIM);
        }
        self.strip_col = first;
    }

    fn move_by(&mut self, delta: isize) {
        if self.horizontal {
            self.strip_col = self.strip_col.saturating_add_signed(delta.signum());
        } else {
            let last = self.tree.rows.len().saturating_sub(1);
            self.sel = self.sel.saturating_add_signed(delta).min(last);
        }
    }

    /// Scrolls the view under the pointer; the cursor only moves to stay in it.
    fn scroll(&mut self, delta: isize) {
        if self.horizontal {
            return self.move_by(delta);
        }
        let (h, len) = (self.body.1.max(1), self.tree.rows.len());
        let last_top = len.saturating_sub(h);
        self.top = self.top.saturating_add_signed(delta).min(last_top);
        let margin = 2.min((h - 1) / 2);
        let low = if self.top == 0 { 0 } else { self.top + margin };
        let high = if self.top == last_top { len.saturating_sub(1) } else { self.top + h - 1 - margin };
        self.sel = self.sel.clamp(low, high.max(low));
    }

    /// The row under the cursor, as (path, is a directory, is open, parent node).
    fn pointed(&self) -> Option<(String, bool, bool, usize)> {
        let n = self.tree.row(self.sel)?;
        Some((n.path.clone(), n.dir, self.is_open(&n.path), n.parent as usize))
    }

    /// Returns true when the program should quit.
    pub fn on_key(&mut self, key: Key) -> bool {
        let page = self.body.1.max(2) as isize - 1;
        match key {
            Key::Char('q') | Key::Ctrl(b'c') | Key::Eof => return true,
            Key::Focus(focused) => self.focused = focused,
            Key::Char('r') => self.reload(),
            Key::Char('e') => {
                // Opening everything also forgets what was closed by hand.
                self.all_open = !self.all_open;
                self.collapsed.clear();
                self.relayout(self.selected_path());
            }
            Key::Char('c') => {
                self.changed_only = !self.changed_only;
                self.relayout(self.selected_path());
            }
            Key::Char('j') | Key::Down => self.move_by(1),
            Key::Char('k') | Key::Up => self.move_by(-1),
            Key::WheelDown => self.scroll(3),
            Key::WheelUp => self.scroll(-3),
            Key::PgDn | Key::Ctrl(b'f') | Key::Ctrl(b'd') => self.move_by(page),
            Key::PgUp | Key::Ctrl(b'b') | Key::Ctrl(b'u') => self.move_by(-page),
            Key::Char('g') | Key::Home => self.move_by(isize::MIN),
            Key::Char('G') | Key::End => self.move_by(isize::MAX),
            Key::Char('l') | Key::Right if self.horizontal => self.move_by(1),
            Key::Char('h') | Key::Left if self.horizontal => self.move_by(-1),
            Key::Click(y) if !self.horizontal && (self.body.0..self.body.0 + self.body.1).contains(&y) => {
                let row = self.top + y - self.body.0;
                if row < self.tree.rows.len() {
                    self.sel = row;
                    if let Some((path, true, open, _)) = self.pointed() {
                        self.set_open(path, !open);
                    }
                }
            }
            Key::Enter | Key::Char(' ') | Key::Char('l') | Key::Right | Key::Char('h') | Key::Left => {
                let Some((path, dir, open, parent)) = self.pointed() else { return false };
                let toggle = matches!(key, Key::Enter | Key::Char(' '));
                let right = matches!(key, Key::Char('l') | Key::Right);
                if dir && (toggle || open != right) {
                    self.set_open(path, !open);
                } else if !toggle && !right && parent != 0 {
                    // Left on a file or a closed directory steps out to its parent.
                    self.sel = self.tree.row_of(&self.tree.nodes[parent].path).unwrap_or(self.sel);
                }
            }
            _ => {}
        }
        false
    }
}
