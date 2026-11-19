use std::time::Instant;

use crate::keys::{DOUBLE_CLICK, Key};
use crate::pane::After;
use crate::reader::ReadFile;

use super::{Click, FILE_PREVIEW, History, TimelineRow};

impl History {
    pub(super) fn input(&mut self, key: Key, now: Instant) -> After {
        let unclicked = !matches!(key, Key::Click(..)) && self.clicked.take().is_some();
        let cleared = self.read_error.take().is_some();
        match key {
            Key::Click(x, y) => self.click(x, y, now),
            Key::Enter => self.enter(),
            Key::Left | Key::Char('h') => self.left(),
            Key::Right | Key::Char('l') => {
                if let Some(detail) = &mut self.detail
                    && detail.next != detail.start
                {
                    detail.start = detail.next;
                } else if !unclicked && !cleared {
                    return After::Ignored;
                }
            }
            Key::Char('r') => self.load(),
            _ => {
                if let Some(delta) = key.rows(self.shown) {
                    let before = (self.cursor, self.top);
                    self.scroll(delta);
                    if matches!(key, Key::WheelUp | Key::WheelDown) {
                        self.top = self.top.saturating_add_signed(delta);
                        self.painted = None;
                    }
                    if (self.cursor, self.top) != before || self.painted.is_none() {
                        return After::Stay;
                    }
                }
                return if unclicked || cleared { After::Stay } else { After::Ignored };
            }
        }
        After::Stay
    }

    fn enter(&mut self) {
        match self.selected() {
            Some(TimelineRow::Commit(i)) => {
                self.toggle_files(i);
                if self.open_index() == Some(i) {
                    self.keep_preview_in_view();
                }
            }
            Some(TimelineRow::File(i)) => self.read_file(i),
            Some(TimelineRow::More) => self.show_all_files(),
            _ => {}
        }
    }

    fn read_file(&mut self, i: usize) {
        let Some(open) = self.open_index() else { return };
        let entry = &self.entries[open];
        let Some(file) = entry.files.as_ref().and_then(|files| files.get(i)) else { return };
        let commit = &entry.commit;
        match self.opener.open(&self.repo, ReadFile::Committed { commit, file }) {
            Ok(reader) => self.reading = reader,
            Err(why) => self.read_error = Some(why),
        }
    }

    /// Make room for the preview when a keyboard opens a commit at the edge.
    fn keep_preview_in_view(&mut self) {
        let files = self.open_files().map_or(0, <[_]>::len);
        let preview = files.min(FILE_PREVIEW) + usize::from(files > FILE_PREVIEW);
        let shown = self.shown.max(1);
        let room = preview.max(1).min(shown - 1);
        self.top = self.top.max((self.cursor + room + 1).saturating_sub(shown));
    }

    fn left(&mut self) {
        match (self.selected(), self.open_index()) {
            (Some(TimelineRow::File(_) | TimelineRow::More), Some(open)) => {
                self.cursor = self.rows.iter().position(|&row| row == TimelineRow::Commit(open)).unwrap_or(self.cursor);
            }
            (Some(TimelineRow::Commit(i)), Some(open)) if i == open => self.toggle_files(i),
            _ => {}
        }
    }

    fn click(&mut self, x: usize, y: usize, now: Instant) {
        let Some(painted) = &self.painted else { return };
        let Some(offset) = y.checked_sub(painted.body_top).filter(|&offset| offset < self.shown) else {
            self.clicked = None;
            return;
        };
        let at = painted.top + offset;
        let Some(&row) = self.rows.get(at).filter(|row| x < painted.width && row.selectable()) else {
            self.clicked = None;
            return;
        };
        let again = self.clicked.is_some_and(|click| click.row == row && now.duration_since(click.at) < DOUBLE_CLICK);
        self.cursor = at;
        self.clicked = Some(Click { row, offset, at: now });
        match (row, again) {
            (TimelineRow::Commit(i), false) => {
                self.toggle_files(i);
                self.top = self.cursor.saturating_sub(offset);
            }
            (TimelineRow::File(i), true) => {
                self.clicked = None;
                self.read_file(i);
            }
            (TimelineRow::More, _) => {
                self.clicked = None;
                self.show_all_files();
            }
            _ => {}
        }
    }

    fn scroll(&mut self, delta: isize) {
        if delta == 0 || self.rows.is_empty() {
            return;
        }
        let at = self.cursor.saturating_add_signed(delta).min(self.rows.len() - 1);
        let before = || self.rows[..=at].iter().rposition(|row| row.selectable());
        let after = || self.rows[at..].iter().position(|row| row.selectable()).map(|i| at + i);
        let next = if delta < 0 { before().or_else(after) } else { after().or_else(before) };
        self.cursor = next.unwrap_or(self.cursor);
    }

    pub(super) fn keep_cursor_in_view(&mut self) {
        // Do not bring the top back under the pointer after a click.
        if self.clicked.is_none()
            && self.open_index() != Some(0)
            && self.selected() == Some(TimelineRow::Commit(0))
            && self.cursor < self.shown
        {
            self.top = 0;
        } else if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + self.shown {
            self.top = self.cursor + 1 - self.shown;
        }
        self.top = self.top.min(self.rows.len().saturating_sub(self.shown));
    }
}
