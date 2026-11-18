//! Application state and input handling.
//!
//! The three panes form one path: pick a moment in the timeline, see the
//! files it touched, read the change. Enter goes deeper, Esc comes back.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::git::{self, Detail, LineKind, Row, Status};
use crate::term::{self, Key};
use crate::tree::Tree;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Timeline,
    Files,
    Change,
}

pub const PANES: [Pane; 3] = [Pane::Timeline, Pane::Files, Pane::Change];

#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    fn contains(&self, x: usize, y: usize) -> bool {
        (self.x..self.x + self.w).contains(&x) && (self.y..self.y + self.h).contains(&y)
    }
}

/// A search being typed; `origin` is where the cursor was when it began.
pub struct Search {
    pub text: String,
    origin: usize,
}

#[derive(Clone, Copy)]
enum Divider {
    /// Between the left column and the change.
    Column,
    /// Between the timeline and the files.
    Row,
}

pub struct App {
    pub root: PathBuf,
    pub name: String,
    status_raw: Vec<u8>,
    pub status: Status,

    pub rows: Vec<Row>,
    log_limit: usize,
    log_done: bool,
    pub all: bool,
    pub sel: usize,
    pub top: usize,
    /// Shows git's own vocabulary: graph, hashes, refs, staged markers.
    pub detailed: bool,
    /// Restricts history and changes to one file or directory.
    pub filter: Option<String>,

    pub detail: Detail,
    pub file_sel: usize,
    pub file_top: usize,
    pub change_top: usize,
    pub change_x: usize,

    /// Shows the whole project as a tree instead of only the changed files.
    pub tree_mode: bool,
    pub tree: Tree,
    tree_stale: bool,
    all_files: Option<Vec<String>>,
    /// Directories the user opened.
    pub expanded: HashSet<String>,
    /// Directories opened because the selected change touches them.
    pub touched: HashSet<String>,
    pub tree_sel: usize,
    pub tree_top: usize,

    pub focus: Pane,
    /// The selection moved and its change is not loaded yet.
    pub detail_dirty: bool,
    /// A commit on screen has no size yet. Set by drawing.
    pub need_stats: bool,
    pub msg: String,
    pub help: bool,
    pub search: Option<Search>,
    last_search: String,

    /// User-dragged divider positions, in screen cells.
    pub split_x: Option<usize>,
    pub split_y: Option<usize>,
    drag: Option<Divider>,
    /// Where the dividers were last drawn; (0, 0) when there are none.
    pub dividers: (usize, usize),
    /// Screen areas of the three panes, set by the last draw.
    pub areas: [Rect; 3],
}

impl App {
    pub fn new(root: PathBuf) -> App {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut app = App {
            root,
            name,
            status_raw: Vec::new(),
            status: Status::default(),
            rows: Vec::new(),
            log_limit: 1000,
            log_done: false,
            all: false,
            sel: 0,
            top: 0,
            detailed: false,
            filter: None,
            detail: Detail::default(),
            file_sel: 0,
            file_top: 0,
            change_top: 0,
            change_x: 0,
            tree_mode: false,
            tree: Tree::default(),
            tree_stale: true,
            all_files: None,
            expanded: HashSet::new(),
            touched: HashSet::new(),
            tree_sel: 0,
            tree_top: 0,
            focus: Pane::Timeline,
            detail_dirty: false,
            need_stats: false,
            msg: String::new(),
            help: false,
            search: None,
            last_search: String::new(),
            split_x: None,
            split_y: None,
            drag: None,
            dividers: (0, 0),
            areas: [Rect::default(); 3],
        };
        app.reload();
        app
    }

    fn reload(&mut self) {
        self.status_raw = git::status_raw(&self.root);
        self.reload_from_status();
    }

    fn reload_from_status(&mut self) {
        self.status = git::parse_status(&self.status_raw);
        self.all_files = None;
        self.tree = Tree::default();
        self.load_log();
        self.load_detail(true);
    }

