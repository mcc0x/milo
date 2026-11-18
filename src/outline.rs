//! The outline: a project's files, coloured by what is not yet committed or
//! pushed.
//!
//! A tall pane shows the file tree, with recent history beneath it. A short
//! one shows only the pending files, flowed across its width. A wide one
//! adds what happened to each file and puts history beside the tree.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::git::{Commit, FileChange, Kind, Repo};
use crate::history::{self, Context};
use crate::icons;
use crate::rex;
use crate::term::{self, Key, Screen, Style};
use crate::tree::{Folds, Tree};
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
/// Rows of the tree kept in view on either side of the cursor.
const MARGIN: usize = 2;
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
/// How long something said in place of the key hint stays.
const NOTE: Duration = Duration::from_secs(4);
/// From this height a quarter of the pane shows recent history.
const HISTORY_ROWS: usize = 20;
/// From this width history sits beside the outline instead.
const SIDE_COLS: usize = 90;
/// Commits loaded for it: more than any pane has columns for dots.
const HISTORY_LIMIT: usize = 300;

/// Whether `path` lies inside the directory `dir`.
fn under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// Rows kept between the cursor and the edge of a tree `h` rows tall.
fn margin(h: usize) -> usize {
    MARGIN.min(h.saturating_sub(1) / 2)
}

pub struct Outline {
    repo: Repo,
    /// Pending files; one both committed and edited again appears twice,
    /// the uncommitted state first.
    files: Vec<FileChange>,
    /// Without git, the listing the tree was built from, to notice changes.
    listing: Vec<String>,
    tree: Tree,
    folds: Folds,
    /// Pending files, each once, most recently written first.
    recent: Vec<usize>,
    /// Recent commits, newest first, and what they were read for.
    history: Vec<Commit>,
    history_key: (String, Option<String>),
    show_history: bool,
    /// Files that changed while the outline was open, by path, and when.
    fresh: HashMap<String, Instant>,
    /// Commits that landed while it was open, by hash, and when.
    fresh_commits: HashMap<String, Instant>,
    /// When the reader last used the pane or returned to it.
    looked: Instant,
    /// Whether the pane has the keyboard, once the terminal has said.
    focus: Option<bool>,
    changed_only: bool,
    sel: usize,
    top: usize,
    /// The cursor was put where it is by a click, so the view stays put.
    by_click: bool,
    /// First column shown when the strip has more than fits.
    strip_col: usize,
    horizontal: bool,
    /// The rows between heading and hint: (first row, height).
    body: (usize, usize),
    /// Columns the outline itself has, with history beside it or not.
    width: usize,
    /// The floating window the last file was opened in, to replace it.
    popup: Option<String>,
    /// The path last clicked and when, to tell a double click.
    clicked: Option<(String, Instant)>,
    /// Something to say in place of the key hint, and when it was said.
    note: Option<(&'static str, Instant)>,
}

impl Outline {
    pub fn new(repo: Repo) -> Outline {
        let mut outline = Outline {
            repo,
            files: Vec::new(),
            listing: Vec::new(),
            tree: Tree::default(),
            folds: Folds::default(),
            recent: Vec::new(),
            history: Vec::new(),
            history_key: (String::new(), None),
            show_history: true,
            fresh: HashMap::new(),
            fresh_commits: HashMap::new(),
            looked: Instant::now(),
            focus: None,
            changed_only: false,
            sel: 0,
            top: 0,
            by_click: false,
            strip_col: 0,
            horizontal: false,
            body: (0, 0),
            width: 0,
            popup: None,
            clicked: None,
            note: None,
        };
        outline.load(None, false);
        // Start where the work is, not at the top of the tree.
        let first = outline.tree.rows.iter().position(|&(n, _)| {
            let n = &outline.tree.nodes[n as usize];
            !n.dir && n.kind != Kind::None
        });
        outline.sel = first.unwrap_or(0);
        outline
    }

    /// Whether a watch on the root sees everything that can change here.
    pub fn watchable(&self) -> bool {
        self.repo.self_contained()
    }

    pub fn root(&self) -> &std::path::Path {
        &self.repo.root
    }

