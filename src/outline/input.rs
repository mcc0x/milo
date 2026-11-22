//! Handles keys and clicks in the outline, or passes them to its reader.

use std::rc::Rc;
use std::time::Instant;

use crate::keys::Direction::{Next, Previous};
use crate::keys::{DOUBLE_CLICK, Key};
use crate::pane::{After, Pane};
use crate::preferences::Preferences;
use crate::reader::ReadFile;

use super::Outline;
use super::draw::{Layout, margin};

pub(super) struct Click {
    target: ClickTarget,
    cell: (usize, usize),
    at: Instant,
}

#[derive(PartialEq, Eq)]
enum ClickTarget {
    Tree(Rc<str>),
    Strip(usize),
}

enum Pointed {
    File(Rc<str>),
    OpenFolder(Rc<str>),
    ClosedFolder(Rc<str>),
}

impl Outline {
    pub(super) fn outline_key(&mut self, key: Key, now: Instant) -> After {
        if let Key::Focus(here) = key {
            if !here {
                if let Some(reader) = &mut self.reading {
                    reader.cover();
                }
                self.clicked = None;
                if let Some(search) = &mut self.search {
                    search.clear_click();
                }
            }
            self.attention.focus(here, now);
            return After::Stay;
        }
        let reschedule = self.attention.used(now);
        if let Some(reader) = &mut self.reading {
            return match reader.key(key) {
                After::Leave => {
                    self.reading = None;
                    After::Stay
                }
                after => after,
            };
        }
        if let Some(search) = &mut self.search {
            use super::search::Action;

            match search.key(key, now) {
                Action::Ignored => return After::Ignored,
                Action::Redraw => {}
                Action::Close => self.search = None,
                Action::Open => {
                    if let Some(path) = search.selected_path().map(Rc::from) {
                        self.search = None;
                        self.open_file(ClickTarget::Tree(path));
                    }
                }
            }
            return After::Stay;
        }
        if key == Key::Char('/') || matches!(key, Key::Click(x, y) if self.painted.find_at(x, y)) {
            self.start_search();
            return After::Stay;
        }
        if key == Key::Char('q') || self.peek && key == Key::Esc {
            return After::Leave;
        }
        let by_click = self.by_click;
        if !matches!(key, Key::Click(..)) {
            self.clicked = None;
            self.by_click = false;
        }
        let changed = match key {
            Key::Char('r') => {
                if self.repo.git {
                    self.untracked.request_sweep();
                    self.check_untracked(now);
                    self.repo.invalidate_tracked_diff();
                    self.reload();
                } else {
                    self.check_folder();
                }
                true
            }
            _ if self.painted.strip => self.strip_key(key, now),
            _ => self.tree_key(key, now),
        };
        if changed || reschedule || by_click != self.by_click { After::Stay } else { After::Ignored }
    }

    /// The strip has no cursor. Keys scroll it; a double click opens a file.
    fn strip_key(&mut self, key: Key, now: Instant) -> bool {
        if let Key::Click(x, y) = key {
            let Some(file) = self.painted.strip_file_at(x, y) else {
                self.clicked = None;
                return false;
            };
            let Some(target) = self.repeat_click(ClickTarget::Strip(file), (x, y), now) else { return false };
            self.open_file(target);
            return true;
        }
        if self.painted.body_rows == 0 {
            return false;
        }
        let delta = match key {
            Key::Char('l') | Key::Right => Some(1),
            Key::Char('h') | Key::Left => Some(-1),
            Key::Ctrl(_) => None,
            _ => key.rows(1),
        };
        let Some(delta) = delta else { return false };
        let last = self.painted.last_column(self.recent.len());
        let column = match delta {
            isize::MIN => 0,
            isize::MAX => last,
            _ => self.strip_col.saturating_add_signed(delta.signum()).min(last),
        };
        std::mem::replace(&mut self.strip_col, column) != column
    }

    fn tree_key(&mut self, key: Key, now: Instant) -> bool {
        let sel = self.sel;
        match key {
            Key::Char('e') => {
                self.folds.toggle_all();
                self.refold(self.selected_path());
            }
            Key::Char('c') if self.repo.git => {
                self.pending_only = !self.pending_only;
                if self.remember && Preferences::set_changes_only(&self.repo.root, self.pending_only).is_err() {
                    self.say("Could not save changes-only preference");
                }
                self.relayout(self.selected_path());
            }
            Key::Char('n' | 'N') => {
                let direction = if key == Key::Char('n') { Next } else { Previous };
                if let Some(path) = self.tree.next_pending(sel, direction).cloned() {
                    self.reveal(path);
                } else if self.tree.has_pending_files() {
                    self.say("Only changed file");
                } else {
                    self.say("No changed files");
                }
            }
            Key::Char('}') => self.sel = self.tree.folder_row(sel, Next).unwrap_or(sel),
            Key::Char('{') => self.sel = self.tree.folder_row(sel, Previous).unwrap_or(sel),
            Key::Char('J') => self.sel = self.tree.sibling_row(sel, Next).unwrap_or(sel),
            Key::Char('K') => self.sel = self.tree.sibling_row(sel, Previous).unwrap_or(sel),
            Key::Char('p') => self.sel = self.tree.parent_row(sel).unwrap_or(sel),
            Key::Click(x, y) => {
                let Some(row) = self.painted.row_at(x, y) else {
                    self.clicked = None;
                    return false;
                };
                self.top = self.painted.tree_rows.start;
                self.click(row, (x, y), now);
            }
            Key::Enter | Key::Char(' ' | 'h' | 'l') | Key::Left | Key::Right => self.open_or_close(key),
            _ => return self.tree_scroll(key, self.painted.body_rows),
        }
        true
    }