    /// Picks up changes made outside the viewer. Called when idle.
    pub fn tick(&mut self) {
        let raw = git::status_raw(&self.root);
        if raw != self.status_raw {
            self.status_raw = raw;
            self.reload_from_status();
        } else if matches!(self.rows.get(self.sel), Some(Row::Uncommitted)) {
            // Further edits to an already-modified file leave status unchanged.
            let detail = self.fetch_detail();
            if detail != self.detail {
                self.set_detail(detail, true);
            }
        }
    }

    fn selected_hash(&self) -> Option<&str> {
        match self.rows.get(self.sel) {
            Some(Row::Commit(c)) => Some(&c.hash),
            _ => None,
        }
    }

    fn load_log(&mut self) {
        let keep = self.selected_hash().map(str::to_string);
        let filter = self.filter.as_deref();
        let (mut rows, done) = git::log(&self.root, self.log_limit, self.all, self.detailed, filter);
        if self.status.files.iter().any(|f| filter.is_none_or(|p| git::under(&f.path, p))) {
            rows.insert(0, Row::Uncommitted);
        }
        let at = |hash: &str| rows.iter().position(|r| matches!(r, Row::Commit(c) if c.hash == hash));
        self.sel = keep.and_then(|h| at(&h)).or_else(|| rows.iter().position(Row::selectable)).unwrap_or(0);
        self.rows = rows;
        self.log_done = done;
    }

    fn fetch_detail(&self) -> Detail {
        let filter = self.filter.as_deref();
        match self.rows.get(self.sel) {
            Some(Row::Uncommitted) => git::worktree_detail(&self.root, &self.status, filter, self.detailed),
            Some(Row::Commit(c)) => git::commit_detail(&self.root, &c.hash, filter, self.detailed),
            _ => Detail::default(),
        }
    }

    /// Loads the files and change for the selected moment. `keep` holds the
    /// reading position, for a reload of the same moment.
    pub fn load_detail(&mut self, keep: bool) {
        let detail = self.fetch_detail();
        self.set_detail(detail, keep);
    }

    fn set_detail(&mut self, detail: Detail, keep: bool) {
        let file = self.detail.files.get(self.file_sel).map(|f| f.path.clone()).filter(|_| keep);
        self.detail = detail;
        self.detail_dirty = false;
        if keep {
            let at = file.and_then(|p| self.detail.files.iter().position(|f| f.path == p));
            self.file_sel = at.unwrap_or(self.file_sel).min(self.detail.files.len().saturating_sub(1));
            self.change_top = self.change_top.min(self.detail.lines.len().saturating_sub(1));
        } else {
            (self.file_sel, self.file_top, self.change_top, self.change_x) = (0, 0, 0, 0);
        }
        self.tree_stale = true;
        if self.tree_mode {
            self.sync_tree();
            if !keep {
                // A new moment: start on the first file it touched.
                let first = self.detail.files.first().and_then(|f| self.tree.row_of(&f.path));
                self.tree_sel = first.unwrap_or(self.tree_sel);
            }
        }
    }

    /// Fills in change sizes for the commits on and just below the screen.
    pub fn load_stats(&mut self) {
        self.need_stats = false;
        let end = (self.top + 2 * self.areas[0].h.max(1)).min(self.rows.len());
        let window = &mut self.rows[self.top.min(end)..end];
        let hashes: Vec<String> = window
            .iter()
            .filter_map(|r| match r {
                Row::Commit(c) if c.stat.is_none() => Some(c.hash.clone()),
                _ => None,
            })
            .collect();
        if hashes.is_empty() {
            return;
        }
        let refs: Vec<&str> = hashes.iter().map(String::as_str).collect();
        let stats = git::stats(&self.root, &refs, self.filter.as_deref());
        for row in window {
            if let Row::Commit(c) = row {
                // A commit git did not answer for gets an empty size, not a retry.
                c.stat.get_or_insert_with(|| {
                    stats.iter().find(|(h, _)| *h == c.hash).map(|(_, s)| *s).unwrap_or_default()
                });
            }
        }
    }

