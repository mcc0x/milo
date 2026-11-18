//! Shows recent commits and the files they touched in a pane of their own.

use std::collections::HashMap;
use std::time::Instant;

use crate::git::{Commit, Ends, PendingFile, Repo};
use crate::help::{self, Page};
use crate::keys::Key;
use crate::outline::DOUBLE_CLICK;
use crate::reader::{OpenReader, ReadFile, Reader};
use crate::screen::{self, Screen, Style, Wash};
use crate::ui::{self, FAINT, Pane};
use crate::watch::Scope;
use crate::when;
use crate::width::str_width;

/// Keep enough history to page back without another git read.
const LIMIT: usize = 300;
const HEADING_ROWS: usize = 4;
const DOT_ROWS: usize = 8;
const DOT_COLS: usize = 52;
const HINT_ROWS: usize = 12;
/// Show the first few files without losing the commit or the next row.
const FILE_PREVIEW: usize = 8;

struct Entry {
    commit: Commit,
    /// Arrived since this pane first read the history.
    fresh: bool,
}

/// Rows the cursor can point at, with the guides between them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    Uncommitted,
    Divider,
    Commit(usize),
    File(usize),
    More,
    Note(&'static str),
}

impl Row {
    fn selectable(self) -> bool {
        matches!(self, Self::Commit(_) | Self::File(_) | Self::More)
    }
}

/// The part of the pane last drawn as a list, where a click can land.
struct Painted {
    width: usize,
    body_top: usize,
}

pub struct History {
    repo: Repo,
    ends: Option<Ends>,
    entries: Vec<Entry>,
    ahead: Option<u32>,
    has_remote: bool,
    uncommitted: usize,
    /// The one commit whose files are shown, as an index into `entries`.
    open: Option<usize>,
    all_files: bool,
    rows: Vec<Row>,
    cursor: usize,
    top: usize,
    shown: usize,
    painted: Option<Painted>,
    /// The last row clicked and when, to tell a double click.
    clicked: Option<(Row, Instant)>,
    compact: bool,
    opener: OpenReader,
    reading: Option<Reader>,
    read_error: Option<&'static str>,
}

impl History {
    pub fn new(repo: Repo, inside_rex: bool) -> Self {
        let mut history = Self {
            repo,
            ends: None,
            entries: Vec::new(),
            ahead: None,
            has_remote: false,
            uncommitted: 0,
            open: None,
            all_files: false,
            rows: Vec::new(),
            cursor: 0,
            top: 0,
            shown: 0,
            painted: None,
            clicked: None,
            compact: false,
            opener: OpenReader::new(inside_rex),
            reading: None,
            read_error: None,
        };
        history.load();
        history
    }

    fn load(&mut self) {
        if !self.repo.git {
            return;
        }
        self.repo.read();
        if !self.repo.ok() {
            return;
        }
        if !self.refresh_remote() {
            return;
        }
        let selected = self.selected();
        let uncommitted = self.repo.uncommitted_count();
        let ahead = self.repo.upstream_ahead();
        if self.ends.as_ref() != Some(self.repo.ends()) || ahead != self.ahead {
            let Some(commits) = self.repo.history(LIMIT) else { return };
            self.ahead = ahead;
            self.replace_commits(commits, selected, uncommitted);
        } else if self.uncommitted != uncommitted {
            self.uncommitted = uncommitted;
            self.build_rows(selected);
        }
    }

    fn refresh_remote(&mut self) -> bool {
        let Some(has_remote) = self.repo.has_remote() else { return false };
        self.has_remote = has_remote;
        true
    }

    /// Remembers the open commit and the cursor by hash when commits move.
    fn replace_commits(&mut self, commits: Vec<Commit>, selected: Row, uncommitted: usize) {
        let remap = |i: usize| commits.iter().position(|c| c.hash == self.entries[i].commit.hash);
        let open = self.open.and_then(remap);
        let selected = match selected {
            // At the newest commit, follow new arrivals unless its files are open.
            Row::Commit(0) if self.open != Some(0) => Row::Commit(0),
            Row::Commit(i) => Row::Commit(remap(i).unwrap_or(0)),
            Row::File(_) | Row::More if open.is_some() => selected,
            _ => Row::Commit(0),
        };
        let before: HashMap<_, _> = self.entries.iter().map(|e| (e.commit.hash.as_str(), e.fresh)).collect();
        let loaded = self.ends.is_some();
        self.entries = commits
            .into_iter()
            .map(|commit| Entry { fresh: before.get(commit.hash.as_str()).copied().unwrap_or(loaded), commit })
            .collect();
        self.ends = Some(self.repo.ends().clone());
        self.all_files &= open.is_some();
        self.clicked = None;
        self.open = open;
        self.uncommitted = uncommitted;
        self.build_rows(selected);
    }

    fn selected(&self) -> Row {
        self.rows.get(self.cursor).copied().unwrap_or(Row::Note(""))
    }

    fn open_files(&self) -> Option<&[PendingFile]> {
        self.repo.commit_files(&self.entries.get(self.open?)?.commit)
    }

