//! The project as a strip, coloured by what is not yet committed or pushed.
//!
//! A tall pane shows the file tree. A short one shows only the pending files,
//! flowed across its width. A wide one adds what happened to each file.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::git::{Commit, FileChange, Kind, Repo};
use crate::icons;
use crate::term::{self, Key, Screen, Style};
use crate::tree::Tree;
use crate::ui::{self, DIM};
use crate::walk;
use crate::when;

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
/// How soon after the operating system reports a change it is looked at;
/// also the least time between two such looks while changes keep coming.
const SETTLE: Duration = Duration::from_millis(120);
/// How long a mark stays once it has been seen.
const FRESH: Duration = Duration::from_secs(4);
/// How long a row counts as being written right now, and pulses.
const LIVE: Duration = Duration::from_secs(3);
const PULSE: Duration = Duration::from_millis(500);
/// How long a mark nobody has seen is kept.
const UNSEEN: Duration = Duration::from_secs(300);
/// Without input for this long, the outline follows the work by itself.
const FOLLOW_AFTER: Duration = Duration::from_secs(15);
/// Two clicks on a row within this long are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// From this height a quarter of the pane shows recent history.
const HISTORY_ROWS: usize = 20;
/// From this width history sits beside the outline instead.
const SIDE_COLS: usize = 90;
/// From this many rows the history panel leads with its row of dots.
const DOT_ROWS: usize = 6;
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

/// The key a commit is marked fresh under; the NUL keeps it apart from paths.
fn commit_key(hash: &str) -> String {
    format!("\0{hash}")
}

