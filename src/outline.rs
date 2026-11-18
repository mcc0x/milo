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
use crate::keys::Key;
use crate::screen::{self, Screen, Style, Wash};
use crate::tree::{Folds, Tree};
use crate::ui::{self, DIM};
use crate::view::View;
use crate::watch::Scope;
use crate::width::str_width;
use crate::{help, icons, rex, walk};

/// At or below this height the pane is a horizontal strip.
const STRIP_ROWS: usize = 7;
/// From this width each file also says what happened to it.
const WIDE_COLS: usize = 52;
/// From this height the last row lists the keys.
const HINT_ROWS: usize = 12;
/// Rows of the tree kept in view on either side of the cursor.
const MARGIN: usize = 2;
/// How long a mark stays once it has been seen.
const FRESH: Duration = Duration::from_secs(4);
/// How long a row counts as being written right now, and pulses.
const LIVE: Duration = Duration::from_secs(3);
const PULSE: Duration = Duration::from_millis(500);
/// How long a mark nobody has seen is kept.
const UNSEEN: Duration = Duration::from_secs(300);
/// Where the terminal does not say which pane has the keyboard, the
/// outline follows the work by itself after this long without input.
const FOLLOW_UNTOLD: Duration = Duration::from_secs(120);
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
    inside_rex: bool,
    /// The floating window the last file was opened in, to replace it.
    popup: Option<String>,
    /// The path last clicked and when, to tell a double click.
    clicked: Option<(String, Instant)>,
    /// Something to say in place of the key hint, and when it was said.
    note: Option<(String, Instant)>,
    /// The page of explanations, when it is up, and how far it is scrolled.
    help: Option<usize>,
    /// A file being read here, in place of the outline, outside Rex.
    reading: Option<View>,
}