    fn build_rows(&mut self, selected: Row) {
        self.painted = None;
        let files = self.open_files().map(<[_]>::len);
        self.rows.clear();
        if self.uncommitted > 0 && (!self.compact || self.entries.is_empty()) {
            self.rows.push(Row::Uncommitted);
        }
        for (i, entry) in self.entries.iter().enumerate() {
            if !self.compact && i > 0 && crosses_remote(&self.entries[i - 1].commit, &entry.commit) {
                self.rows.push(Row::Divider);
            }
            self.rows.push(Row::Commit(i));
            if self.open == Some(i) {
                match files {
                    Some(0) => self.rows.push(Row::Note("No files changed.")),
                    Some(n) => {
                        let shown = if self.all_files { n } else { n.min(FILE_PREVIEW) };
                        self.rows.extend((0..shown).map(Row::File));
                        if shown < n {
                            self.rows.push(Row::More);
                        }
                    }
                    None => self.rows.push(Row::Note("Git did not answer for this commit.")),
                }
            }
        }
        if self.entries.is_empty() {
            self.rows.push(Row::Note("No commits yet."));
        }
        self.cursor = self.rows.iter().position(|&row| row == selected).unwrap_or(0);
    }

    fn toggle_files(&mut self, i: usize) {
        self.all_files = false;
        self.open = if self.open == Some(i) {
            None
        } else {
            self.repo.load_commit_files(&self.entries[i].commit);
            Some(i)
        };
        self.build_rows(Row::Commit(i));
    }

    fn show_all_files(&mut self) {
        self.all_files = true;
        self.build_rows(Row::File(FILE_PREVIEW));
    }

    fn enter(&mut self) {
        match self.selected() {
            Row::Commit(i) => {
                self.toggle_files(i);
                if self.open == Some(i) {
                    self.keep_preview_in_view();
                }
            }
            Row::File(i) => self.read_file(i),
            Row::More => self.show_all_files(),
            _ => {}
        }
    }

    fn read_file(&mut self, i: usize) {
        let Some(open) = self.open else { return };
        let commit = &self.entries[open].commit;
        let Some(file) = self.repo.commit_files(commit).and_then(|files| files.get(i)) else { return };
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
        let room = preview.min(shown - 1);
        self.top = self.top.max((self.cursor + room + 1).saturating_sub(shown));
    }

    fn left(&mut self) {
        match (self.selected(), self.open) {
            (Row::File(_) | Row::More, Some(open)) => {
                self.cursor = self.rows.iter().position(|&row| row == Row::Commit(open)).unwrap_or(self.cursor);
            }
            (Row::Commit(i), Some(open)) if i == open => self.toggle_files(i),
            _ => {}
        }
    }