    /// Every file to show: git's own list, or a plain walk of the open folders.
    fn list(&self) -> Vec<String> {
        if self.repo.git {
            self.repo.tracked()
        } else {
            walk::list(&self.repo.root, |p| self.folds.is_open(p))
        }
    }

    /// Reads everything afresh. `raw` is a status the repository was just
    /// asked for, if any; `mark` marks commits that were not there before.
    fn load(&mut self, raw: Option<Vec<u8>>, mark: bool) {
        let selected = self.selected_path();
        self.repo.read(raw);
        self.files = self.repo.pending();
        let listed = self.list();
        self.tree = Tree::build(listed.iter().map(String::as_str), &self.files);
        self.listing = if self.repo.git { Vec::new() } else { listed };

        // History only changes when the branch or what is pushed does.
        let key = self.repo.history_key();
        if key != self.history_key {
            let history = self.repo.history(HISTORY_LIMIT);
            // With nothing known before, every commit would count as new.
            if mark && !self.history.is_empty() {
                let known: HashSet<&str> = self.history.iter().map(|c| c.hash.as_str()).collect();
                let now = Instant::now();
                for c in history.iter().filter(|c| !known.contains(c.hash.as_str())) {
                    self.fresh_commits.insert(c.hash.clone(), now);
                }
            }
            (self.history, self.history_key) = (history, key);
        }

        self.recent.clear();
        let mut once = HashSet::new();
        for (i, f) in self.files.iter().enumerate() {
            if once.insert(f.path.as_str()) {
                self.recent.push(i);
            }
            // A folder with pending work opens unless the reader closed it,
            // and stays open after: a commit must not fold the tree away.
            let mut p = f.path.as_str();
            while let Some((dir, _)) = p.rsplit_once('/') {
                self.folds.suggest(dir);
                p = dir;
            }
        }
        let files = &self.files;
        self.recent.sort_by_key(|&i| std::cmp::Reverse(files[i].stamp.map(|(at, _)| at)));
        self.relayout(selected);
    }

    /// Loads afresh, marks what differs from before and, left alone, brings
    /// the latest of it into view.
    fn reload(&mut self, raw: Option<Vec<u8>>) {
        let old = std::mem::take(&mut self.files);
        self.load(raw, true);
        let before: HashMap<(&str, bool), &FileChange> = old.iter().map(|f| ((f.path.as_str(), f.committed), f)).collect();
        let now = Instant::now();
        // The file written most recently among those that changed.
        let mut newest: Option<&FileChange> = None;
        for f in &self.files {
            if before.get(&(f.path.as_str(), f.committed)).is_some_and(|old| *old == f) {
                continue;
            }
            self.fresh.insert(f.path.clone(), now);
            if newest.is_none_or(|n| n.stamp.map(|s| s.0) <= f.stamp.map(|s| s.0)) {
                newest = Some(f);
            }
        }
        if let Some(path) = newest.filter(|_| self.following()).map(|f| f.path.clone()) {
            self.reveal(&path);
        }
    }

    /// Looks for changes made outside the outline, and takes them in.
    pub fn check(&mut self) {
        if self.repo.git {
            if let Some(raw) = self.repo.poll(&self.files) {
                self.reload(Some(raw));
            }
        } else if self.list() != self.listing {
            self.reload(None);
        }
    }

    /// Whether the outline should move by itself to where work is
    /// happening: the keyboard is elsewhere, or has not been used here lately.
    fn following(&self) -> bool {
        self.focus == Some(false) || self.looked.elapsed() >= FOLLOW_AFTER
    }

    /// How much longer a mark made at `at` stays. It fades `FRESH` after it
    /// has been seen: as it appears if the pane is known to have the
    /// keyboard, otherwise from the next time the reader uses it. Unseen, it
    /// gives up after `UNSEEN`.
    fn mark_left(&self, at: Instant) -> Duration {
        let seen = if self.focus == Some(true) {
            Some(at.max(self.looked))
        } else {
            Some(self.looked).filter(|&looked| looked > at)
        };
        let fade = seen.map_or(UNSEEN, |from| FRESH.saturating_sub(from.elapsed()));
        fade.min(UNSEEN.saturating_sub(at.elapsed()))
    }

