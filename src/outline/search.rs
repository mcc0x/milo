//! A separate list of file matches, leaving the outline's selection and folds alone.

use std::borrow::Cow;
use std::ops::Range;
use std::rc::Rc;
use std::thread::{Builder, JoinHandle};
use std::time::{Duration, Instant};

use crate::keys::{DOUBLE_CLICK, Key};
use crate::pane::After;
use crate::screen::{Screen, Style, Wash};
use crate::ui::{self, FAINT};
use crate::walk;
use crate::width::{byte_at_col, str_width};

use super::Outline;

pub(super) const WALK_FRAME: Duration = Duration::from_millis(40);
const QUERY_LIMIT: usize = 4096;
const OPEN: &str = "Enter open";

pub(super) enum Action {
    Ignored,
    Redraw,
    Close,
    Open,
}

#[derive(Default, PartialEq, Eq)]
enum Status {
    #[default]
    Loading,
    Complete,
    Partial,
    Unavailable,
}

#[derive(Default)]
enum Files {
    #[default]
    None,
    Git(Vec<Rc<str>>),
    Folder(Vec<String>),
}

impl Files {
    fn paths(&self) -> impl Iterator<Item = &str> {
        let (git, folder): (&[Rc<str>], &[String]) = match self {
            Self::Git(paths) => (paths, &[]),
            Self::Folder(paths) => (&[], paths),
            Self::None => (&[], &[]),
        };
        git.iter().map(AsRef::as_ref).chain(folder.iter().map(String::as_str))
    }

    fn path(&self, i: usize) -> &str {
        match self {
            Self::Git(paths) => &paths[i],
            Self::Folder(paths) => &paths[i],
            Self::None => unreachable!("without paths there are no results"),
        }
    }
}

#[derive(Default)]
pub(super) struct Search {
    files: Files,
    status: Status,
    pending_only: bool,
    query: String,
    notice: Option<&'static str>,
    matches: Vec<usize>,
    dirty: bool,
    selected: usize,
    top: usize,
    pan: usize,
    path_width: usize,
    /// Hit targets from the latest paint, cleared when the list changes.
    painted: Range<usize>,
    width: usize,
    rows: usize,
    close: Range<usize>,
    open_y: Option<usize>,
    clicked: Option<(usize, usize, usize, Instant)>,
}

impl Outline {
    pub(super) fn start_search(&mut self) {
        let mut search = Search { pending_only: self.pending_only, ..Search::default() };
        if self.repo.git {
            self.search_walk = None;
            let mut paths: Vec<_> = self.tree.files(self.pending_only).cloned().collect();
            paths.sort_unstable();
            search.status = if !self.repo.ok() && paths.is_empty() {
                Status::Unavailable
            } else if !self.repo.ok() || self.repo.paths_incomplete() {
                Status::Partial
            } else {
                Status::Complete
            };
            search.files = Files::Git(paths);
            search.invalidate();
        } else {
            // A full, bounded walk may be slow. Keep Escape and typing responsive,
            // and reuse an in-flight walk if search was just closed and reopened.
            if self.search_walk.as_ref().is_none_or(JoinHandle::is_finished) {
                let root = self.repo.root.clone();
                self.search_walk = Builder::new().spawn(move || walk::list(&root, |_| true)).ok();
            }
            if self.search_walk.is_none() {
                search.status = Status::Unavailable;
            }
        }
        self.clicked = None;
        self.painted.clear_tree_targets();
        self.painted.clear_strip_targets();
        self.search = Some(search);
    }

    pub(super) fn draw_search(&mut self, screen: &mut Screen, now: Instant) {
        self.expire(now);
        let Some(search) = &mut self.search else { return };
        if search.status == Status::Loading
            && self.search_walk.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(walk) = self.search_walk.take()
        {
            if let Ok(listing) = walk.join() {
                search.load_folder(listing);
            } else {
                search.status = Status::Unavailable;
            }
        }
        search.draw(screen);
    }
}

impl Search {
    fn load_folder(&mut self, mut listing: walk::Listing) {
        self.status = if listing.unreadable && listing.paths.is_empty() {
            Status::Unavailable
        } else if listing.unreadable || listing.truncated {
            Status::Partial
        } else {
            Status::Complete
        };
        listing.paths.retain(|path| !path.ends_with('/'));
        self.files = Files::Folder(listing.paths);
        self.invalidate();
    }