    fn click(&mut self, x: usize, y: usize, now: Instant) {
        let Some(painted) = &self.painted else { return };
        let Some(offset) = y.checked_sub(painted.body_top).filter(|&offset| offset < self.shown) else {
            self.clicked = None;
            return;
        };
        let at = self.top + offset;
        let Some(&row) = self.rows.get(at).filter(|row| x < painted.width && row.selectable()) else {
            self.clicked = None;
            return;
        };
        let again = self.clicked.is_some_and(|(r, at)| r == row && now.duration_since(at) < DOUBLE_CLICK);
        self.cursor = at;
        self.clicked = Some((row, now));
        match (row, again) {
            (Row::Commit(i), false) => {
                self.toggle_files(i);
                self.top = self.cursor.saturating_sub(offset);
            }
            (Row::File(i), true) => {
                self.clicked = None;
                self.read_file(i);
            }
            (Row::More, _) => {
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

    fn keep_cursor_in_view(&mut self) {
        // Do not bring the top back under the pointer after a click.
        if self.clicked.is_none() && self.selected() == Row::Commit(0) && self.cursor < self.shown {
            self.top = 0;
        } else if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + self.shown {
            self.top = self.cursor + 1 - self.shown;
        }
        self.top = self.top.min(self.rows.len().saturating_sub(self.shown));
    }

    fn draw_heading(&self, s: &mut Screen) {
        let branch = self.repo.branch();
        let name_width = str_width(&self.repo.name) + str_width(branch) + 5;
        let summary = self.draw_summary(s, name_width);
        let room = s.w.saturating_sub(summary + 3);
        let branch_room = str_width(branch).min(room / 2);
        let skip = if branch_room > 0 { 2 } else { 0 };
        let name = s.put_fit(1, 0, room.saturating_sub(branch_room + skip), &self.repo.name, Style::PLAIN.bold());
        s.put_fit(1 + name + skip, 0, branch_room, branch, Style::fg(screen::MAGENTA));
    }

    fn draw_summary(&self, s: &mut Screen, name_width: usize) -> usize {
        let right = s.w - 1;
        if !self.repo.ok() {
            let text = if name_width + 18 <= s.w { "git did not answer" } else { "! git" };
            return s.put_right(right, 0, text, ui::DEL);
        }
        if !self.has_remote {
            return s.put_right(right, 0, "no remote", FAINT);
        }
        let Some(ahead) = self.ahead else {
            let text = if right >= 14 { "not pushed yet" } else { "not pushed" };
            return s.put_right(right, 0, text, FAINT);
        };
        if ahead == 0 {
            let text = if name_width + 12 <= s.w { "✓ all pushed" } else { "✓" };
            return s.put_right(right, 0, text, ui::ADD);
        }
        let digits = digits(ahead);
        let words = if ahead == 1 { [" commit to push", " commit ↑"] } else { [" commits to push", " commits ↑"] };
        if let Some(word) = words.into_iter().find(|word| name_width + digits + str_width(word) <= s.w) {
            count_right(s, 0, ahead, word, Style::PLAIN)
        } else {
            s.put_number(right, 0, ahead, Style::PLAIN);
            s.put(right.saturating_sub(digits + 1), 0, 1, "↑", Style::PLAIN);
            digits + 1
        }
    }

    /// Shows the shape of recent history: a gap between days, a tick at the remote.
    fn draw_dots(&self, s: &mut Screen) {
        let pairs = || self.entries.windows(2).map(|pair| (&pair[0].commit, &pair[1].commit));
        let days = pairs().filter(|(a, b)| a.day != b.day).count();
        let ticks = pairs().filter(|(a, b)| crosses_remote(a, b)).count();
        let cells = self.entries.len() + ticks + usize::from(self.uncommitted > 0);
        let right = s.w - 1;
        let step = if cells * 2 + days < right { 2 } else { 1 };
        let mut x = 1;
        if self.uncommitted > 0 {
            s.put(x, 1, 1, "○", Style::PLAIN.bold());
            x += step;
        }
        let mut before = None;
        for entry in &self.entries {
            let commit = &entry.commit;
            if let Some(prev) = before {
                if crosses_remote(prev, commit) && x < right {
                    s.put(x, 1, 1, "│", FAINT);
                    x += step;
                }
                x += usize::from(prev.day != commit.day);
            }
            if x >= right {
                break;
            }
            s.put(x, 1, 1, "●", self.dot_style(commit));
            x += step;
            before = Some(commit);
        }
    }

    fn dot_style(&self, c: &Commit) -> Style {
        match (self.repo.remote(), c.unpushed) {
            (None, _) => Style::PLAIN,
            (Some(_), false) => FAINT,
            (Some(_), true) => Style::PLAIN.bold(),
        }
    }

    fn draw_divider(&self, s: &mut Screen, y: usize) {
        let right = s.w - 1;
        let x = 1 + s.put(1, y, right - 1, "── on ", FAINT);
        let target = self.repo.remote().unwrap_or_default();
        let end = x + s.put_fit(x, y, right - x - 3, target, FAINT);
        for x in end + 1..right {
            s.put(x, y, 1, "─", FAINT);
        }
    }

    fn draw_body(&self, s: &mut Screen, body_top: usize) {
        let mut ages = Ages::default();
        let open_files = self.open_files();
        let files = open_files.unwrap_or_default();
        let right = s.w - 1;
        for (i, row) in self.rows.iter().enumerate().skip(self.top).take(self.shown) {
            let y = body_top + i - self.top;
            match *row {
                Row::Uncommitted => self.draw_uncommitted(s, y),
                Row::Divider => self.draw_divider(s, y),
                Row::Commit(index) => {
                    let end = if self.open == Some(index) {
                        open_files.map_or(right, |files| file_count(s, y, files.len()))
                    } else {
                        ages.draw(s, y, self.entries[index].commit.time)
                    };
                    self.draw_commit(s, y, end, index);
                }
                Row::File(index) => draw_file(s, y, &files[index]),
                Row::More => {
                    draw_more(s, y, files.len() - FILE_PREVIEW);
                }
                Row::Note(text) => {
                    s.put_fit(3, y, right - 3, text, FAINT);
                }
            }
            if i == self.cursor && row.selectable() {
                s.wash(0, y, right, Wash::Bar);
            }
        }
        ui::thumb(s, right, body_top, self.shown, self.rows.len(), self.top);
    }

    fn draw_uncommitted(&self, s: &mut Screen, y: usize) {
        let word = if self.uncommitted == 1 { "file" } else { "files" };
        let bold = Style::PLAIN.bold();
        s.put(1, y, 1, "○", bold);
        s.put_fit(3, y, s.w - 4, &format!("{} {word} to commit", self.uncommitted), bold);
    }

    fn draw_commit(&self, s: &mut Screen, y: usize, end: usize, i: usize) {
        let Entry { commit, fresh } = &self.entries[i];
        let bold = Style::PLAIN.bold();
        if *fresh {
            s.put(0, y, 1, "•", bold);
        }
        s.put(1, y, 1, "●", self.dot_style(commit));
        if self.open == Some(i) {
            s.put(2, y, 1, "▾", FAINT);
        }
        let quiet = commit.merge || (!commit.unpushed && self.repo.remote().is_some());
        let style = if *fresh {
            bold
        } else if quiet {
            FAINT
        } else {
            Style::PLAIN
        };
        let split = prefix_len(&commit.subject);
        let room = end.saturating_sub(3);
        let used = s.put(3, y, room, &commit.subject[..split], FAINT);
        s.put_fit(3 + used, y, room - used, &commit.subject[split..], style);
    }

    fn draw_hint(&self, s: &mut Screen) {
        if let Some(error) = self.read_error {
            s.put_fit(1, s.h - 1, s.w - 2, error, FAINT);
            return;
        }
        let action = match self.selected() {
            Row::Commit(i) if self.open == Some(i) => "Enter hide files",
            Row::Commit(_) => "Enter show files",
            Row::File(_) => "Enter read",
            Row::More => "Enter show all",
            _ => "",
        };
        let middle: &[_] =
            if self.entries.is_empty() { &["r reload", "q quit"] } else { &["↑↓ move", "r reload", "q quit"] };
        help::draw_hint(s, action, middle, "? help");
    }

    fn input(&mut self, key: Key, now: Instant) {
        if matches!(key, Key::Focus(_)) {
            return;
        }
        if !matches!(key, Key::Click(..)) {
            self.clicked = None;
        }
        self.read_error = None;
        match key {
            Key::Click(x, y) => self.click(x, y, now),
            Key::Enter => self.enter(),
            Key::Left | Key::Char('h') => self.left(),
            Key::Char('r') => self.load(),
            _ => {
                if let Some(delta) = key.rows(self.shown) {
                    self.scroll(delta);
                    if matches!(key, Key::WheelUp | Key::WheelDown) {
                        self.top = self.top.saturating_add_signed(delta);
                        self.painted = None;
                    }
                }
            }
        }
    }
}

/// Leaves the age blank when it would repeat the one above.
#[derive(Default)]
struct Ages {
    now: Option<i64>,
    time: Option<i64>,
    shown: String,
}

impl Ages {
    fn draw(&mut self, s: &mut Screen, y: usize, time: i64) -> usize {
        let right = s.w - 1;
        if s.w < 24 || self.time == Some(time) {
            return right;
        }
        self.time = Some(time);
        let age = when::short_age(self.now.get_or_insert_with(when::now).saturating_sub(time));
        if age == self.shown {
            return right;
        }
        let used = s.put_right(right, y, &age, FAINT);
        self.shown = age;
        right - used - 1
    }
}

impl Pane for History {
    fn help(&self) -> Page {
        self.reading.as_ref().map_or_else(Page::history, Pane::help)
    }

    fn watch_scope(&self) -> Scope {
        self.repo.watch_scope()
    }

    fn check(&mut self) -> bool {
        if !self.repo.git || self.reading.is_some() {
            return true;
        }
        if self.repo.busy() {
            return false;
        }
        if self.repo.poll(&[]) {
            self.load();
        } else if self.repo.ok() {
            self.refresh_remote();
        }
        true
    }

    fn key(&mut self, key: Key) -> bool {
        if let Some(reader) = &mut self.reading {
            if reader.on_key(key) {
                self.reading = None;
            }
            return false;
        }
        self.input(key, Instant::now());
        false
    }

    fn close(&mut self) -> bool {
        self.reading.take().is_none()
    }

    fn draw(&mut self, s: &mut Screen) {
        self.painted = None;
        if let Some(reader) = &mut self.reading {
            reader.draw(s);
            return;
        }
        s.clear();
        if s.w < 12 || s.h == 0 {
            return;
        }
        if !self.repo.git {
            s.put_fit(1, 0, s.w - 2, "This folder is not a git project.", FAINT);
            return;
        }
        let compact = s.h < HEADING_ROWS;
        if self.compact != compact {
            self.compact = compact;
            self.build_rows(self.selected());
        }
        let dots = s.h >= DOT_ROWS && s.w >= DOT_COLS && self.entries.len() > 1;
        let hint = s.h >= HINT_ROWS || (s.h > 1 && self.read_error.is_some());
        let body_top = usize::from(!compact) + usize::from(dots);
        self.shown = s.h - body_top - usize::from(hint);
        self.keep_cursor_in_view();
        if !compact {
            self.draw_heading(s);
        }
        if dots {
            self.draw_dots(s);
        }
        self.draw_body(s, body_top);
        self.painted = Some(Painted { width: s.w, body_top });
        if hint {
            self.draw_hint(s);
        }
    }
}

fn count_right(s: &mut Screen, y: usize, count: u32, suffix: &str, style: Style) -> usize {
    let right = s.w - 1;
    let used = s.put_right(right, y, suffix, style);
    s.put_number(right - used, y, count, style);
    digits(count) + used
}

fn digits(count: u32) -> usize {
    count.checked_ilog10().unwrap_or(0) as usize + 1
}

fn draw_more(s: &mut Screen, y: usize, count: usize) {
    let Ok(count) = u32::try_from(count) else { return };
    let right = s.w - 1;
    let start = 5 + s.put(5, y, 2, "… ", FAINT);
    let end = start + digits(count);
    if end <= right {
        s.put_number(end, y, count, FAINT);
        s.put_fit(end, y, right - end, " more", FAINT);
    }
}

fn file_count(s: &mut Screen, y: usize, files: usize) -> usize {
    let right = s.w - 1;
    let Ok(count) = u32::try_from(files) else { return right };
    let suffix = if count == 1 { " file" } else { " files" };
    let width = digits(count) + suffix.len();
    if width + 5 > right {
        return right;
    }
    right - count_right(s, y, count, suffix, FAINT) - 1
}

fn draw_file(s: &mut Screen, y: usize, file: &PendingFile) {
    let right = s.w - 1;
    let size = ui::size_width(file.add, file.del);
    let word = ui::kind_word(file.kind);
    let left = 5;
    let details = size + usize::from(size > 0) + word.len();
    // Keep enough room for the change details even when the name must be clipped.
    let name_end = if left + 4 + details <= right {
        ui::size_right(s, right, y, file.add, file.del);
        let end = right - size - usize::from(size > 0);
        s.put_right(end, y, word, ui::kind_colour(file.kind));
        end - word.len() - 1
    } else {
        right
    };
    s.put(3, y, 1, "│", FAINT);
    ui::file_name(s, left, y, name_end - left, file);
}

/// Adjacent commits on different sides of the comparison branch need a divider.
fn crosses_remote(before: &Commit, c: &Commit) -> bool {
    before.unpushed && !c.unpushed
}

/// A leading `scope: ` is dimmed so the rest of the subject stands out.
fn prefix_len(subject: &str) -> usize {
    match subject.split_once(": ") {
        Some((scope, _)) if scope.len() <= 24 && !scope.contains(' ') => scope.len() + 2,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn open(f: &Fixture) -> History {
        History::new(Repo::open(&f.root).unwrap(), false)
    }

    fn draw(history: &mut History, w: usize, h: usize) -> Screen {
        let mut screen = Screen::sized(w, h);
        history.draw(&mut screen);
        screen
    }

    fn row<'a>(rows: &'a [String], text: &str) -> &'a str {
        rows.iter().find(|row| row.contains(text)).expect(text)
    }

    fn commit(f: &Fixture, subject: &str) {
        f.git(&["commit", "-q", "--allow-empty", "-m", subject]);
    }

    fn active_row(screen: &Screen, name: &str) -> bool {
        screen.text().iter().enumerate().any(|(y, row)| row.contains(name) && screen.bar_at(y))
    }

    fn read_next(history: &mut History, name: &str) -> Vec<String> {
        assert!(!history.on_key(Key::Down));
        assert!(active_row(&draw(history, 65, 12), name), "{name} is not selected");
        assert!(!history.on_key(Key::Enter));
        draw(history, 65, 12).text()
    }

    #[test]
    fn a_scope_prefix_is_one_short_word() {
        assert_eq!(prefix_len("fix: off by one"), 5);
        assert_eq!(prefix_len("feat(ui): tabs"), 10);
        assert_eq!(prefix_len("Merge branch: not a scope"), 0);
        assert_eq!(prefix_len("No prefix here"), 0);
    }

    #[test]
    fn the_heading_names_the_project_branch_and_commits_to_push() {
        let f = Fixture::new();
        commit(&f, "first");
        let mut history = open(&f);
        let heading = &draw(&mut history, 65, 8).text()[0];
        assert!(heading.starts_with(" project  main") && heading.ends_with("no remote"), "{heading}");
        assert_eq!(heading, &format!(" project  main{}no remote", " ".repeat(41)));
        f.push();
        assert!(history.check());
        let heading = &draw(&mut history, 65, 8).text()[0];
        assert!(heading.ends_with("✓ all pushed"), "{heading}");
        commit(&f, "second");
        assert!(history.check());
        let heading = &draw(&mut history, 65, 8).text()[0];
        assert!(heading.ends_with("1 commit to push"), "{heading}");
        commit(&f, "third");
        assert!(history.check());
        let heading = &draw(&mut history, 65, 8).text()[0];
        assert!(heading.ends_with("2 commits to push"), "{heading}");
    }

    #[test]
    fn an_all_pushed_heading_uses_a_check_and_a_narrow_count_still_names_commits() {
        let f = Fixture::new();
        commit(&f, "first");
        f.push();
        let mut history = open(&f);
        assert_eq!(draw(&mut history, 65, 8).text()[0], format!(" project  main{}✓ all pushed", " ".repeat(38)));
        assert_eq!(draw(&mut history, 22, 8).text()[0], " project  main      ✓");
        for i in 1..=3 {
            commit(&f, &format!("local {i}"));
        }
        assert!(history.check());
        assert_eq!(draw(&mut history, 65, 8).text()[0], format!(" project  main{}3 commits to push", " ".repeat(33)));
        assert_eq!(draw(&mut history, 30, 8).text()[0], " project  main    3 commits ↑");
        assert_eq!(draw(&mut history, 22, 8).text()[0], " project  main     ↑3");
    }

    #[test]
    fn an_unpushed_branch_names_its_state_when_a_remote_exists() {
        let f = Fixture::new();
        commit(&f, "first");
        f.git(&["switch", "-q", "-c", "feature"]);
        commit(&f, "feature work");
        let mut history = open(&f);
        assert!(draw(&mut history, 50, 12).text()[0].ends_with("no remote"));
        f.git(&["remote", "add", "backup", "../missing"]);
        assert!(history.check());
        let heading = &draw(&mut history, 50, 12).text()[0];
        assert!(heading.starts_with(" project  feature") && heading.ends_with("not pushed yet"), "{heading}");
        assert_eq!(heading, &format!(" project  feature{}not pushed yet", " ".repeat(18)));
        f.git(&["remote", "remove", "backup"]);
        assert!(history.check());
        assert!(draw(&mut history, 50, 12).text()[0].ends_with("no remote"));
        f.push();
        f.git(&["remote", "set-head", "origin", "main"]);
        let mut history = open(&f);
        assert!(draw(&mut history, 50, 12).text()[0].ends_with("not pushed yet"));
        f.git(&["push", "-q", "-u", "origin", "feature"]);
        assert!(history.check());
        assert!(draw(&mut history, 50, 12).text()[0].ends_with("✓ all pushed"));
    }

    #[test]
    fn uncommitted_then_unpushed_then_the_remote_boundary() {
        let f = Fixture::new();
        commit(&f, "oldest");
        f.push();
        commit(&f, "middle");
        commit(&f, "newest");
        f.write("one", "uncommitted\n");
        f.write("two", "uncommitted\n");
        let mut history = open(&f);
        let rows = draw(&mut history, 65, 12).text();
        assert_eq!(rows[1], " ○ ● ● │ ●");
        assert_eq!(rows[2], " ○ 2 files to commit");
        assert!(rows[3].starts_with(" ● newest"), "{rows:?}");
        assert_eq!(rows[4], " ● middle");
        assert_eq!(rows[5], format!(" ── on origin/main {}", "─".repeat(45)));
        assert_eq!(rows[6], " ● oldest");
        assert!(rows[11].contains("Enter show files") && rows[11].contains("? help"));
    }

    #[test]
    fn the_remote_boundary_is_a_rule_and_does_not_take_the_cursor() {
        let f = Fixture::new();
        commit(&f, "on main");
        f.push();
        commit(&f, "still here");
        let mut history = open(&f);
        let screen = draw(&mut history, 30, 12);
        assert_eq!(screen.text()[2], " ── on origin/main ──────────");
        history.on_key(Key::Down);
        assert!(active_row(&draw(&mut history, 30, 12), "on main"));
    }

    #[test]
    fn clicks_toggle_commits_and_only_a_quick_repeat_reads_the_same_file() {
        let f = Fixture::new();
        for i in 0..6 {
            commit(&f, &format!("older {i}"));
        }
        for name in ["a", "b", "c"] {
            f.write(&format!("{name}.txt"), &format!("stored {name}\n"));
        }
        f.commit("write three files");
        let mut history = open(&f);
        draw(&mut history, 40, 5);
        history.on_key(Key::Click(5, 1));
        let screen = draw(&mut history, 40, 5);
        assert!(active_row(&screen, "write three files") && screen.text()[1].contains("▾"));
        assert!(screen.text()[4].contains("c.txt"));

        let now = Instant::now();
        let step = std::time::Duration::from_millis(1);
        history.input(Key::Click(5, 3), now);
        history.input(Key::Click(5, 4), now + step);
        let selected = draw(&mut history, 40, 5);
        assert!(selected.text()[4].contains("c.txt") && selected.bar_at(4));
        history.input(Key::Click(5, 4), now + step + DOUBLE_CLICK);
        assert!(active_row(&draw(&mut history, 40, 5), "c.txt"));
        history.input(Key::Click(5, 4), now + 2 * step + DOUBLE_CLICK);
        assert_eq!(draw(&mut history, 40, 5).text()[1], " 1 + stored c");

        history.on_key(Key::Esc);
        let before = draw(&mut history, 40, 5).text();
        history.on_key(Key::WheelDown);
        let after = draw(&mut history, 40, 5);
        assert_ne!(after.text(), before);
        assert!(active_row(&after, "older 3"));
        history.on_key(Key::Home);
        draw(&mut history, 40, 5);
        history.on_key(Key::Click(5, 1));
        let closed = draw(&mut history, 40, 5);
        assert!(active_row(&closed, "write three files"));
        assert!(!closed.text().iter().any(|row| row.contains("c.txt")));
        history.on_key(Key::WheelDown);
        let scrolled = draw(&mut history, 40, 5);
        assert_ne!(scrolled.text()[1], closed.text()[1]);
        assert!(active_row(&scrolled, "older 3"));
    }

    #[test]
    fn opening_a_long_commit_in_a_narrow_pane_keeps_the_subject_and_previews_eight_names() {
        let f = Fixture::new();
        commit(&f, "older commit");
        f.push();
        for i in 0..40 {
            f.write(&format!("long/shared/folder/path/page{i:02}.txt"), "a file\n");
        }
        f.commit("touch forty files");
        for i in 1..=8 {
            commit(&f, &format!("newer {i}"));
        }
        let mut history = open(&f);
        draw(&mut history, 30, 12);
        history.on_key(Key::PgDn);
        assert!(active_row(&draw(&mut history, 30, 12), "older commit"));
        history.on_key(Key::Up);
        history.on_key(Key::Enter);
        let preview = draw(&mut history, 30, 12);
        let rows = preview.text();
        assert!(
            preview.bar_at(1) && rows[1].contains("touch forty files") && rows[1].ends_with("40 files"),
            "{rows:?}"
        );
        for (i, row) in rows[2..10].iter().enumerate() {
            assert!(
                row.contains(&format!("page{i:02}.txt lon")) && row.trim_end_matches('▐').ends_with("new +1"),
                "{rows:?}"
            );
        }
        assert!(rows[10].contains("… 32 more"), "{rows:?}");
        assert_eq!(rows[11], " Enter hide files · ? help");

        history.on_key(Key::End);
        history.on_key(Key::Up);
        let more = draw(&mut history, 30, 12);
        assert!(active_row(&more, "… 32 more"));
        assert_eq!(more.text()[11], " Enter show all · ? help");
        history.on_key(Key::Enter);
        assert!(active_row(&draw(&mut history, 30, 12), "page08.txt"));
        history.on_key(Key::End);
        history.on_key(Key::Up);
        assert!(active_row(&draw(&mut history, 30, 12), "page39.txt"));
        commit(&f, "a commit arrived");
        assert!(history.check());
        assert!(active_row(&draw(&mut history, 30, 12), "page39.txt"));
        history.on_key(Key::Left);
        history.on_key(Key::Enter);
        history.on_key(Key::Enter);
        assert!(draw(&mut history, 30, 12).text()[10].contains("… 32 more"));
    }

    #[test]
    fn enter_hints_follow_the_selected_row_and_disappear_where_enter_does_nothing() {
        let f = Fixture::new();
        let mut history = open(&f);
        let rows = draw(&mut history, 65, 12).text();
        assert!(rows[1].contains("No commits yet.") && !rows[11].contains("Enter"));
        f.write("notes.txt", "some notes\n");
        assert!(history.check());
        let uncommitted = draw(&mut history, 65, 12).text();
        assert!(uncommitted[1].contains("1 file to commit") && !uncommitted[11].contains("Enter"));
        history.on_key(Key::Enter);
        assert_eq!(draw(&mut history, 65, 12).text(), uncommitted);
        f.commit("add notes");
        assert!(history.check());
        assert_eq!(draw(&mut history, 30, 12).text()[11], " Enter show files · ? help");
        history.on_key(Key::Enter);
        assert_eq!(draw(&mut history, 30, 12).text()[11], " Enter hide files · ? help");
        history.on_key(Key::Down);
        assert_eq!(draw(&mut history, 30, 12).text()[11], " Enter read · q quit · ? help");
    }

    #[test]
    fn enter_shows_the_files_and_counts_and_then_hides_them() {
        let f = Fixture::new();
        f.write("edit.txt", "old\n");
        f.write("remove.txt", "delete\nme\n");
        f.write("move.txt", "move\n");
        f.commit("original");
        f.write("add.txt", "a\nb\nc\n");
        f.write("edit.txt", "new\nsome more\n");
        std::fs::remove_file(f.root.join("remove.txt")).unwrap();
        std::fs::rename(f.root.join("move.txt"), f.root.join("moved.txt")).unwrap();
        f.commit("touch four files");
        let mut history = open(&f);
        let before = draw(&mut history, 65, 12).text();
        history.on_key(Key::Enter);
        let rows = draw(&mut history, 65, 12).text();
        assert!(row(&rows, "touch four files").contains("▾"));
        assert!(row(&rows, "add.txt").contains("new") && row(&rows, "add.txt").ends_with("+3"), "{rows:?}");
        assert!(row(&rows, "edit.txt").contains("edited") && row(&rows, "edit.txt").ends_with("+2 −1"), "{rows:?}");
        assert!(row(&rows, "moved.txt").ends_with("moved"), "{rows:?}");
        assert!(row(&rows, "remove.txt").contains("removed") && row(&rows, "remove.txt").ends_with("−2"), "{rows:?}");
        history.on_key(Key::Enter);
        assert_eq!(draw(&mut history, 65, 12).text(), before);
        history.on_key(Key::Enter);
        assert_eq!(draw(&mut history, 65, 12).text(), rows);
        history.on_key(Key::End);
        history.on_key(Key::Enter);
        let root = draw(&mut history, 65, 12).text();
        assert!(row(&root, "remove.txt").contains("new") && row(&root, "remove.txt").ends_with("+2"), "{root:?}");
    }

    #[test]
    fn reading_a_file_shows_its_commit_and_returns_to_the_same_row() {
        let f = Fixture::new();
        f.write("page.rs", "fn main() {\n    let n = 1;\n}\n");
        f.commit("original");
        f.write("page.rs", "fn main() {\n    let n = 2;\n}\n");
        f.commit("change the number");
        f.write("page.rs", "uncommitted work\n");

        let mut history = open(&f);
        let id = format!("commit {}", &history.entries[0].commit.hash[..7]);
        history.on_key(Key::Enter);
        let rows = read_next(&mut history, "page.rs");
        assert!(rows[0].contains("page.rs") && rows[0].contains(&id) && rows[0].ends_with("ago"), "{rows:?}");
        assert_eq!(&rows[1..5], [" 1   fn main() {", "   -     let n = 1;", " 2 +     let n = 2;", " 3   }"]);
        assert!(!rows.iter().any(|row| row.contains("uncommitted work")));
        assert!(rows[11].contains("n N changes") && !rows[11].contains("reload"));
        let mut help = Screen::sized(40, 20);
        history.help().draw(&mut help);
        let help = help.text();
        assert!(help.iter().any(|row| row.contains("As of a commit")));
        assert!(!help.iter().any(|row| row.contains("reload")));

        f.write("page.rs", "another edit\n");
        history.on_key(Key::Char('r'));
        assert!(history.check());
        assert_eq!(draw(&mut history, 65, 12).text(), rows);

        assert!(!history.on_key(Key::Char('q')));
        let screen = draw(&mut history, 65, 12);
        assert!(active_row(&screen, "page.rs"));
        assert!(row(&screen.text(), "change the number").contains("▾"));
        assert!(screen.text()[11].contains("Enter read"));
        history.on_key(Key::Enter);
        assert!(!history.on_key(Key::Esc));
        assert!(active_row(&draw(&mut history, 65, 12), "page.rs"));

        history.on_key(Key::Left);
        let screen = draw(&mut history, 65, 12);
        assert!(active_row(&screen, "change the number"));
        assert!(row(&screen.text(), "change the number").contains("▾"));
        assert!(screen.text()[11].contains("Enter hide files"));
        history.on_key(Key::Left);
        let rows = draw(&mut history, 65, 12).text();
        assert!(!row(&rows, "change the number").contains("▾"));
        assert!(!rows.iter().any(|row| row.contains("page.rs")));
    }

    #[test]
    fn a_removed_file_and_moves_read_from_the_right_paths() {
        let f = Fixture::new();
        let start = "first retained line\nsecond retained line\nthird retained line\n";
        f.write("gone.txt", "a lost line\n");
        f.write("old.txt", &format!("{start}before\n"));
        f.write("unchanged.txt", "an unchanged line\n");
        f.commit("original");
        std::fs::remove_file(f.root.join("gone.txt")).unwrap();
        std::fs::rename(f.root.join("old.txt"), f.root.join("moved.txt")).unwrap();
        std::fs::rename(f.root.join("unchanged.txt"), f.root.join("same.txt")).unwrap();
        f.write("moved.txt", &format!("{start}after\n"));
        f.commit("moves and a removal");
        f.write("gone.txt", "a new file with the same name\n");
        f.write("same.txt", "edited after the move\n");

        let mut history = open(&f);
        history.on_key(Key::Enter);
        let rows = read_next(&mut history, "gone.txt");
        assert_eq!(rows[1], "   - a lost line");
        assert!(!rows.iter().any(|row| row.contains("a new file")));
        history.on_key(Key::Esc);
        let rows = read_next(&mut history, "moved.txt");
        assert_eq!(
            &rows[1..6],
            [
                " 1   first retained line",
                " 2   second retained line",
                " 3   third retained line",
                "   - before",
                " 4 + after"
            ]
        );
        history.on_key(Key::Esc);
        let rows = read_next(&mut history, "same.txt");
        assert_eq!(rows[1], " 1   an unchanged line");
        assert!(!rows.iter().any(|row| row.contains("edited after the move")));
    }

    #[test]
    fn a_file_in_the_first_commit_has_only_additions() {
        let f = Fixture::new();
        f.write("first.txt", "first line\nsecond line\n");
        f.commit("start");
        let mut history = open(&f);
        history.on_key(Key::Enter);
        let rows = read_next(&mut history, "first.txt");
        assert_eq!(&rows[1..3], [" 1 + first line", " 2 + second line"]);
    }

    #[test]
    fn opening_a_merge_at_the_bottom_shows_its_first_file() {
        let f = Fixture::new();
        commit(&f, "base");
        f.git(&["branch", "topic"]);
        f.write("main.txt", "main\n");
        f.commit("main work");
        f.git(&["switch", "-q", "topic"]);
        f.write("topic.txt", "topic\n");
        f.commit("topic work");
        f.git(&["switch", "-q", "main"]);
        f.git(&["merge", "-q", "--no-ff", "-m", "merge topic", "topic"]);
        commit(&f, "later");
        let mut history = open(&f);
        draw(&mut history, 40, 2);
        history.on_key(Key::Down);
        history.on_key(Key::Enter);
        let rows = draw(&mut history, 40, 2).text();
        assert!(rows[0].contains("merge topic"), "{rows:?}");
        assert!(rows[1].contains("topic.txt") && rows[1].ends_with("+1"), "{rows:?}");

        let rows = read_next(&mut history, "topic.txt");
        assert_eq!(rows[1], " 1 + topic");
        assert!(!rows.iter().any(|row| row.contains("main work")));
    }

    #[test]
    fn scrolling_keeps_the_cursor_on_screen() {
        let f = Fixture::new();
        for i in 1..=8 {
            commit(&f, &format!("commit {i}"));
        }
        let mut history = open(&f);
        for (key, subject) in [
            (Key::Home, "commit 8"),
            (Key::PgDn, "commit 6"),
            (Key::PgDn, "commit 4"),
            (Key::WheelDown, "commit 1"),
            (Key::Up, "commit 2"),
            (Key::Home, "commit 8"),
        ] {
            draw(&mut history, 40, 4);
            history.on_key(key);
            let screen = draw(&mut history, 40, 4);
            let rows = screen.text();
            let y = rows.iter().position(|row| row.contains(subject)).expect(subject);
            assert!(screen.bar_at(y), "{rows:?}");
        }
    }

    #[test]
    fn a_commit_that_lands_while_open_appears_marked_on_check() {
        let f = Fixture::new();
        let mut history = open(&f);
        assert!(draw(&mut history, 65, 8).text().iter().any(|row| row.contains("No commits yet.")));
        commit(&f, "arrived");
        assert!(history.check());
        let screen = draw(&mut history, 65, 8);
        let rows = screen.text();
        let y = rows.iter().position(|row| row.contains("arrived")).unwrap();
        assert!(rows[y].starts_with("•● arrived"), "{rows:?}");
        assert!(screen.bar_at(y));
    }

    #[test]
    fn short_and_narrow_panes_spend_their_space_on_commits() {
        let f = Fixture::new();
        commit(&f, "older");
        f.push();
        commit(&f, "newer");
        f.write("uncommitted", "uncommitted\n");
        let mut history = open(&f);
        let short = draw(&mut history, 65, 2).text();
        assert!(short[0].contains("newer") && short[1].contains("older"), "{short:?}");
        let narrow = draw(&mut history, 22, 12).text();
        assert!(narrow[0].starts_with(" project  main") && narrow[0].ends_with("↑1"), "{narrow:?}");
        assert_eq!(narrow[1], " ○ 1 file to commit");
        assert!(narrow[2].contains("newer"));
    }

    #[test]
    fn a_plain_folder_says_it_is_not_a_git_project() {
        let f = Fixture::folder();
        f.write("page.txt", "a file\n");
        let mut history = open(&f);
        assert!(history.check());
        assert_eq!(draw(&mut history, 40, 3).text(), [" This folder is not a git project.", "", ""]);
    }
}