    /// How long the screen stays right if nothing happens: until a mark
    /// fades or pulses, or a note has been up long enough.
    pub fn next_frame(&self) -> Option<Duration> {
        let marks = self.fresh.values().chain(self.fresh_commits.values());
        let mark = marks.map(|&at| if at.elapsed() < LIVE { PULSE } else { self.mark_left(at) }).min();
        let note = self.note.map(|(_, at)| NOTE.saturating_sub(at.elapsed()));
        [mark, note].into_iter().flatten().min()
    }

    /// Forgets marks and notes that have had their time.
    fn expire(&mut self) {
        let fresh = std::mem::take(&mut self.fresh);
        self.fresh = fresh.into_iter().filter(|(_, at)| !self.mark_left(*at).is_zero()).collect();
        let commits = std::mem::take(&mut self.fresh_commits);
        self.fresh_commits = commits.into_iter().filter(|(_, at)| !self.mark_left(*at).is_zero()).collect();
        self.note = self.note.filter(|(_, at)| at.elapsed() < NOTE);
    }

    fn selected_path(&self) -> Option<String> {
        self.tree.row(self.sel).map(|n| n.path.clone())
    }

    /// Recomputes the visible rows, keeping the cursor on `selected`.
    fn relayout(&mut self, selected: Option<String>) {
        self.tree.layout(|p| self.folds.is_open(p), self.changed_only);
        let at = selected.and_then(|p| self.tree.row_of(&p)).unwrap_or(self.sel);
        self.sel = at.min(self.tree.rows.len().saturating_sub(1));
    }

    /// Applies a change to which folders are open. Without git a folder's
    /// contents are only read once it is open, so that means listing again.
    fn refold(&mut self, selected: Option<String>) {
        if self.repo.git {
            self.relayout(selected);
        } else {
            self.load(None, false);
        }
    }

    /// Opens whatever folders hide `path` and puts the cursor on it.
    fn reveal(&mut self, path: &str) {
        self.folds.open_above(path);
        self.relayout(Some(path.to_string()));
    }

    /// Shows a line in place of the key hint for a few seconds.
    fn say(&mut self, text: &'static str) {
        self.note = Some((text, Instant::now()));
    }

    /// Opens a file in a floating window, in place of the one opened before.
    fn open_file(&mut self, path: &str) {
        match rex::open(&self.repo.root, path, self.popup.as_deref()) {
            Ok(layer) => self.popup = Some(layer),
            Err(why) => {
                self.popup = None;
                self.say(why);
            }
        }
    }

