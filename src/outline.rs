//! The project as a strip, coloured by what is not yet committed or pushed.
//!
//! A tall pane shows the file tree. A short one shows only the pending files,
//! flowed across its width. A wide one adds what happened to each file.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime};

use crate::git::{Commit, FileChange, Kind, Repo};
use crate::icons;
use crate::term::{self, Key, Screen, Style};
use crate::tree::Tree;
use crate::ui::{self, DIM};
use crate::walk;

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
/// From this height a quarter of the pane shows recent history.
const HISTORY_ROWS: usize = 20;
/// Commits loaded for it: more than any pane has columns for dots.
const HISTORY_LIMIT: usize = 300;

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
    /// Without git, the listing the tree was built from, to notice changes.
    listing: Vec<String>,
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
    /// Recent commits, newest first; empty outside git.
    history: Vec<Commit>,
    show_history: bool,
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
    pub fn new(repo: Repo) -> Outline {
        let mut outline = Outline {
            repo,
            files: Vec::new(),
            listing: Vec::new(),
            tree: Tree::default(),
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            sums: HashMap::new(),
            uncommitted: HashSet::new(),
            recent: Vec::new(),
            history: Vec::new(),
            show_history: true,
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

    /// Every file to show: git's own list, or a plain walk of the open folders.
    fn list(&self) -> Vec<String> {
        if self.repo.git {
            self.repo.files()
        } else {
            walk::list(&self.repo.root, |p| self.is_open(p))
        }
    }

    fn load(&mut self) {
        let selected = self.selected_path();
        self.files = self.repo.load();
        self.checked = Instant::now();
        let all = self.list();
        let pending = self.files.iter().map(|f| f.path.as_str());
        self.tree = Tree::build(all.iter().map(String::as_str).chain(pending));
        self.listing = if self.repo.git { Vec::new() } else { all };
        self.history = self.repo.history(HISTORY_LIMIT);
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
            let stale = if self.repo.git { self.repo.stale(&self.files) } else { self.list() != self.listing };
            if stale {
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

    /// Applies a change to which folders are open. Without git a folder's
    /// contents are only read once it is open, so that means listing again.
    fn refresh_open(&mut self, selected: Option<String>) {
        if self.repo.git {
            self.relayout(selected);
        } else {
            self.load();
        }
    }

    fn set_open(&mut self, path: String, open: bool) {
        if open {
            self.collapsed.remove(&path);
            self.expanded.insert(path.clone());
        } else {
            self.expanded.remove(&path);
            self.collapsed.insert(path.clone());
        }
        self.refresh_open(Some(path));
    }

    pub fn draw(&mut self, s: &mut Screen) {
        s.clear();
        let (w, h) = (s.w, s.h);
        if w < 12 || h < 1 {
            return;
        }
        // The strip lists what is pending, so it needs git to have anything.
        self.horizontal = self.repo.git && h <= STRIP_ROWS;
        // A strip of two or three rows has none to spare for a heading.
        let head = (h >= 4) as usize;
        let hint = (h >= HINT_ROWS) as usize;
        // A quarter of a tall pane goes to recent history, if there is any.
        let history = !self.horizontal && self.show_history && h >= HISTORY_ROWS && !self.history.is_empty();
        let panel = if history { (h / 4).max(5) } else { 0 };
        self.body = (head, h - head - hint - panel);
        if head == 1 {
            self.header(s);
        }
        if self.horizontal {
            self.draw_strip(s);
        } else {
            self.draw_tree(s);
        }
        if panel > 0 {
            self.draw_history(s, self.body.0 + self.body.1, panel);
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
        let (mut parts, mut sum_w) = if self.repo.git { summary(true) } else { (Vec::new(), 0) };
        if self.repo.git && name_w + sum_w > w {
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
        // Without git there is no "changed" to narrow down to.
        let text = match (self.repo.git, w) {
            (true, 60..) => {
                let e = if self.all_open { "e close folders" } else { "e open all" };
                let c = if self.changed_only { "c all files" } else { "c changed only" };
                // With work pending, stepping through it is the key worth knowing.
                let first = if self.files.is_empty() { "enter open" } else { "n next change" };
                format!("{first} · {e} · {c} · q quit")
            }
            (true, 30..) if !self.files.is_empty() => "n next · e all · c changed · q quit".to_string(),
            (true, 30..) => "e open all · c changed · q quit".to_string(),
            (true, _) => "e all · c changed".to_string(),
            (false, 30..) => {
                let e = if self.all_open { "e close folders" } else { "e open all" };
                format!("enter open · {e} · q quit")
            }
            (false, _) => "e all · q quit".to_string(),
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

    /// Recent commits beneath the tree: a row of dots for the shape of it,
    /// then the newest few spelled out. A hollow dot is work not committed,
    /// yellow is committed but not pushed, green is safe on the remote, and
    /// the ringed dot is where the branch is now. With no remote to push
    /// to, commits are simply uncoloured.
    fn draw_history(&self, s: &mut Screen, y0: usize, rows: usize) {
        let w = s.w;
        s.put(0, y0, w, &"─".repeat(w), DIM);
        s.put(2, y0, w.saturating_sub(3), " History ", Style::PLAIN.bold());
        let dot = |i: usize, c: &Commit| {
            let st = match (self.repo.remote, c.pushed) {
                (false, _) => Style::PLAIN,
                (true, true) => Style::fg(term::GREEN),
                (true, false) => Style::fg(term::YELLOW),
            };
            (if i == 0 { "◉" } else { "●" }, st)
        };
        let dirty = !self.uncommitted.is_empty();
        // Spaced out while every commit fits that way, packed once they do not.
        let room = w.saturating_sub(2);
        let step = if (self.history.len() + dirty as usize) * 2 <= room { 2 } else { 1 };
        let (mut x, y) = (1, y0 + 1);
        if dirty {
            s.put(x, y, 1, "○", Style::PLAIN.bold());
            x += step;
        }
        for (i, c) in self.history.iter().enumerate().take((w - 1).saturating_sub(x) / step) {
            let (glyph, st) = dot(i, c);
            s.put(x, y, 1, glyph, st);
            x += step;
        }

        let mut y = y0 + 2;
        if dirty && y < y0 + rows {
            s.put(1, y, 1, "○", Style::PLAIN.bold());
            s.put_fit(3, y, w.saturating_sub(4), "not committed yet", Style::PLAIN.bold());
            y += 1;
        }
        for (i, c) in self.history.iter().enumerate().take((y0 + rows).saturating_sub(y)) {
            let (glyph, st) = dot(i, c);
            s.put(1, y, 1, glyph, st);
            let age_x = (w - 1).saturating_sub(c.age.len());
            s.put(age_x, y, c.age.len(), &c.age, DIM);
            // What is already pushed recedes, like untouched files above.
            let text = if c.pushed && self.repo.remote { Style::PLAIN.faint() } else { Style::PLAIN };
            s.put_fit(3, y, age_x.saturating_sub(4), &c.subject, text);
            y += 1;
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

    /// Jumps to the next or previous pending file in tree order, wrapping
    /// around, and opens whatever folders were hiding it.
    fn jump_pending(&mut self, forward: bool) {
        let from = self.tree.rows.get(self.sel).map(|&(n, _)| n);
        let Some(target) = self.tree.next_pending(from, forward) else { return };
        let path = self.tree.nodes[target as usize].path.clone();
        let mut p = path.as_str();
        while let Some((dir, _)) = p.rsplit_once('/') {
            self.collapsed.remove(dir);
            self.expanded.insert(dir.to_string());
            p = dir;
        }
        self.relayout(Some(path));
    }

    /// Jumps to the next or previous folder, over the files between.
    fn jump_folder(&mut self, forward: bool) {
        let rows = &self.tree.rows;
        let is_dir = |&(n, _): &(u32, u16)| self.tree.nodes[n as usize].dir;
        let hit = if forward {
            rows.iter().skip(self.sel + 1).position(is_dir).map(|i| self.sel + 1 + i)
        } else {
            rows.iter().take(self.sel).rposition(is_dir)
        };
        self.sel = hit.unwrap_or(self.sel);
    }

    /// Jumps to the next or previous row at the same depth in the same
    /// folder, over anything nested in between.
    fn jump_sibling(&mut self, forward: bool) {
        let rows = &self.tree.rows;
        let Some(&(_, depth)) = rows.get(self.sel) else { return };
        // The folder's rows end at the first one shallower than this.
        let hit = if forward {
            let ahead = rows[self.sel + 1..].iter().take_while(|r| r.1 >= depth);
            ahead.enumerate().find(|(_, r)| r.1 == depth).map(|(i, _)| self.sel + 1 + i)
        } else {
            let behind = rows[..self.sel].iter().rev().take_while(|r| r.1 >= depth);
            behind.enumerate().find(|(_, r)| r.1 == depth).map(|(i, _)| self.sel - 1 - i)
        };
        self.sel = hit.unwrap_or(self.sel);
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
            Key::Char('t') => self.show_history = !self.show_history,
            Key::Char('e') => {
                // Opening everything also forgets what was closed by hand.
                self.all_open = !self.all_open;
                self.collapsed.clear();
                self.refresh_open(self.selected_path());
            }
            Key::Char('c') if self.repo.git => {
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
            Key::Char('n') if !self.horizontal => self.jump_pending(true),
            Key::Char('N') if !self.horizontal => self.jump_pending(false),
            Key::Char('}') if !self.horizontal => self.jump_folder(true),
            Key::Char('{') if !self.horizontal => self.jump_folder(false),
            Key::Char('J') if !self.horizontal => self.jump_sibling(true),
            Key::Char('K') if !self.horizontal => self.jump_sibling(false),
            Key::Char('p') if !self.horizontal => {
                if let Some((.., parent)) = self.pointed().filter(|p| p.3 != 0) {
                    self.sel = self.tree.row_of(&self.tree.nodes[parent].path).unwrap_or(self.sel);
                }
            }
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