    fn filter(&mut self) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        let folded = if self.query.is_ascii() {
            Cow::Borrowed(self.query.as_str())
        } else {
            Cow::Owned(self.query.chars().flat_map(char::to_lowercase).collect())
        };
        self.matches.clear();
        self.matches
            .extend(self.files.paths().enumerate().filter_map(|(i, path)| contains(path, &folded).then_some(i)));
    }

    fn invalidate(&mut self) {
        self.dirty = true;
        (self.selected, self.top, self.pan) = (0, 0, 0);
        self.painted = 0..0;
        self.open_y = None;
        self.clear_click();
    }

    pub(super) fn selected_path(&self) -> Option<&str> {
        self.matches.get(self.selected).map(|&i| self.files.path(i))
    }

    pub(super) fn clear_click(&mut self) {
        self.clicked = None;
    }

    pub(super) fn paste(&mut self, text: Option<&str>) -> After {
        let Some(line) = text.and_then(|text| text.split(['\r', '\n']).next()) else {
            self.notice = Some("Paste too long");
            return After::Stay;
        };
        if self.query.len() + line.len() > QUERY_LIMIT {
            self.notice = Some("Query too long");
            return After::Stay;
        }
        let before = self.query.len();
        self.query.extend(line.chars().filter(|ch| !ch.is_control()));
        if self.query.len() == before {
            return After::Ignored;
        }
        self.notice = None;
        self.invalidate();
        After::Stay
    }

    pub(super) fn key(&mut self, key: Key, now: Instant) -> Action {
        if !matches!(key, Key::Click(..)) {
            self.clear_click();
        }
        match key {
            Key::Esc | Key::Ctrl(b'c') => return Action::Close,
            Key::Enter => {
                self.filter();
                return if self.selected_path().is_some() { Action::Open } else { Action::Ignored };
            }
            Key::Click(x, 0) if self.close.contains(&x) => return Action::Close,
            Key::Click(x, y) if self.open_y == Some(y) && (1..1 + OPEN.len()).contains(&x) => return Action::Open,
            Key::Click(x, y) => return self.click(x, y, now),
            Key::Char(ch) if !ch.is_control() => {
                let action = self.paste(Some(ch.encode_utf8(&mut [0; 4])));
                return if action == After::Stay { Action::Redraw } else { Action::Ignored };
            }
            Key::Backspace | Key::Ctrl(b'h') => {
                self.query.pop();
                self.notice = None;
                self.invalidate();
            }
            Key::Ctrl(b'u') => {
                self.query.clear();
                self.notice = None;
                self.invalidate();
            }
            Key::Left | Key::Right => {
                self.filter();
                let Some(path) = self.selected_path() else { return Action::Ignored };
                let max = str_width(path).saturating_sub(self.path_width.max(1));
                let step = (self.path_width / 3).max(1);
                let next = if key == Key::Left { self.pan.saturating_sub(step) } else { (self.pan + step).min(max) };
                if next == self.pan {
                    return Action::Ignored;
                }
                self.pan = next;
            }
            key => {
                let Some(delta) = key.rows(self.rows) else { return Action::Ignored };
                self.filter();
                let next = self.selected.saturating_add_signed(delta).min(self.matches.len().saturating_sub(1));
                if self.selected == next {
                    return Action::Ignored;
                }
                self.selected = next;
                self.pan = 0;
            }
        }
        Action::Redraw
    }

    fn click(&mut self, x: usize, y: usize, now: Instant) -> Action {
        let Some(offset) = y.checked_sub(1).filter(|&i| x < self.width && i < self.painted.len()) else {
            self.clear_click();
            return Action::Ignored;
        };
        self.selected = self.painted.start + offset;
        if self.clicked.is_some_and(|(i, a, b, clicked)| {
            (i, a, b) == (self.selected, x, y) && now.saturating_duration_since(clicked) < DOUBLE_CLICK
        }) {
            return Action::Open;
        }
        self.clicked = Some((self.selected, x, y, now));
        Action::Redraw
    }

    fn empty_message(&self) -> &'static str {
        match self.status {
            Status::Loading => "Listing files…",
            Status::Unavailable => "Cannot list files",
            _ if self.query.is_empty() => "No files",
            _ => "No matches",
        }
    }

    fn draw_path(&self, s: &mut Screen, x: usize, y: usize, path: &str) {
        if self.pan > 0 {
            let at = byte_at_col(path, self.pan);
            s.put(x, y, 1, "‹", FAINT);
            s.put_fit(x + 1, y, self.path_width.saturating_sub(1), &path[at..], Style::PLAIN);
        } else {
            s.put_middle(x, y, self.path_width, path, Style::PLAIN);
        }
    }

    fn draw(&mut self, s: &mut Screen) {
        self.filter();
        s.clear();
        self.open_y = None;
        self.close = 0..0;
        let (w, h) = (s.w, s.h);
        if h == 0 || w == 0 {
            self.painted = 0..0;
            return;
        }
        let inset = usize::from(w >= 8);
        let close = if w >= 24 && h > 1 {
            "Esc back"
        } else if w >= 4 {
            "×"
        } else {
            ""
        };
        let close_at = w - inset - str_width(close);
        self.close = close_at..close_at + s.put(close_at, 0, w, close, FAINT);
        let partial = self.status == Status::Partial;
        let badge = if partial && w >= 40 {
            "Partial"
        } else if partial && w >= 8 {
            "!"
        } else {
            ""
        };
        let badge_at = close_at.saturating_sub(str_width(badge) + usize::from(!badge.is_empty()));
        s.put(badge_at, 0, w, badge, ui::DEL);
        let prefix = if w > 3 { "/ " } else { "/" };
        let x = inset + s.put(inset, 0, w, prefix, Style::PLAIN.bold());
        let room = badge_at.saturating_sub(x + usize::from(!close.is_empty()));
        // Even a one-row strip can show its current match or say why there is none.
        let single_row = h == 1 && room >= 14;
        let room = if single_row { room / 3 } else { room };
        if self.query.is_empty() {
            let label = if self.pending_only { "Find changed files" } else { "Find files" };
            s.put_fit(x, 0, room, label, FAINT);
        } else {
            let skip = str_width(&self.query).saturating_sub(room);
            if skip == 0 {
                s.put(x, 0, room, &self.query, Style::PLAIN);
            } else if room > 0 {
                let at = byte_at_col(&self.query, skip + 1);
                s.put(x, 0, 1, "…", FAINT);
                s.put(x + 1, 0, room - 1, &self.query[at..], Style::PLAIN);
            }
        }

        let len = self.matches.len();
        let footer = h >= 4 && (self.notice.is_some() || len > 0 && w >= OPEN.len() + 3);
        let rows = h - 1 - usize::from(footer);
        if (w, rows) != (self.width, self.rows) {
            self.clear_click();
            self.pan = 0;
        }
        (self.width, self.rows) = (w, rows);
        self.path_width = w - 2 * inset;
        if rows == 0 {
            self.painted = 0..0;
            if single_row {
                let x = x + room + 2;
                self.path_width = badge_at.saturating_sub(x + 1);
                if let Some(path) = self.selected_path().filter(|_| self.notice.is_none()) {
                    self.draw_path(s, x, 0, path);
                } else {
                    s.put_fit(x, 0, self.path_width, self.notice.unwrap_or_else(|| self.empty_message()), FAINT);
                }
            }
            return;
        }
        if len == 0 {
            s.put_fit(inset, 1, self.path_width, self.notice.unwrap_or_else(|| self.empty_message()), FAINT);
            return;
        }
        self.top =
            self.top.clamp((self.selected + 1).saturating_sub(rows), self.selected).min(len.saturating_sub(rows));
        self.painted = self.top..(self.top + rows).min(len);
        for (i, y) in self.painted.clone().zip(1..) {
            let path = self.files.path(self.matches[i]);
            self.draw_path(s, inset, y, path);
            if i == self.selected {
                s.wash(0, y, w, Wash::Bar);
            }
        }
        if footer {
            let hint = if w >= 36 { "Enter open · ↑↓ move · ←→ pan" } else { "Enter open · ↑↓ move" };
            let text = self.notice.unwrap_or(hint);
            s.put_fit(inset, h - 1, self.path_width, text, FAINT);
            if self.notice.is_none() {
                self.open_y = Some(h - 1);
            }
        }
    }
}