    fn pointed(&self) -> Option<Pointed> {
        let node = self.tree.row(self.sel)?;
        let path = Rc::clone(&node.path);
        Some(if !node.dir {
            Pointed::File(path)
        } else if self.folds.is_open(&path) {
            Pointed::OpenFolder(path)
        } else {
            Pointed::ClosedFolder(path)
        })
    }

    fn open_or_close(&mut self, key: Key) {
        match (key, self.pointed()) {
            (Key::Enter | Key::Char(' ' | 'l') | Key::Right, Some(Pointed::ClosedFolder(path))) => {
                self.set_open(path, true);
            }
            (Key::Enter | Key::Char(' ' | 'h') | Key::Left, Some(Pointed::OpenFolder(path))) => {
                self.set_open(path, false);
            }
            (Key::Enter, Some(Pointed::File(path))) => {
                self.open_file(ClickTarget::Tree(path));
            }
            (Key::Char('h') | Key::Left, _) => self.sel = self.tree.parent_row(self.sel).unwrap_or(self.sel),
            _ => {}
        }
    }

    fn set_open(&mut self, path: Rc<str>, open: bool) {
        let mut remembered = false;
        if self.folds.remember(&path)
            && let Some(store) = &self.fold_store
        {
            match store.set(&path, open) {
                Ok(()) => remembered = true,
                Err(_) => self.say("Could not save folder choice"),
            }
        }
        self.folds.set(&path, open, remembered);
        self.refold(Some(path));
    }

    fn tree_scroll(&mut self, key: Key, shown: usize) -> bool {
        let Some(rows) = key.rows(shown) else { return false };
        let before = (self.sel, self.top);
        // The wheel moves the view; the other keys move the cursor.
        if matches!(key, Key::WheelUp | Key::WheelDown) {
            self.scroll_under_pointer(rows, shown.max(1));
        } else {
            let last = self.tree.rows.len().saturating_sub(1);
            self.sel = self.sel.saturating_add_signed(rows).min(last);
        }
        (self.sel, self.top) != before
    }

    fn scroll_under_pointer(&mut self, delta: isize, rows: usize) {
        let len = self.tree.rows.len();
        let last_top = len.saturating_sub(rows);
        self.top = self.top.saturating_add_signed(delta).min(last_top);
        let margin = margin(rows);
        let low = if self.top == 0 { 0 } else { self.top + margin };
        let high = if self.top == last_top { len.saturating_sub(1) } else { self.top + rows - 1 - margin };
        self.sel = self.sel.clamp(low, high.max(low));
    }

    /// Clicks toggle folders, select files, and open files when repeated.
    fn click(&mut self, row: usize, cell: (usize, usize), now: Instant) {
        let Some(node) = self.tree.row(row) else { return };
        let target = ClickTarget::Tree(Rc::clone(&node.path));
        let folder = node.dir.then(|| Rc::clone(&node.path));
        (self.sel, self.by_click) = (row, true);
        match (folder, self.repeat_click(target, cell, now)) {
            (Some(path), None) => {
                let open = self.folds.is_open(&path);
                self.set_open(path, !open);
            }
            (None, Some(target)) => self.open_file(target),
            _ => {} // A double click on a folder is one toggle, not two.
        }
    }

    /// Only a repeated click on the same target and terminal cell opens it.
    fn repeat_click(&mut self, target: ClickTarget, cell: (usize, usize), now: Instant) -> Option<ClickTarget> {
        if self.clicked.take().is_some_and(|click| {
            click.target == target && click.cell == cell && now.saturating_duration_since(click.at) < DOUBLE_CLICK
        }) {
            return Some(target);
        }
        self.clicked = Some(Click { target, cell, at: now });
        None
    }

    pub(super) fn record_layout(&mut self, layout: Layout) {
        if let Some(Click { target: ClickTarget::Strip(file), cell: (x, y), .. }) = &self.clicked
            && layout.strip_file_at(*x, *y) != Some(*file)
        {
            self.clicked = None;
        }
        self.painted = layout;
    }

    /// Pending indices no longer identify the files in the last painted strip.
    pub(super) fn invalidate_strip_clicks(&mut self) {
        self.painted.clear_strip_targets();
        if matches!(self.clicked, Some(Click { target: ClickTarget::Strip(_), .. })) {
            self.clicked = None;
        }
    }

    /// Opens the reader in this pane, or replaces the floating reader in Rex.
    fn open_file(&mut self, target: ClickTarget) {
        let path: &str = match &target {
            ClickTarget::Tree(path) => path,
            ClickTarget::Strip(file) => self.pending[*file].path.as_str(),
        };
        match self.opener.open(&self.repo, ReadFile::Working(path)) {
            Ok(reader) => {
                self.reading = reader;
                if self.reading.is_some() {
                    self.painted.clear_tree_targets();
                    self.painted.clear_strip_targets();
                }
            }
            Err(why) => self.say(why),
        }
    }
}