    /// Brings the project tree in line with the current change.
    fn sync_tree(&mut self) {
        let files = self.all_files.get_or_insert_with(|| git::files(&self.root));
        let selected = self.tree.row(self.tree_sel).map(|n| n.path.clone());
        let missing = self.detail.files.iter().any(|f| !self.tree.contains(&f.path));
        if missing || self.tree.ghosts || self.tree.nodes.is_empty() {
            let changed = self.detail.files.iter().map(|f| f.path.as_str());
            self.tree = Tree::build(files.iter().map(String::as_str).chain(changed));
            self.tree.ghosts = missing;
        }
        self.tree.colour(self.detail.files.iter().map(|f| (f.path.as_str(), f.kind)));
        self.touched.clear();
        for f in &self.detail.files {
            let mut p = f.path.as_str();
            while let Some((dir, _)) = p.rsplit_once('/') {
                if !self.touched.insert(dir.to_string()) {
                    break;
                }
                p = dir;
            }
        }
        self.tree_stale = false;
        self.relayout(selected);
    }

    /// Recomputes the visible tree rows, keeping the cursor on `selected`.
    fn relayout(&mut self, selected: Option<String>) {
        let (expanded, touched) = (&self.expanded, &self.touched);
        self.tree.layout(|p| expanded.contains(p) || touched.contains(p));
        let at = selected.and_then(|p| self.tree.row_of(&p)).unwrap_or(self.tree_sel);
        self.tree_sel = at.min(self.tree.rows.len().saturating_sub(1));
    }

    pub fn is_open(&self, path: &str) -> bool {
        self.expanded.contains(path) || self.touched.contains(path)
    }

    fn toggle_dir(&mut self, path: String) {
        if self.is_open(&path) {
            self.expanded.remove(&path);
            self.touched.remove(&path);
        } else {
            self.expanded.insert(path.clone());
        }
        self.relayout(Some(path));
    }

    /// The file whose section the change view is currently showing.
    pub fn current_file(&self) -> Option<usize> {
        let end = (self.change_top + 1).min(self.detail.lines.len());
        self.detail.lines[..end].iter().rev().find_map(|l| match l.kind {
            LineKind::File(i) => Some(i as usize),
            _ => None,
        })
    }

    /// Selects a changed file and brings its section into view.
    fn select_file(&mut self, i: usize) {
        let Some(file) = self.detail.files.get(i) else { return };
        self.file_sel = i;
        match file.line {
            Some(line) => self.change_top = line,
            None => self.msg = "This file's change is too large to show.".into(),
        }
    }

    /// The path the cursor is on in the files pane.
    fn pointed_path(&self) -> Option<String> {
        if self.tree_mode && self.focus == Pane::Files {
            self.tree.row(self.tree_sel).map(|n| n.path.clone())
        } else {
            self.detail.files.get(self.file_sel).map(|f| f.path.clone())
        }
    }

    fn set_filter(&mut self, filter: Option<String>) {
        self.filter = filter;
        self.top = 0;
        self.load_log();
        self.detail_dirty = true;
    }

    fn set_moment(&mut self, i: usize) {
        if i != self.sel {
            self.sel = i;
            self.detail_dirty = true;
        }
        if !self.log_done && i + 5 >= self.rows.len() {
            self.log_limit *= 4;
            self.load_log();
        }
    }

    /// Moves the timeline cursor by `delta` moments, stepping over headings.
    fn move_moment(&mut self, delta: isize) {
        let n = self.rows.len() as isize;
        let (step, mut left) = (delta.signum(), delta.unsigned_abs().min(n as usize));
        let (mut i, mut best) = (self.sel as isize, self.sel);
        while left > 0 {
            i += step;
            if i < 0 || i >= n {
                break;
            }
            if self.rows[i as usize].selectable() {
                best = i as usize;
                left -= 1;
            }
        }
        self.set_moment(best);
    }

