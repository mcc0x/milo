//! Handles keys and clicks in the outline, or passes them to its reader.

use std::rc::Rc;
use std::time::Instant;

use crate::keys::Direction::{Next, Previous};
use crate::keys::{DOUBLE_CLICK, Key};
use crate::pane::{After, Pane};
use crate::preferences::Preferences;

use super::Outline;
use super::draw::margin;

enum Pointed {
    File(Rc<str>),
    OpenFolder(Rc<str>),
    ClosedFolder(Rc<str>),
}

impl Outline {
    pub(super) fn outline_key(&mut self, key: Key, now: Instant) -> After {
        if let Key::Focus(here) = key {
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
        if key == Key::Char('q') || self.peek && key == Key::Esc {
            return After::Leave;
        }
        let by_click = self.by_click;
        self.by_click &= matches!(key, Key::Click(..));
        let changed = match key {
            Key::Char('r') => {
                if self.repo.git {
                    self.check_untracked(now);
                    self.reload();
                } else {
                    self.refresh_folder(self.selected_path());
                }
                true
            }
            _ if self.painted.strip => self.strip_key(key),
            _ => self.tree_key(key, now),
        };
        if changed || reschedule || by_click != self.by_click { After::Stay } else { After::Ignored }
    }

    /// The strip has no cursor: its keys move it a column at a time.
    fn strip_key(&mut self, key: Key) -> bool {
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
                }
            }
            Key::Char('}') => self.sel = self.tree.folder_row(sel, Next).unwrap_or(sel),
            Key::Char('{') => self.sel = self.tree.folder_row(sel, Previous).unwrap_or(sel),
            Key::Char('J') => self.sel = self.tree.sibling_row(sel, Next).unwrap_or(sel),
            Key::Char('K') => self.sel = self.tree.sibling_row(sel, Previous).unwrap_or(sel),
            Key::Char('p') => self.sel = self.tree.parent_row(sel).unwrap_or(sel),
            Key::Click(x, y) => {
                if let Some(row) = self.painted.row_at(x, y) {
                    self.click(self.top + row, now);
                }
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
                self.open_file(&path);
            }
            (Key::Char('h') | Key::Left, _) => self.sel = self.tree.parent_row(self.sel).unwrap_or(self.sel),
            _ => {}
        }
    }

    fn set_open(&mut self, path: Rc<str>, open: bool) {
        self.folds.set(&path, open);
        if self.folds.remember(&path)
            && let Some(store) = &self.fold_store
            && store.set(&path, open).is_err()
        {
            self.say("Could not save folder choice");
        }
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
    fn click(&mut self, row: usize, now: Instant) {
        let Some(node) = self.tree.row(row) else { return };
        let (path, dir) = (node.path.clone(), node.dir);
        let again =
            self.clicked.take().is_some_and(|(p, at)| p == path && now.saturating_duration_since(at) < DOUBLE_CLICK);
        (self.sel, self.by_click) = (row, true);
        match (dir, again) {
            (true, false) => {
                let open = self.folds.is_open(&path);
                self.clicked = Some((path.clone(), now));
                self.set_open(path, !open);
            }
            (true, true) => {} // A double click on a folder is one toggle, not two.
            (false, false) => self.clicked = Some((path, now)),
            (false, true) => self.open_file(&path),
        }
    }
}