/// Length of a leading "scope: " in a commit subject: a short single word
/// and the colon and space after it. Zero if there is none.
fn prefix_len(subject: &str) -> usize {
    match subject.split_once(": ") {
        Some((scope, _)) if scope.len() <= 24 && !scope.contains(' ') => scope.len() + 2,
        _ => 0,
    }
}

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
    /// The operating system reported a change that has not been looked at yet.
    poked: bool,
    /// When the reader last used the pane or returned to it.
    looked: Instant,
    /// The terminal has reported focus at least once, so `focused` is real.
    focus_known: bool,
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
    /// Columns the outline itself has, with history beside it or not.
    width: usize,
    /// The floating window the last file was opened in, to replace it.
    popup: Option<String>,
    /// The row last clicked and when, to tell a double click.
    clicked: Option<(usize, Instant)>,
    /// Something to say in place of the key hint, and when it was said.
    note: Option<(String, Instant)>,
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
            focus_known: false,
            looked: Instant::now(),
            poked: false,
            sel: 0,
            top: 0,
            strip_col: 0,
            horizontal: false,
            body: (0, 0),
            width: 0,
            popup: None,
            clicked: None,
            note: None,
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
        let known: HashSet<String> = self.history.iter().map(|c| c.hash.clone()).collect();
        self.load();
        let now = Instant::now();
        // The file written most recently among those that changed.
        let mut newest: Option<(Option<SystemTime>, String)> = None;
        for (key, seen) in self.seen() {
            if before.get(&key) != Some(&seen) {
                if newest.as_ref().is_none_or(|(at, _)| *at <= seen.3) {
                    newest = Some((seen.3, key.0.clone()));
                }
                self.fresh.insert(key.0, now);
            }
        }
        // With nothing known before, every commit would count as new.
        for c in self.history.iter().filter(|c| !known.is_empty() && !known.contains(&c.hash)) {
            self.fresh.insert(commit_key(&c.hash), now);
        }
        // Left alone, the outline keeps the latest work in view.
        if let Some((_, path)) = newest.filter(|_| self.following()) {
            self.reveal(path);
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
        let marks = std::mem::take(&mut self.fresh);
        self.fresh = marks.into_iter().filter(|(_, at)| !self.mark_left(*at).is_zero()).collect();
        self.check_if_due();
    }

    /// Called when the operating system reports a change under the root.
    pub fn changed(&mut self) {
        self.poked = true;
        self.check_if_due();
    }

    /// Looks for changes, at most once per `SETTLE` after being told of one
    /// and once per `CHECK` regardless.
    fn check_if_due(&mut self) {
        let due = if self.poked { SETTLE } else { CHECK };
        if self.checked.elapsed() < due {
            return;
        }
        (self.checked, self.poked) = (Instant::now(), false);
        let stale = if self.repo.git { self.repo.stale(&self.files) } else { self.list() != self.listing };
        if stale {
            self.reload();
        }
    }

    /// Whether the reader is, as far as can be told, looking at the pane
    /// right now: it has the keyboard, and the terminal is known to say so.
    fn watched(&self) -> bool {
        self.focus_known && self.focused
    }

    /// Whether the outline should move by itself to where work is
    /// happening: the keyboard is elsewhere, or has not been used here lately.
    fn following(&self) -> bool {
        (self.focus_known && !self.focused) || self.looked.elapsed() >= FOLLOW_AFTER
    }

    /// How much longer a mark made at `at` stays. It fades `FRESH` after it
    /// has been seen: at once if the pane is being watched, otherwise from
    /// the next time the reader looks. Unseen, it gives up after `UNSEEN`.
    fn mark_left(&self, at: Instant) -> Duration {
        // Watching, a mark is seen as it appears, or on returning to the pane.
        let seen = if self.watched() {
            Some(at.max(self.looked))
        } else {
            Some(self.looked).filter(|&looked| looked > at)
        };
        let fade = seen.map_or(UNSEEN, |from| FRESH.saturating_sub(from.elapsed()));
        fade.min(UNSEEN.saturating_sub(at.elapsed()))
    }

    /// How long to wait for input before the next `tick`.
    pub fn idle_ms(&self) -> i32 {
        let due = if self.poked { SETTLE } else { CHECK };
        let mut wait = due.saturating_sub(self.checked.elapsed());
        for &at in self.fresh.values() {
            wait = wait.min(self.mark_left(at));
            // A mark still pulsing needs a redraw at each beat.
            if at.elapsed() < LIVE {
                wait = wait.min(PULSE);
            }
        }
        wait.as_millis() as i32 + 1
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
        // Recent history goes beside the outline where the pane is wide, and
        // beneath the tree where it is only tall.
        let history = self.show_history && !self.history.is_empty();
        let beside = if history && w >= SIDE_COLS && h >= 3 { (w / 3).clamp(30, 52) } else { 0 };
        let beneath = history && beside == 0 && !self.horizontal && h >= HISTORY_ROWS;
        let panel = if beneath { self.history_rows(h - head - hint) } else { 0 };
        // The column left over between the two is the space that parts them.
        self.width = if beside > 0 { w - beside - 1 } else { w };
        self.body = (head, h - head - hint - panel);
        if head == 1 {
            self.header(s);
        }
        if self.horizontal {
            self.draw_strip(s);
        } else {
            self.draw_tree(s);
        }
        // Space, not a rule, keeps the two apart: two columns beside, one
        // row beneath. Beside, the heading shares the project's own row.
        if beside > 0 {
            self.draw_history(s, (self.width + 1, 0), (beside, head + self.body.1));
        }
        if panel > 0 {
            self.draw_history(s, (0, self.body.0 + self.body.1 + 1), (w, panel - 1));
        }
        if hint == 1 && self.focused {
            self.hint(s);
        }
    }

    /// The project and branch, and how much is waiting: in bold what is not
    /// committed, in regular weight what is only not pushed, as in the rows.
    fn header(&self, s: &mut Screen) {
        let w = self.width;
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
        if let Some((note, _)) = self.note.as_ref().filter(|(_, at)| at.elapsed() < FRESH) {
            s.put_fit(1, y, w - 2, note, Style::fg(term::YELLOW));
            return;
        }
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
        let (w, (y0, h)) = (self.width, self.body);
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
            // A row that changed: pulsing while it is being written, then a
            // quieter mark until it has been seen. A closed folder carries
            // the mark of whatever changed inside it.
            let changed = if n.dir && !open {
                self.fresh.iter().filter(|(p, _)| under(p, &n.path)).map(|(_, at)| *at).max()
            } else {
                self.fresh.get(&n.path).copied()
            };
            if let Some(age) = changed.map(|at| at.elapsed()) {
                let live = age < LIVE;
                let glyph = match (live, age.as_millis() / PULSE.as_millis() % 2) {
                    (true, 0) => "●",
                    (true, _) => "○",
                    (false, _) => "•",
                };
                s.put(0, y, 1, glyph, if live { colour.bold() } else { colour });
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

    /// Rows the history panel wants out of `avail`: no more than it has to
    /// say, about a quarter of the pane, more where the tree leaves room.
    fn history_rows(&self, avail: usize) -> usize {
        let unpushed = self.history.iter().filter(|c| !c.pushed).count();
        let remote_row = (unpushed > 0 && unpushed < self.history.len()) as usize;
        let dirty = self.files.iter().any(|f| !f.committed) as usize;
        // A blank row above, the heading, the dots, then one row per line.
        let content = 3 + dirty + remote_row + self.history.len();
        let spare = avail.saturating_sub(self.tree.rows.len()).min(avail / 2);
        content.min((avail / 4).max(DOT_ROWS + 1).max(spare)).min(avail.saturating_sub(3))
    }

    fn dot_style(&self, i: usize, c: &Commit) -> Style {
        let st = match (&self.repo.remote, c.pushed) {
            (None, _) => Style::PLAIN,
            (Some(_), true) => Style::fg(term::GREEN),
            (Some(_), false) => Style::fg(term::YELLOW),
        };
        // The branch tip is the same dot, bold.
        if i == 0 { st.bold() } else { st }
    }

    /// Whether history passes, between two neighbours, from local commits
    /// to ones the remote has.
    fn crosses(&self, before: &Commit, c: &Commit) -> bool {
        self.repo.remote.is_some() && !before.pushed && c.pushed
    }

    /// The shape of recent history in one row: newest first, a gap between
    /// days, a tick where the remote is.
    fn draw_dots(&self, s: &mut Screen, (left, right): (usize, usize), y: usize, dirty: bool) {
        // Spaced out while every commit fits that way, packed once they do not.
        let pairs = || self.history.iter().zip(self.history.iter().skip(1));
        let days = pairs().filter(|(a, b)| a.day != b.day).count();
        let ticks = pairs().filter(|(a, b)| self.crosses(a, b)).count();
        let cells = self.history.len() + ticks + dirty as usize;
        let step = if cells * 2 + days <= right.saturating_sub(left) { 2 } else { 1 };
        let mut x = left;
        if dirty {
            s.put(x, y, 1, "○", Style::PLAIN.bold());
            x += step;
        }
        for (i, c) in self.history.iter().enumerate() {
            if let Some(before) = i.checked_sub(1).map(|b| &self.history[b]) {
                if self.crosses(before, c) && x < right {
                    s.put(x, y, 1, "│", DIM);
                    x += step;
                }
                x += (before.day != c.day) as usize;
            }
            if x >= right {
                break;
            }
            s.put(x, y, 1, "●", self.dot_style(i, c));
            x += step;
        }
    }

    /// Recent commits, in the area whose corner is (x0, y0): beneath the
    /// tree, or beside it in a wide pane. A heading in the project name's
    /// own weight, a row of dots for the shape where there is room for it,
    /// then one commit a row. Hollow is work not committed, yellow is
    /// committed but not pushed, green is on the remote. Without a remote,
    /// commits are simply uncoloured.
    fn draw_history(&self, s: &mut Screen, (x0, y0): (usize, usize), (w, rows): (usize, usize)) {
        // Text keeps one column clear of both edges of the area.
        let (left, right, end) = (x0 + 1, x0 + w - 1, y0 + rows);
        let unpushed = self.history.iter().filter(|c| !c.pushed).count();
        let dirty = self.files.iter().filter(|f| !f.committed).count();

        s.put(left, y0, w - 2, "History", Style::PLAIN.bold());
        // The project heading counts files to push; this counts commits, and says so.
        if unpushed > 0 {
            let text = format!("{unpushed} unpushed");
            s.put(right.saturating_sub(text.len()), y0, text.len(), &text, Style::fg(term::YELLOW));
        }
        // In a short pane every row goes to commits, one beneath the other.
        let dots = rows >= DOT_ROWS;
        if dots {
            self.draw_dots(s, (left, right), y0 + 1, dirty > 0);
        }

        let text_x = left + 2;
        let mut y = y0 + 1 + dots as usize;
        if dirty > 0 && y < end {
            let text = format!("{dirty} file{} not committed", if dirty == 1 { "" } else { "s" });
            s.put(left, y, 1, "○", Style::PLAIN.bold());
            s.put_fit(text_x, y, right.saturating_sub(text_x), &text, Style::PLAIN.bold());
            y += 1;
        }
        let now = when::now();
        let mut last_age = String::new();
        for (i, c) in self.history.iter().enumerate() {
            if i > 0 && self.crosses(&self.history[i - 1], c) && y < end {
                // Everything below this name is already on the remote.
                let name = self.repo.remote.as_deref().unwrap_or_default();
                s.put_fit(text_x, y, right.saturating_sub(text_x), name, DIM);
                y += 1;
            }
            if y >= end {
                break;
            }
            s.put(left, y, 1, "●", self.dot_style(i, c));
            // An age is only worth its column where it differs from the row above.
            let age = when::short_age(now - c.time);
            let mut text_end = right;
            if age != last_age {
                text_end -= age.len();
                s.put(text_end, y, age.len(), &age, DIM);
                text_end -= 1;
            }
            last_age = age;

            let room = text_end.saturating_sub(text_x);
            let fresh = self.fresh.contains_key(&commit_key(&c.hash));
            // Merges and what is already pushed recede; a commit that just
            // landed stands out for a moment.
            let quiet = c.merge || (c.pushed && self.repo.remote.is_some());
            let st = match (fresh, quiet) {
                (true, _) => Style::PLAIN.bold(),
                (false, true) => Style::PLAIN.faint(),
                (false, false) => Style::PLAIN,
            };
            // A "scope: " prefix steps back so the eye lands on what follows.
            let split = if fresh || quiet { 0 } else { prefix_len(&c.subject) };
            let used = s.put(text_x, y, room, &c.subject[..split], Style::PLAIN.faint());
            s.put_fit(text_x + used, y, room.saturating_sub(used), &c.subject[split..], st);
            y += 1;
        }
    }

    /// Pending files only, most recent first, filling columns top to bottom.
    fn draw_strip(&mut self, s: &mut Screen) {
        let (w, (y0, h)) = (self.width, self.body);
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
        self.reveal(path);
    }

    /// Opens whatever folders hide `path` and puts the cursor on it.
    fn reveal(&mut self, path: String) {
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
        // Any key, click or return to the pane is the reader looking at it.
        self.looked = Instant::now();
        match key {
            Key::Char('q') | Key::Ctrl(b'c') | Key::Eof => return true,
            Key::Focus(focused) => {
                (self.focused, self.focus_known) = (focused, true);
                if !focused {
                    return false;
                }
            }
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
            Key::Click(x, y)
                if !self.horizontal && x < self.width && (self.body.0..self.body.0 + self.body.1).contains(&y) =>
            {
                let row = self.top + y - self.body.0;
                if row < self.tree.rows.len() {
                    // A second click on the same row, soon after the first, reads a file.
                    let again = self.clicked.is_some_and(|(r, at)| r == row && at.elapsed() < DOUBLE_CLICK);
                    self.clicked = Some((row, Instant::now()));
                    self.sel = row;
                    match self.pointed() {
                        Some((path, true, open, _)) => self.set_open(path, !open),
                        Some((path, false, ..)) if again => self.open_file(&path),
                        _ => {}
                    }
                }
            }
            Key::Enter | Key::Char(' ') | Key::Char('l') | Key::Right | Key::Char('h') | Key::Left => {
                let Some((path, dir, open, parent)) = self.pointed() else { return false };
                let toggle = matches!(key, Key::Enter | Key::Char(' '));
                let right = matches!(key, Key::Char('l') | Key::Right);
                if dir && (toggle || open != right) {
                    self.set_open(path, !open);
                } else if !dir && key == Key::Enter {
                    self.open_file(&path);
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

/// Whether `path` lies inside the directory `dir`.
fn under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

impl Outline {
    /// Opens a file in a floating window over the current tab, in place of
    /// the one opened before it. Rex makes the window, so this only works
    /// when running inside Rex.
    fn open_file(&mut self, path: &str) {
        let Ok(session) = std::env::var("REX_SESSION") else {
            return self.say("Reading a file needs Rex.");
        };
        let Ok(exe) = std::env::current_exe() else { return };
        if let Some(layer) = self.popup.take() {
            // It may be closed already; either way it is gone afterwards.
            let request = format!(r#"{{"session_id":{},"layer_id":{}}}"#, json(&session), json(&layer));
            rex(&["api", "call", "-s", &session, "session.close_layer", &request]);
        }
        // Over the tab the outline is in, wherever the reader is looking;
        // failing that, over the active one.
        let window = std::env::var("REX_BLOCK")
            .ok()
            .and_then(|block| rex(&["block", "inspect", "-s", &session, &block, "--json"]))
            .and_then(|reply| field(&reply, "window_id"))
            .map(|id| format!(r#""window_id":{},"#, json(&id)))
            .unwrap_or_default();
        let name = path.rsplit_once('/').map_or(path, |(_, n)| n);
        let request = format!(
            concat!(
                r#"{{"session_id":{},{}"bounds":[0.12,0.08,0.76,0.84],"focus":true,"layout":{{"block":{{"#,
                r#""flavor":"com.superlogical.terminal.shell","label":{},"#,
                r#""options":{{"command":[{},"view",{}],"shell":"none","cwd":{}}}}}}}}}"#,
            ),
            json(&session),
            window,
            json(name),
            json(&exe.to_string_lossy()),
            json(&self.repo.root.join(path).to_string_lossy()),
            json(&self.repo.root.to_string_lossy()),
        );
        match rex(&["api", "call", "-s", &session, "session.new_layer", &request]) {
            Some(reply) => self.popup = field(&reply, "layer_id"),
            None => self.say("Rex did not open the file."),
        }
    }

    /// Shows a line in place of the key hint for a few seconds.
    fn say(&mut self, text: &str) {
        self.note = Some((text.to_string(), Instant::now()));
    }
}

/// Runs the Rex command line and returns what it printed. `None` if it failed.
fn rex(args: &[&str]) -> Option<String> {
    let out = Command::new("rex").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A string as a JSON literal.
fn json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => out.extend(['\\', c]),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The value of a string field, wherever it first appears in a JSON reply.
fn field(reply: &str, name: &str) -> Option<String> {
    let (_, rest) = reply.split_once(&format!("\"{name}\""))?;
    let value = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    Some(value.split_once('"')?.0.to_string())
}