/// The query is lowercase if non-ASCII. Never allocate a lowercase copy of each path.
fn contains(path: &str, folded: &str) -> bool {
    if folded.is_empty() {
        return true;
    }
    if path.is_ascii() {
        return path.as_bytes().windows(folded.len()).any(|part| part.eq_ignore_ascii_case(folded.as_bytes()));
    }
    path.char_indices().any(|(at, _)| {
        let mut tail = path[at..].chars().flat_map(char::to_lowercase);
        folded.chars().all(|ch| tail.next().is_some_and(|c| c.eq_ignore_ascii_case(&ch)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_search_marks_partial_walks_without_treating_folders_as_files() {
        let mut search = Search::default();
        search.load_folder(walk::Listing {
            truncated: true,
            paths: vec!["nested/".into(), "nested/path-a.txt".into()],
            ..walk::Listing::default()
        });
        let mut screen = Screen::sized(48, 1);
        search.draw(&mut screen);
        assert!(screen.text()[0].contains("Partial"));
        assert!(screen.text()[0].contains("nested/path-a.txt"));
        assert_eq!(search.matches.len(), 1);
        assert_eq!(search.selected_path(), Some("nested/path-a.txt"));

        search.paste(Some("not found"));
        search.draw(&mut screen);
        assert!(screen.text()[0].contains("No matches"));
        assert!(screen.text()[0].contains("Partial"));
        assert!(matches!(search.key(Key::Enter, Instant::now()), Action::Ignored));
    }

    #[test]
    fn outline_search_pans_the_part_of_a_path_that_does_not_fit() {
        let mut search = Search::default();
        search.load_folder(walk::Listing {
            paths: vec!["prefix/ancestor/different-component/common/guide.md".into()],
            ..walk::Listing::default()
        });
        let mut screen = Screen::sized(20, 5);
        search.draw(&mut screen);
        assert!(!screen.text()[1].contains("different"));
        let now = Instant::now();
        search.key(Key::Right, now);
        search.key(Key::Right, now);
        search.draw(&mut screen);
        assert!(screen.text()[1].contains("different"));
        assert!(screen.text()[1].contains("‹"));
    }
}