impl Outline {
    pub fn new(repo: Repo, inside_rex: bool) -> Outline {
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
            inside_rex,
            popup: None,
            clicked: None,
            note: None,
            help: None,
            reading: None,
        };
        let _ = outline.load(None, None, false);
        // Start where the work is, not at the top of the tree.
        let first = outline.tree.rows.iter().position(|r| {
            let n = outline.tree.node(r.node);
            !n.dir && n.kind != Kind::None
        });
        outline.sel = first.unwrap_or(0);
        outline
    }

    /// What to watch for changes to the outline.
    pub fn watch(&self) -> Scope {
        self.repo.watch()
    }

    /// Without git, every file in the folders that are open.
    fn walk(&self) -> Vec<String> {
        walk::list(&self.repo.root, |p| self.folds.is_open(p))
    }

    /// Reads everything afresh. `raw` is a status the repository was just
    /// asked for and `listing` a walk just made, if any; `mark` marks commits
    /// that were not there before. Returns the replaced list so the caller
    /// can compare it to what is pending, if git answered.
    fn load(&mut self, raw: Option<Vec<u8>>, listing: Option<Vec<String>>, mark: bool) -> Option<Vec<FileChange>> {
        let selected = self.selected_path();
        self.repo.read(raw);
        let old = self.repo.pending().map(|files| std::mem::replace(&mut self.files, files));
        if self.repo.git {
            self.tree.rebuild(self.repo.tracked(), &self.files);
        } else {
            self.listing = listing.unwrap_or_else(|| self.walk());
            self.tree.rebuild(self.listing.iter().map(String::as_str), &self.files);
        }

        // History only changes when the branch or what is pushed does.
        let key = self.repo.history_key();
        if key != self.history_key
            && let Some(history) = self.repo.history(HISTORY_LIMIT)
        {
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
        old
    }

    /// Loads afresh, marks what differs from before and, left alone, brings
    /// the latest of it into view.
    fn reload(&mut self, raw: Option<Vec<u8>>, listing: Option<Vec<String>>) {
        let Some(old) = self.load(raw, listing, true) else { return };
        let before: HashMap<(&str, bool), &FileChange> =
            old.iter().map(|f| ((f.path.as_str(), f.committed), f)).collect();
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
            self.follow(&path);
        }
    }

    /// Looks for changes made outside the outline, and takes them in.
    /// Returns false if it is too soon to tell, and worth looking again.
    pub fn check(&mut self) -> bool {
        // A file being read here is kept up to date, and so is what it covers.
        if let Some(view) = &mut self.reading {
            view.check();
        }
        if self.repo.busy() {
            // Git is part-way through changing many files at once.
            return false;
        }
        if self.repo.git {
            if let Some(raw) = self.repo.poll(&self.files) {
                self.reload(Some(raw), None);
            }
        } else {
            let listing = self.walk();
            if listing != self.listing {
                self.reload(None, Some(listing));
            }
        }
        true
    }

    /// Whether the outline should move by itself to where work is
    /// happening: only while the keyboard is in another pane, so that it
    /// never moves under someone reading it. A terminal that does not say
    /// where the keyboard is leaves that to a long silence.
    fn following(&self) -> bool {
        match self.focus {
            Some(focused) => !focused,
            None => self.looked.elapsed() >= FOLLOW_UNTOLD,
        }
    }

    /// How much longer a mark made at `at` stays. It fades `FRESH` after it
    /// has been seen: as it appears if the pane is known to have the
    /// keyboard, otherwise from the next time the reader uses it. Unseen, it
    /// gives up after `UNSEEN`.
    fn mark_left(at: Instant, looked: Instant, focus: Option<bool>) -> Duration {
        let seen = if focus == Some(true) { Some(at.max(looked)) } else { Some(looked).filter(|&looked| looked > at) };
        let fade = seen.map_or(UNSEEN, |from| FRESH.saturating_sub(from.elapsed()));
        fade.min(UNSEEN.saturating_sub(at.elapsed()))
    }

    /// How long the screen stays right if nothing happens: until a mark
    /// fades or pulses, or a note has been up long enough.
    pub fn next_frame(&self) -> Option<Duration> {
        if self.reading.is_some() || self.help.is_some() {
            return None;
        }
        let marks = self.fresh.values().chain(self.fresh_commits.values());
        let mark = marks
            .map(|&at| if at.elapsed() < LIVE { PULSE } else { Self::mark_left(at, self.looked, self.focus) })
            .min();
        let note = self.note.as_ref().map(|(_, at)| NOTE.saturating_sub(at.elapsed()));
        [mark, note].into_iter().flatten().min()
    }

    /// Forgets marks and notes that have had their time.
    fn expire(&mut self) {
        let (looked, focus) = (self.looked, self.focus);
        let keep = |at| !Self::mark_left(at, looked, focus).is_zero();
        self.fresh.retain(|_, at| keep(*at));
        self.fresh_commits.retain(|_, at| keep(*at));
        self.note = self.note.take().filter(|(_, at)| at.elapsed() < NOTE);
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
            let _ = self.load(None, None, false);
        }
    }

    /// Opens whatever folders hide `path` and puts the cursor on it.
    fn reveal(&mut self, path: &str) {
        self.folds.open_above(path);
        self.relayout(Some(path.to_string()));
    }

    /// Brings the file being worked on into view and says so, since the
    /// view moving by itself would otherwise be a puzzle. A folder the
    /// reader closed stays closed: the cursor goes to it instead.
    fn follow(&mut self, path: &str) {
        let mut shown = path;
        while self.tree.row_of(shown).is_none() {
            match shown.rsplit_once('/') {
                Some((dir, _)) => shown = dir,
                None => return,
            }
        }
        self.sel = self.tree.row_of(shown).unwrap_or(self.sel);
        self.by_click = false;
        let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
        self.say(format!("following {name}"));
    }

    /// Shows a line in place of the key hint for a few seconds.
    fn say(&mut self, text: impl Into<String>) {
        self.note = Some((text.into(), Instant::now()));
    }

    /// Opens a file to read: in a floating window inside Rex, in place of
    /// the one opened before, and otherwise here in this pane.
    fn open_file(&mut self, path: &str) {
        if !self.inside_rex {
            self.reading = View::open(&self.repo.root.join(path));
            return;
        }
        match rex::open(&self.repo.root, path, self.popup.as_deref()) {
            Ok(layer) => self.popup = Some(layer),
            Err(why) => {
                self.popup = None;
                self.say(why);
            }
        }
    }

    pub fn draw(&mut self, s: &mut Screen) {
        if let Some(view) = &mut self.reading {
            return view.draw(s);
        }
        if let Some(top) = &mut self.help {
            return help::draw(s, &help::outline(self.repo.git), top);
        }
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
            history::draw(s, &self.history, &ctx, (w - beside, 0), (beside, head + avail), true);
        }
        if panel > 0 {
            history::draw(s, &self.history, &ctx, (0, head + avail - panel + 1), (w, panel - 1), false);
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
        if let Some((note, _)) = &self.note {
            s.put(0, h - 1, w, &" ".repeat(w), Style::PLAIN);
            s.put_fit(1, h - 1, w - 2, note, Style::fg(screen::YELLOW));
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
        // Spelled out where it fits beside the name; "○3 ↑2" where it does not.
        let name_w = str_width(&self.repo.name) + str_width(&self.repo.branch) + 5;
        let summary = |long: bool| {
            let mut parts = Vec::new();
            if !self.repo.ok {
                // Nothing below can be trusted, so nothing is claimed.
                let text = if long { "git did not answer" } else { "! git" };
                return (vec![(text.to_string(), ui::DEL)], str_width(text));
            }
            if commit + push == 0 {
                // Nothing can be called pushed where there is nowhere to push.
                let text = match (long, self.repo.remote.is_some()) {
                    (true, true) => "✓ all committed and pushed",
                    (true, false) => "✓ all committed",
                    (false, _) => "✓ clean",
                };
                parts.push((text.to_string(), Style::fg(screen::GREEN)));
                if long && self.repo.remote.is_none() {
                    parts.push((" · no remote".to_string(), DIM));
                }
            }
            if commit > 0 {
                let text = if long { format!("{commit} to commit") } else { format!("○{commit}") };
                parts.push((text, Style::PLAIN.bold()));
            }
            if commit > 0 && push > 0 {
                parts.push((if long { " · " } else { " " }.to_string(), DIM));
            }
            if push > 0 {
                let text = if long { format!("{push} to push") } else { format!("↑{push}") };
                parts.push((text, Style::PLAIN));
            }
            let width: usize = parts.iter().map(|(t, _)| str_width(t)).sum();
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
        s.put_fit(x + 2, 0, room.saturating_sub(x + 1), &self.repo.branch, Style::fg(screen::MAGENTA));
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
                format!("{first} · {e} · {c} · ? help")
            }
            // What `c` would do now also says whether it is on.
            (true, 30..) if !self.files.is_empty() => {
                format!("n next · {} · ? help", if self.changed_only { "c all files" } else { "c changed" })
            }
            (true, 30..) => "enter open · e all · ? help".to_string(),
            (false, 44..) => format!("enter open · {e} · ? help"),
            (false, 30..) => "enter open · e all · ? help".to_string(),
            (_, _) => "? help · q quit".to_string(),
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

        // What happened to each file, and by how much, in two columns no
        // wider than what there is to say.
        let wide = w >= WIDE_COLS;
        let (mut word_w, mut size_w) = (0, 0);
        for row in self.tree.rows.iter().filter(|_| wide) {
            let n = self.tree.node(row.node);
            if !n.dir {
                word_w = word_w.max(ui::kind_word(n.kind).len());
            }
            size_w = size_w.max(ui::size_width(n.add, n.del));
        }
        // A gap between the two, where there are two.
        size_w += (size_w > 0) as usize;
        let quiet = self.files.is_empty();
        let rows = self.tree.rows.iter().enumerate().skip(self.top).take(h);
        for (y, (i, row)) in (y0..).zip(rows) {
            let n = self.tree.node(row.node);
            let pending = n.kind != Kind::None;
            let open = n.dir && self.folds.is_open(&n.path);
            let colour = ui::kind_style(n.kind);

            // The last column is the scroll thumb's; text stops before it.
            let mut right = w - 1;
            if wide && pending {
                ui::size_right(s, right, y, n.add, n.del);
                right -= size_w + word_w;
                if !n.dir {
                    s.put(right, y, word_w, ui::kind_word(n.kind), ui::kind_colour(n.kind));
                }
                right -= 1;
            }

            let indent = (usize::from(row.depth) * 2).min(w / 3);
            for guide in (0..indent).step_by(2) {
                s.put(1 + guide, y, 1, "│", DIM);
            }
            let mut x = 1 + indent;
            let name = n.name();
            let icon = if n.dir { icons::dir(open) } else { icons::file(name) };
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
                x += s.put_fit(x, y, room, name, st);
                // A closed folder still says how much is inside it.
                if !open && n.pending > 0 {
                    s.put(x + 1, y, right.saturating_sub(x + 1), &n.pending.to_string(), DIM);
                }
            } else {
                s.put_middle(x, y, room, name, st);
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
            // A shade of the background rather than a solid bar, so the row
            // keeps its own colour; and no bar at all while the keyboard is
            // in another pane.
            if i == self.sel && self.focus != Some(false) {
                s.wash(0, y, w - 1, Wash::Bar);
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
        // Each column is as wide as its own names need, so one long name
        // does not cost every column its room.
        let total = self.recent.len().div_ceil(h);
        let first = self.strip_col.min(total - 1);
        let (mut x, mut col) = (0, first);
        while col < total {
            let files = &self.recent[col * h..((col + 1) * h).min(self.recent.len())];
            let name_w = |&f: &usize| {
                let path = &self.files[f].path;
                str_width(path.rsplit_once('/').map_or(path, |(_, n)| n))
            };
            let col_w = (files.iter().map(name_w).max().unwrap_or(0) + 12).clamp(16, 34).min(w);
            // A column is drawn whole or not at all, bar the first.
            if x > 0 && x + col_w > w {
                break;
            }
            for (row, &f) in files.iter().enumerate() {
                ui::file_name(s, x + 1, y0 + row, col_w - 2, &self.files[f]);
            }
            (x, col) = (x + col_w, col + 1);
        }
        if first > 0 {
            s.put(0, y0, 1, "‹", DIM);
        }
        let after = self.recent.len().saturating_sub(col * h);
        if after > 0 {
            let more = format!("  +{after} more ›");
            s.put(w.saturating_sub(str_width(&more) + 1), y0 + h - 1, w, &more, DIM);
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
        // A file read here takes the keys; closing it comes back to the outline.
        if let Some(view) = &mut self.reading {
            match key {
                Key::Ctrl(b'c') | Key::Eof => return true,
                _ if view.on_key(key) => self.reading = None,
                _ => {}
            }
            self.looked = Instant::now();
            return false;
        }
        self.looked = Instant::now();
        if let Some(top) = self.help {
            self.help = match key {
                Key::Ctrl(b'c') | Key::Eof => return true,
                Key::Char('j') | Key::Down | Key::WheelDown => Some(top + 1),
                Key::Char('k') | Key::Up | Key::WheelUp => Some(top.saturating_sub(1)),
                _ => None,
            };
            return false;
        }
        if key == Key::Char('?') {
            self.help = Some(0);
            return false;
        }
        self.by_click &= matches!(key, Key::Click(..));
        match key {
            Key::Char('q') | Key::Ctrl(b'c') | Key::Eof => return true,
            Key::Char('r') => self.reload(None, None),
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
        let page = isize::try_from(self.body.1.max(2) - 1).unwrap_or(isize::MAX);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::fs;

    fn project() -> Fixture {
        let f = Fixture::new();
        f.write("src/main.rs", "fn main() {}\n");
        f.write("src/deep/util.rs", "fn util() {}\n");
        f.write("README.md", "# hello\n");
        f.commit("first");
        f.write("src/main.rs", "fn main() {\n    run();\n}\n");
        f.write("notes.txt", "one\ntwo\n");
        f
    }

    fn open(f: &Fixture) -> Outline {
        Outline::new(Repo::open(&f.root).unwrap(), false)
    }

    fn draw(o: &mut Outline, w: usize, h: usize) -> Screen {
        let mut screen = Screen::sized(w, h);
        o.draw(&mut screen);
        screen
    }

    fn file_row<'a>(rows: &'a [String], name: &str) -> &'a str {
        rows.iter().find(|row| row.split_whitespace().any(|word| word == name)).unwrap().as_str()
    }

    #[test]
    fn the_header_distinguishes_work_to_commit_from_work_to_push() {
        let f = Fixture::new();
        f.write("page.txt", "before\n");
        f.commit("first");
        let mut o = open(&f);
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("✓ all committed · no remote"));

        f.push();
        assert!(o.check());
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("✓ all committed and pushed"));

        f.write("page.txt", "after\n");
        assert!(o.check());
        assert!(o.tree.row(o.tree.row_of("page.txt").unwrap()).unwrap().uncommitted);
        f.commit("second");
        f.write("note.txt", "not committed\n");
        assert!(o.check());
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("1 to commit · 1 to push"));
        assert!(draw(&mut o, 29, 10).text()[0].ends_with("○1 ↑1"));
        let pending: Vec<_> = o.files.iter().map(|f| (f.path.as_str(), f.committed)).collect();
        assert_eq!(pending, [("note.txt", false), ("page.txt", true)]);
        assert!(o.tree.row(o.tree.row_of("note.txt").unwrap()).unwrap().uncommitted);
        assert!(!o.tree.row(o.tree.row_of("page.txt").unwrap()).unwrap().uncommitted);

        f.push();
        assert!(o.check());
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("1 to commit"));
        let pending: Vec<_> = o.files.iter().map(|f| (f.path.as_str(), f.committed)).collect();
        assert_eq!(pending, [("note.txt", false)]);
        assert_eq!(o.tree.row(o.tree.row_of("page.txt").unwrap()).unwrap().kind, Kind::None);
    }

    #[test]
    fn file_rows_name_the_change_and_the_reader_agrees_on_its_size() {
        let f = Fixture::new();
        f.write("edit.txt", "before\nsame\n");
        f.write("gone.txt", "remove me\n");
        f.write("old.txt", "keep this text\n");
        f.commit("first");
        f.write("edit.txt", "after\nextra\nsame\n");
        f.write("staged.txt", "stage me\n");
        f.write("loose.txt", "not yet tracked\n");
        f.git(&["add", "staged.txt"]);
        f.git(&["mv", "old.txt", "moved.txt"]);
        fs::remove_file(f.root.join("gone.txt")).unwrap();

        let mut o = open(&f);
        let rows = draw(&mut o, 65, 12).text();
        for (name, label) in [
            ("edit.txt", "edited +2 −1"),
            ("staged.txt", "new +1"),
            ("gone.txt", "removed −1"),
            ("moved.txt", "moved"),
            ("loose.txt", "new +1"),
        ] {
            let row = file_row(&rows, name);
            let tail = row.split_once(name).unwrap().1.split_whitespace().collect::<Vec<_>>().join(" ");
            assert_eq!(tail, label, "{row}");
        }
        assert!(!rows.iter().any(|row| row.contains("old.txt")), "{rows:?}");

        let mut view = View::open(&f.root.join("edit.txt")).unwrap();
        let mut screen = Screen::sized(65, 12);
        view.draw(&mut screen);
        let counts = |row: &str| row.split_whitespace().rev().take(2).collect::<Vec<_>>().join(" ");
        assert_eq!(counts(file_row(&rows, "edit.txt")), counts(&screen.text()[0]));
    }

    #[test]
    fn a_folder_opens_for_pending_work_but_stays_closed_when_the_user_closes_it() {
        let f = project();
        let mut o = open(&f);
        let rows = draw(&mut o, 40, 12).text();
        assert!(rows.iter().any(|row| row == " ▾ src"));
        assert!(rows.iter().any(|row| row.contains("main.rs")));

        o.on_key(Key::Char('p'));
        o.on_key(Key::Enter);
        assert!(draw(&mut o, 40, 12).text().iter().any(|row| row.trim_start() == "▸ src 1"));
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        assert!(o.check());
        let rows = draw(&mut o, 40, 12).text();
        assert!(rows.iter().any(|row| row.contains("▸ src 2")), "{rows:?}");
        assert!(!rows.iter().any(|row| row.contains("main.rs") || row.contains("util.rs")), "{rows:?}");
    }

    #[test]
    fn opening_every_folder_twice_restores_the_previous_folds() {
        let f = project();
        let mut o = open(&f);
        let before = draw(&mut o, 40, 12).text();
        assert!(before.iter().any(|row| row.contains("▸ deep")));
        o.on_key(Key::Char('e'));
        let all = draw(&mut o, 40, 12).text();
        assert!(all.iter().any(|row| row.contains("▾ deep")));
        assert!(all.iter().any(|row| row.contains("util.rs")));
        o.on_key(Key::Char('e'));
        assert_eq!(draw(&mut o, 40, 12).text(), before);
    }

    #[test]
    fn changed_only_hides_untouched_files_and_the_hint_says_how_to_show_them_again() {
        let f = project();
        let mut o = open(&f);
        let all = draw(&mut o, 65, 12).text();
        assert!(all.iter().any(|row| row.contains("README.md")));
        assert!(all[11].contains("c changed only"));

        o.on_key(Key::Char('c'));
        let changed = draw(&mut o, 65, 12).text();
        assert!(!changed.iter().any(|row| row.contains("README.md")));
        assert!(changed.iter().any(|row| row.contains("main.rs")));
        assert!(changed.iter().any(|row| row.contains("notes.txt")));
        assert!(changed[11].contains("c all files"));
        o.on_key(Key::Char('c'));
        assert_eq!(draw(&mut o, 65, 12).text(), all);
    }

    #[test]
    fn the_outline_follows_writes_only_when_the_keyboard_is_elsewhere() {
        let f = project();
        let mut o = open(&f);
        o.on_key(Key::Focus(true));
        let before = o.selected_path().unwrap();
        assert_ne!(before, "src/deep/util.rs");
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        assert!(o.check());
        assert_eq!(o.selected_path().as_deref(), Some(before.as_str()));
        let focused = draw(&mut o, 65, 12).text();
        assert!(focused.iter().any(|row| row.contains("util.rs")));
        assert!(!focused[11].contains("following"));

        o.on_key(Key::Focus(false));
        f.write("src/deep/util.rs", "fn util() { even_more(); }\n");
        assert!(o.check());
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));
        assert_eq!(draw(&mut o, 65, 12).text()[11].trim_start(), "following util.rs");
    }

    #[test]
    fn at_seven_rows_the_tree_becomes_a_strip_of_pending_files() {
        let f = project();
        let mut o = open(&f);
        let tree = draw(&mut o, 40, 8).text();
        assert!(tree.iter().any(|row| row.contains("▾ src")));
        assert!(tree.iter().any(|row| row.contains("README.md")));
        let strip = draw(&mut o, 40, 7).text();
        assert!(strip.iter().any(|row| row.contains("main.rs src")));
        assert!(strip.iter().any(|row| row.contains("notes.txt")));
        assert!(!strip.iter().any(|row| row.contains("▾ src") || row.contains("README.md")));
    }

    #[test]
    fn a_twentieth_row_makes_room_for_history_beneath_the_tree() {
        let f = project();
        let mut o = open(&f);
        assert!(!draw(&mut o, 65, 19).text().iter().any(|row| row.contains("History")));
        let tall = draw(&mut o, 65, 20).text();
        let history = tall.iter().position(|row| row.trim_start() == "History").unwrap();
        assert!(tall[..history].iter().any(|row| row.contains("main.rs")));
        assert!(tall[history + 1..].iter().any(|row| row.contains("first")));
    }

    #[test]
    fn a_ninetieth_column_puts_history_beside_the_tree() {
        let f = project();
        let mut o = open(&f);
        assert!(!draw(&mut o, 89, 11).text().iter().any(|row| row.contains("History")));
        let wide = draw(&mut o, 90, 11).text();
        assert!(wide[0].starts_with(" project") && wide[0].ends_with("History"), "{wide:?}");
        assert!(wide.iter().any(|row| row.contains("first")));
    }

    #[test]
    fn a_wide_tree_has_columns_for_the_kind_and_size_of_a_change() {
        let f = project();
        let mut o = open(&f);
        let narrow = draw(&mut o, 51, 10).text();
        let wide = draw(&mut o, 52, 10).text();
        assert!(file_row(&narrow, "main.rs").ends_with("main.rs"));
        let row = file_row(&wide, "main.rs");
        assert!(row.contains("edited") && row.ends_with("+3 −1"), "{row}");
    }

    #[test]
    fn next_and_previous_pending_follow_tree_order_open_folders_and_wrap() {
        let f = project();
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        let mut o = open(&f);
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));
        o.on_key(Key::Char('p'));
        o.on_key(Key::Enter);
        assert!(!draw(&mut o, 40, 12).text().iter().any(|row| row.contains("util.rs")));
        o.on_key(Key::Char('n'));
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));
        assert!(draw(&mut o, 40, 12).text().iter().any(|row| row.contains("util.rs")));
        for (key, paths) in [
            ('n', ["src/main.rs", "notes.txt", "src/deep/util.rs"]),
            ('N', ["notes.txt", "src/main.rs", "src/deep/util.rs"]),
        ] {
            for path in paths {
                o.on_key(Key::Char(key));
                assert_eq!(o.selected_path().as_deref(), Some(path));
            }
        }
    }

    #[test]
    fn reading_in_the_outline_pane_returns_to_the_same_cursor_on_quit() {
        let f = project();
        let mut o = open(&f);
        o.on_key(Key::Char('n'));
        let before = draw(&mut o, 65, 12);
        let rows = before.text();
        let cursor = rows.iter().position(|row| row.contains("notes.txt")).unwrap();
        assert!(before.bar_at(cursor));
        assert!(!o.on_key(Key::Enter));
        let reader = draw(&mut o, 65, 12).text();
        assert!(reader[0].contains("notes.txt"));
        assert_eq!(reader[1], " 1 + one");
        assert!(!o.on_key(Key::Char('q')));
        let back = draw(&mut o, 65, 12);
        assert_eq!(back.text(), rows);
        assert!(back.bar_at(cursor));
    }

    #[test]
    fn question_mark_shows_help_and_escape_restores_the_outline() {
        let f = project();
        let mut o = open(&f);
        let before = draw(&mut o, 65, 12).text();
        assert!(!o.on_key(Key::Char('?')));
        let help = draw(&mut o, 65, 12).text();
        assert!(help[0].trim_start().starts_with("Files"));
        assert!(help[11].contains("any key closes this"));
        assert!(!o.on_key(Key::Esc));
        assert_eq!(draw(&mut o, 65, 12).text(), before);
    }

    #[test]
    fn a_plain_folder_has_no_git_counts_and_picks_up_new_files_on_check() {
        let f = Fixture::folder();
        f.write("sub/a.txt", "a\n");
        f.write("b.txt", "b\n");
        let mut o = open(&f);
        assert_eq!(draw(&mut o, 40, 8).text(), [" project", " ▸ sub", " • b.txt", "", "", "", "", ""]);
        o.on_key(Key::Enter);
        assert!(draw(&mut o, 40, 8).text().iter().any(|row| row.contains("a.txt")));
        f.write("sub/new.txt", "new\n");
        assert!(o.check());
        let rows = draw(&mut o, 40, 8).text();
        assert_eq!(rows[0], " project");
        assert!(rows.iter().any(|row| row.contains("new.txt")), "{rows:?}");
    }
}