    fn scroll_change(&mut self, delta: isize) {
        let last = self.detail.lines.len().saturating_sub(1);
        self.change_top = self.change_top.saturating_add_signed(delta).min(last);
        if let Some(i) = self.current_file() {
            self.file_sel = i;
        }
    }

    /// Rows in a pane, for cursor arithmetic.
    fn count(&self, pane: Pane) -> usize {
        match pane {
            Pane::Timeline => self.rows.len(),
            Pane::Files if self.tree_mode => self.tree.rows.len(),
            Pane::Files => self.detail.files.len(),
            Pane::Change => self.detail.lines.len(),
        }
    }

    fn cursor(&self) -> usize {
        match self.focus {
            Pane::Timeline => self.sel,
            Pane::Files if self.tree_mode => self.tree_sel,
            Pane::Files => self.file_sel,
            Pane::Change => self.change_top,
        }
    }

    fn set_cursor(&mut self, i: usize) {
        match self.focus {
            Pane::Timeline if self.rows.get(i).is_some_and(Row::selectable) => self.set_moment(i),
            Pane::Timeline => {}
            Pane::Files if self.tree_mode => self.tree_sel = i,
            Pane::Files => self.select_file(i),
            Pane::Change => self.scroll_change(i as isize - self.change_top as isize),
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        match self.focus {
            Pane::Timeline => self.move_moment(delta),
            Pane::Change => self.scroll_change(delta),
            Pane::Files => {
                let last = self.count(Pane::Files).saturating_sub(1);
                self.set_cursor(self.cursor().saturating_add_signed(delta).min(last));
            }
        }
    }

    /// Whether row `i` of the focused pane contains `needle` (lower case).
    fn matches(&self, i: usize, needle: &str) -> bool {
        let has = |s: &str| s.to_lowercase().contains(needle);
        match self.focus {
            Pane::Timeline => match &self.rows[i] {
                Row::Commit(c) => has(&c.subject) || has(&c.author) || has(&c.refs) || c.hash.starts_with(needle),
                Row::Uncommitted => has("not committed yet"),
                _ => false,
            },
            Pane::Files if self.tree_mode => self.tree.row(i).is_some_and(|n| has(&n.path)),
            Pane::Files => has(&self.detail.files[i].path),
            Pane::Change => has(&self.detail.lines[i].text),
        }
    }

    /// Moves to the nearest match at or after `from`, wrapping around.
    fn find(&mut self, needle: &str, from: usize, forward: bool) {
        let n = self.count(self.focus);
        let needle = needle.to_lowercase();
        if n == 0 || needle.is_empty() {
            return;
        }
        let hit = (0..n).map(|k| if forward { (from + k) % n } else { (from + n - k) % n }).find(|&i| self.matches(i, &needle));
        match hit {
            Some(i) => self.set_cursor(i),
            None => self.msg = format!("Nothing matches “{needle}”."),
        }
    }

    fn search_key(&mut self, key: Key) {
        let Some(mut search) = self.search.take() else { return };
        match key {
            Key::Enter => {
                self.last_search = search.text;
                return;
            }
            Key::Esc | Key::Ctrl(b'c') => return self.set_cursor(search.origin),
            Key::Backspace if search.text.is_empty() => return,
            Key::Backspace => {
                search.text.pop();
            }
            Key::Char(c) => search.text.push(c),
            _ => {}
        }
        self.msg.clear();
        self.find(&search.text, search.origin, true);
        self.search = Some(search);
    }

    fn forward(&mut self) {
        match self.focus {
            Pane::Timeline => self.focus = Pane::Files,
            Pane::Change => {}
            Pane::Files if !self.tree_mode => {
                self.select_file(self.file_sel);
                self.focus = Pane::Change;
            }
            Pane::Files => {
                let Some(node) = self.tree.row(self.tree_sel) else { return };
                let (path, dir) = (node.path.clone(), node.dir);
                if dir {
                    return self.toggle_dir(path);
                }
                match self.detail.files.iter().position(|f| f.path == path) {
                    Some(i) => {
                        self.select_file(i);
                        self.focus = Pane::Change;
                    }
                    None => self.msg = "Not changed here. Press f to see this file's own history.".into(),
                }
            }
        }
    }

    fn back(&mut self) {
        match self.focus {
            Pane::Change => self.focus = Pane::Files,
            Pane::Files => self.focus = Pane::Timeline,
            Pane::Timeline if self.filter.is_some() => self.set_filter(None),
            Pane::Timeline => {}
        }
    }

    /// Returns true when the application should quit.
    pub fn on_key(&mut self, key: Key) -> bool {
        if matches!(key, Key::Eof | Key::Ctrl(b'c')) && self.search.is_none() {
            return true;
        }
        match key {
            Key::Click(x, y) => self.click(x, y),
            Key::Drag(x, y) => self.drag_to(x, y),
            Key::Release => self.drag = None,
            Key::WheelUp(x, y) => self.wheel(x, y, -3),
            Key::WheelDown(x, y) => self.wheel(x, y, 3),
            _ if self.help => self.help = false,
            _ if self.search.is_some() => self.search_key(key),
            _ => {
                self.msg.clear();
                return self.command(key);
            }
        }
        false
    }

    fn command(&mut self, key: Key) -> bool {
        if let Some(delta) = self.motion(key) {
            self.move_cursor(delta);
            return false;
        }
        let at = PANES.iter().position(|&p| p == self.focus).unwrap_or(0);
        match key {
            Key::Char('q') => return true,
            Key::Char('?') => self.help = true,
            Key::Tab => self.focus = PANES[(at + 1) % 3],
            Key::BackTab => self.focus = PANES[(at + 2) % 3],
            Key::Char(c @ '1'..='3') => self.focus = PANES[c as usize - '1' as usize],
            Key::Enter => self.forward(),
            Key::Esc => self.back(),
            Key::Char('/') => self.search = Some(Search { text: String::new(), origin: self.cursor() }),
            Key::Char('n') | Key::Char('N') => {
                let n = self.count(self.focus).max(1);
                let forward = key == Key::Char('n');
                let from = (self.cursor() + if forward { 1 } else { n - 1 }) % n;
                let needle = self.last_search.clone();
                self.find(&needle, from, forward);
            }
            Key::Char(']') => self.select_file(self.file_sel + 1),
            Key::Char('[') => self.select_file(self.file_sel.saturating_sub(1)),
            Key::Char('r') => self.reload(),
            Key::Char('a') => {
                self.all = !self.all;
                self.load_log();
                self.detail_dirty = true;
            }
            Key::Char('d') => {
                self.detailed = !self.detailed;
                self.load_log();
                self.load_detail(true);
            }
            Key::Char('t') => {
                self.tree_mode = !self.tree_mode;
                if self.tree_mode {
                    if self.tree_stale {
                        self.sync_tree();
                    }
                    let current = self.detail.files.get(self.file_sel).and_then(|f| self.tree.row_of(&f.path));
                    self.tree_sel = current.unwrap_or(self.tree_sel);
                }
            }
            Key::Char('f') => {
                if let Some(path) = self.pointed_path() {
                    let same = self.filter.as_deref() == Some(path.as_str());
                    self.set_filter((!same).then_some(path));
                }
            }
            Key::Char('y') => {
                let text = match self.focus {
                    Pane::Timeline => self.selected_hash().map(str::to_string),
                    _ => self.pointed_path(),
                };
                if let Some(text) = text {
                    term::copy(&text);
                    self.msg = format!("Copied {text}");
                }
            }
            key => self.pane_key(key),
        }
        false
    }

    /// Cursor movement shared by all panes, in rows.
    fn motion(&self, key: Key) -> Option<isize> {
        let page = self.areas[PANES.iter().position(|&p| p == self.focus)?].h.max(2) as isize - 1;
        Some(match key {
            Key::Char('j') | Key::Down => 1,
            Key::Char('k') | Key::Up => -1,
            Key::PgDn | Key::Ctrl(b'f') | Key::Char(' ') => page,
            Key::PgUp | Key::Ctrl(b'b') => -page,
            Key::Ctrl(b'd') => page / 2,
            Key::Ctrl(b'u') => -page / 2,
            Key::Char('g') | Key::Home => isize::MIN,
            Key::Char('G') | Key::End => isize::MAX,
            _ => return None,
        })
    }

    /// Left and right: sideways in the change, in and out everywhere else.
    fn pane_key(&mut self, key: Key) {
        let right = matches!(key, Key::Char('l') | Key::Right);
        if !right && !matches!(key, Key::Char('h') | Key::Left) {
            return;
        }
        match self.focus {
            Pane::Change if right => self.change_x += 8,
            Pane::Change => self.change_x = self.change_x.saturating_sub(8),
            Pane::Files if self.tree_mode => {
                let Some(node) = self.tree.row(self.tree_sel) else { return };
                let (path, dir, parent) = (node.path.clone(), node.dir, node.parent as usize);
                let open = self.is_open(&path);
                if dir && open != right {
                    self.toggle_dir(path);
                } else if right {
                    self.forward();
                } else if parent != 0 {
                    self.tree_sel = self.tree.row_of(&self.tree.nodes[parent].path).unwrap_or(self.tree_sel);
                } else {
                    self.back();
                }
            }
            _ if right => self.forward(),
            _ => self.back(),
        }
    }

    fn pane_at(&self, x: usize, y: usize) -> Option<Pane> {
        PANES.into_iter().zip(self.areas).find(|(_, r)| r.contains(x, y)).map(|(p, _)| p)
    }

    fn wheel(&mut self, x: usize, y: usize, delta: isize) {
        let Some(pane) = self.pane_at(x, y) else { return };
        // The wheel scrolls what is under the pointer without moving focus.
        let focus = std::mem::replace(&mut self.focus, pane);
        self.move_cursor(delta);
        self.focus = focus;
    }

    fn drag_to(&mut self, x: usize, y: usize) {
        match self.drag {
            Some(Divider::Column) => self.split_x = Some(x),
            Some(Divider::Row) => self.split_y = Some(y),
            None => {}
        }
    }

    fn click(&mut self, x: usize, y: usize) {
        self.help = false;
        let (div_x, div_y) = self.dividers;
        if div_x != 0 && x == div_x {
            self.drag = Some(Divider::Column);
            return;
        }
        if div_y != 0 && y == div_y && x < div_x {
            self.drag = Some(Divider::Row);
            return;
        }
        let Some(pane) = self.pane_at(x, y) else { return };
        self.focus = pane;
        let area = self.areas[PANES.iter().position(|&p| p == pane).unwrap_or(0)];
        match pane {
            Pane::Timeline => self.set_cursor(self.top + y - area.y),
            Pane::Change => {}
            Pane::Files if !self.tree_mode => self.select_file(self.file_top + y - area.y),
            Pane::Files => {
                let row = self.tree_top + y - area.y;
                let Some(node) = self.tree.row(row) else { return };
                let (path, dir) = (node.path.clone(), node.dir);
                self.tree_sel = row;
                if dir {
                    self.toggle_dir(path);
                } else if let Some(i) = self.detail.files.iter().position(|f| f.path == path) {
                    self.select_file(i);
                }
            }
        }
    }
}