    pub fn draw(&mut self, s: &mut Screen) {
        s.clear();
        self.expire();
        let (w, h) = (s.w, s.h);
        if w < 12 || h < 1 {
            return;
        }
        // The strip lists what is pending, so it needs git to have anything.
        self.horizontal = self.repo.git && h <= STRIP_ROWS;
        // A strip of two or three rows has none to spare for a heading.
        let head = (h >= 4) as usize;
        let hint = (h >= HINT_ROWS) as usize;
        let ctx = Context {
            remote: self.repo.remote.as_deref(),
            dirty: self.files.iter().filter(|f| !f.committed).count(),
            fresh: &self.fresh_commits,
        };
        // Recent history goes beside the outline where the pane is wide, and
        // beneath the tree where it is only tall.
        let shown = self.show_history && !self.history.is_empty();
        let beside = if shown && w >= SIDE_COLS && h >= 3 { (w / 3).clamp(30, 52) } else { 0 };
        let beneath = shown && beside == 0 && !self.horizontal && h >= HISTORY_ROWS;
        let avail = h - head - hint;
        let panel = if beneath { history::rows(&self.history, &ctx, avail, self.tree.rows.len()) } else { 0 };
        // Space, not a rule, keeps the two apart: a column beside, a row
        // beneath. Beside, the heading shares the project's own row.
        if beside > 0 {
            history::draw(s, &self.history, &ctx, (w - beside, 0), (beside, head + avail));
        }
        if panel > 0 {
            history::draw(s, &self.history, &ctx, (0, head + avail - panel + 1), (w, panel - 1));
        }
        self.width = if beside > 0 { w - beside - 1 } else { w };
        self.body = (head, avail - panel);
        if head == 1 {
            self.header(s);
        }
        if self.horizontal {
            self.draw_strip(s);
        } else {
            self.draw_tree(s);
        }
        // A note is said wherever there is a last row, hint or not.
        if let Some((note, _)) = self.note {
            s.put(0, h - 1, w, &" ".repeat(w), Style::PLAIN);
            s.put_fit(1, h - 1, w - 2, note, Style::fg(term::YELLOW));
        } else if hint == 1 && self.focus != Some(false) {
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
        let e = if self.folds.all() { "e close folders" } else { "e open all" };
        // Without git there is no "changed" to narrow down to.
        let text = match (self.repo.git, w) {
            (true, 60..) => {
                let c = if self.changed_only { "c all files" } else { "c changed only" };
                // With work pending, stepping through it is the key worth knowing.
                let first = if self.files.is_empty() { "enter open" } else { "n next change" };
                format!("{first} · {e} · {c} · q quit")
            }
            (true, 30..) if !self.files.is_empty() => "n next · e all · c changed · q quit".to_string(),
            (true, 30..) => "e open all · c changed · q quit".to_string(),
            (true, _) => "e all · c changed".to_string(),
            (false, 30..) => format!("enter open · {e} · q quit"),
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
        // Keep a little of the tree in view on both sides of the cursor,
        // unless a click put it there: then the rows must not move under
        // the pointer before a second click.
        let margin = if self.by_click { 0 } else { margin(h) };
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
            let open = n.dir && self.folds.is_open(&n.path);
            let colour = ui::kind_style(n.kind);

            // The last column is the scroll thumb's; text stops before it.
            let mut right = w - 1;
            if wide && pending {
                ui::size_right(s, right, y, n.add, n.del);
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
            let st = if n.uncommitted {
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
                if !open && n.pending > 0 {
                    s.put(x + 1, y, right.saturating_sub(x + 1), &n.pending.to_string(), DIM);
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
            if i == self.sel && self.focus != Some(false) {
                s.tint(0, y, w - 1);
            }
        }
        ui::thumb(s, w - 1, y0, h, len, self.top);
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
                let Some(&f) = self.recent.get((first + col) * h + row) else { break };
                ui::file_name(s, col * col_w + 1, y0 + row, col_w - 2, &self.files[f]);
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
        let last = self.tree.rows.len().saturating_sub(1);
        self.sel = self.sel.saturating_add_signed(delta).min(last);
    }

    /// Scrolls the view under the pointer; the cursor only moves to stay in it.
    fn scroll(&mut self, delta: isize) {
        let (h, len) = (self.body.1.max(1), self.tree.rows.len());
        let last_top = len.saturating_sub(h);
        self.top = self.top.saturating_add_signed(delta).min(last_top);
        let margin = margin(h);
        let low = if self.top == 0 { 0 } else { self.top + margin };
        let high = if self.top == last_top { len.saturating_sub(1) } else { self.top + h - 1 - margin };
        self.sel = self.sel.clamp(low, high.max(low));
    }

    /// The row under the cursor, as (path, is a directory, is open).
    fn pointed(&self) -> Option<(String, bool, bool)> {
        let n = self.tree.row(self.sel)?;
        Some((n.path.clone(), n.dir, self.folds.is_open(&n.path)))
    }

    fn set_open(&mut self, path: String, open: bool) {
        self.folds.set(&path, open);
        self.refold(Some(path));
    }

    /// Returns true when the program should quit.
    pub fn on_key(&mut self, key: Key) -> bool {
        if let Key::Focus(focused) = key {
            self.focus = Some(focused);
            // Coming back to the pane is the reader looking at it; leaving is not.
            if focused {
                self.looked = Instant::now();
            }
            return false;
        }
        self.looked = Instant::now();
        self.by_click &= matches!(key, Key::Click(..));
        match key {
            Key::Char('q') | Key::Ctrl(b'c') | Key::Eof => return true,
            Key::Char('r') => self.reload(None),
            Key::Char('t') => self.show_history = !self.show_history,
            _ if self.horizontal => self.strip_key(key),
            _ => self.tree_key(key),
        }
        false
    }

    /// The strip has no cursor: its keys only move it sideways.
    fn strip_key(&mut self, key: Key) {
        match key {
            Key::Char('l' | 'j') | Key::Right | Key::Down | Key::WheelDown | Key::PgDn => self.strip_col += 1,
            Key::Char('h' | 'k') | Key::Left | Key::Up | Key::WheelUp | Key::PgUp => {
                self.strip_col = self.strip_col.saturating_sub(1);
            }
            Key::Char('g') | Key::Home => self.strip_col = 0,
            // The last column is wherever drawing finds it.
            Key::Char('G') | Key::End => self.strip_col = usize::MAX / 2,
            _ => {}
        }
    }

    fn tree_key(&mut self, key: Key) {
        let page = self.body.1.max(2) as isize - 1;
        let sel = self.sel;
        match key {
            Key::Char('e') => {
                self.folds.toggle_all();
                self.refold(self.selected_path());
            }
            Key::Char('c') if self.repo.git => {
                self.changed_only = !self.changed_only;
                self.relayout(self.selected_path());
            }
            Key::Char('j') | Key::Down => self.move_by(1),
            Key::Char('k') | Key::Up => self.move_by(-1),
            Key::WheelDown => self.scroll(3),
            Key::WheelUp => self.scroll(-3),
            Key::PgDn | Key::Ctrl(b'f') => self.move_by(page),
            Key::PgUp | Key::Ctrl(b'b') => self.move_by(-page),
            Key::Ctrl(b'd') => self.move_by(page / 2),
            Key::Ctrl(b'u') => self.move_by(-page / 2),
            Key::Char('g') | Key::Home => self.move_by(isize::MIN),
            Key::Char('G') | Key::End => self.move_by(isize::MAX),
            Key::Char('n' | 'N') => {
                let next = self.tree.next_pending(sel, key == Key::Char('n')).map(str::to_string);
                if let Some(path) = next {
                    self.reveal(&path);
                }
            }
            Key::Char('}') => self.sel = self.tree.folder_row(sel, true).unwrap_or(sel),
            Key::Char('{') => self.sel = self.tree.folder_row(sel, false).unwrap_or(sel),
            Key::Char('J') => self.sel = self.tree.sibling_row(sel, true).unwrap_or(sel),
            Key::Char('K') => self.sel = self.tree.sibling_row(sel, false).unwrap_or(sel),
            Key::Char('p') => self.sel = self.tree.parent_row(sel).unwrap_or(sel),
            Key::Click(x, y) if x < self.width && (self.body.0..self.body.0 + self.body.1).contains(&y) => {
                self.click(self.top + y - self.body.0);
            }
            Key::Enter | Key::Char(' ') => match self.pointed() {
                Some((path, true, open)) => self.set_open(path, !open),
                Some((path, false, _)) if key == Key::Enter => self.open_file(&path),
                _ => {}
            },
            Key::Char('l') | Key::Right => {
                if let Some((path, true, false)) = self.pointed() {
                    self.set_open(path, true);
                }
            }
            // Left closes an open folder; anywhere else it steps out to the parent.
            Key::Char('h') | Key::Left => match self.pointed() {
                Some((path, true, true)) => self.set_open(path, false),
                _ => self.sel = self.tree.parent_row(sel).unwrap_or(sel),
            },
            _ => {}
        }
    }

    /// A click selects a row and opens or closes a folder; a second click
    /// on the same file, soon after the first, reads it.
    fn click(&mut self, row: usize) {
        let Some(node) = self.tree.row(row) else { return };
        let (path, dir) = (node.path.clone(), node.dir);
        let again = self.clicked.take().is_some_and(|(p, at)| p == path && at.elapsed() < DOUBLE_CLICK);
        (self.sel, self.by_click) = (row, true);
        match (dir, again) {
            (true, false) => {
                let open = self.folds.is_open(&path);
                self.clicked = Some((path.clone(), Instant::now()));
                self.set_open(path, !open);
            }
            // A double click on a folder is one toggle, not two.
            (true, true) => {}
            (false, false) => self.clicked = Some((path, Instant::now())),
            (false, true) => self.open_file(&path),
        }
    }
}
