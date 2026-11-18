//! Shows recent commits and the files they touched in a pane of their own.

use std::collections::HashMap;

use crate::git::{Commit, Ends, PendingFile, Repo};
use crate::help::Page;
use crate::keys::Key;
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
    Note(&'static str),
}

impl Row {
    fn selectable(self) -> bool {
        matches!(self, Self::Commit(_) | Self::File(_))
    }
}

pub struct History {
    repo: Repo,
    ends: Option<Ends>,
    entries: Vec<Entry>,
    ahead: u32,
    uncommitted: usize,
    /// The one commit whose files are shown, as an index into `entries`.
    open: Option<usize>,
    rows: Vec<Row>,
    cursor: usize,
    top: usize,
    shown: usize,
    compact: bool,
}

impl History {
    pub fn new(repo: Repo) -> Self {
        let mut history = Self {
            repo,
            ends: None,
            entries: Vec::new(),
            ahead: 0,
            uncommitted: 0,
            open: None,
            rows: Vec::new(),
            cursor: 0,
            top: 0,
            shown: 0,
            compact: false,
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
        let selected = self.selected();
        let uncommitted = self.repo.uncommitted_count();
        let ahead_changed = self.repo.upstream_ahead().unwrap_or(self.ahead) != self.ahead
            || (self.repo.remote().is_none() && self.ahead != 0);
        if self.ends.as_ref() != Some(self.repo.ends()) || ahead_changed {
            let Some(commits) = self.repo.history(LIMIT) else { return };
            let Some(ahead) = self.repo.commits_to_push() else { return };
            self.ahead = ahead;
            self.replace_commits(commits, selected, uncommitted);
        } else if self.uncommitted != uncommitted {
            self.uncommitted = uncommitted;
            self.build_rows(selected);
        }
    }

    /// Remembers the open commit and the cursor by hash when commits move.
    fn replace_commits(&mut self, commits: Vec<Commit>, selected: Row, uncommitted: usize) {
        let remap = |i: usize| commits.iter().position(|c| c.hash == self.entries[i].commit.hash);
        let open = self.open.and_then(remap);
        let selected = match selected {
            // At the newest commit, follow new arrivals unless its files are open.
            Row::Commit(0) if self.open != Some(0) => Row::Commit(0),
            Row::Commit(i) => Row::Commit(remap(i).unwrap_or(0)),
            Row::File(_) if open.is_some() => selected,
            _ => Row::Commit(0),
        };
        let before: HashMap<_, _> = self.entries.iter().map(|e| (e.commit.hash.as_str(), e.fresh)).collect();
        let loaded = self.ends.is_some();
        self.entries = commits
            .into_iter()
            .map(|commit| Entry { fresh: before.get(commit.hash.as_str()).copied().unwrap_or(loaded), commit })
            .collect();
        self.ends = Some(self.repo.ends().clone());
        self.open = open;
        self.uncommitted = uncommitted;
        self.build_rows(selected);
    }

    fn selected(&self) -> Row {
        self.rows.get(self.cursor).copied().unwrap_or(Row::Commit(0))
    }

    fn open_files(&self) -> Option<&[PendingFile]> {
        self.repo.commit_files(&self.entries.get(self.open?)?.commit)
    }

    fn build_rows(&mut self, selected: Row) {
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
                    Some(n) => self.rows.extend((0..n).map(Row::File)),
                    None => self.rows.push(Row::Note("Git did not answer for this commit.")),
                }
            }
        }
        if self.entries.is_empty() {
            self.rows.push(Row::Note("No commits yet."));
        }
        self.cursor = self.rows.iter().position(|&row| row == selected).unwrap_or(0);
    }

    fn toggle_files(&mut self) {
        let i = match self.selected() {
            Row::Commit(i) => i,
            Row::File(_) => self.open.expect("file rows belong to an open commit"),
            _ => return,
        };
        self.open = if self.open == Some(i) {
            None
        } else {
            self.repo.load_commit_files(&self.entries[i].commit);
            Some(i)
        };
        self.build_rows(Row::Commit(i));
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
        // Keep the first file in view as well when an opened commit is at the edge.
        let selected = self.selected();
        let room_for_file = self.shown > 1 && self.open.is_some_and(|i| selected == Row::Commit(i));
        let last = self.cursor + usize::from(room_for_file);
        if selected == Row::Commit(0) && last < self.shown {
            self.top = 0;
        } else if self.cursor < self.top {
            self.top = self.cursor;
        } else if last >= self.top + self.shown {
            self.top = last + 1 - self.shown;
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
        if self.repo.remote().is_none() {
            return s.put_right(right, 0, "no remote", FAINT);
        }
        let digits = self.ahead.checked_ilog10().unwrap_or(0) as usize + 1;
        let word = if self.ahead == 1 { " commit to push" } else { " commits to push" };
        if name_width + digits + word.len() <= s.w {
            let used = s.put_right(right, 0, word, Style::PLAIN);
            s.put_number(right - used, 0, self.ahead, Style::PLAIN);
            digits + used
        } else {
            s.put_number(right, 0, self.ahead, Style::PLAIN);
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

    fn draw_body(&self, s: &mut Screen, body_top: usize) {
        let mut ages = Ages::default();
        let files = self.open_files().unwrap_or_default();
        let right = s.w - 1;
        for (i, row) in self.rows.iter().enumerate().skip(self.top).take(self.shown) {
            let y = body_top + i - self.top;
            match *row {
                Row::Uncommitted => self.draw_uncommitted(s, y),
                Row::Divider => {
                    let x = 3 + s.put(3, y, right - 3, "pushed to ", FAINT);
                    s.put_fit(x, y, right - x, self.repo.remote().unwrap_or_default(), FAINT);
                }
                Row::Commit(index) => {
                    let end = ages.draw(s, y, self.entries[index].commit.time);
                    self.draw_commit(s, y, end, index);
                }
                Row::File(index) => draw_file(s, y, &files[index]),
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
        Page::history()
    }

    fn watch_scope(&self) -> Scope {
        self.repo.watch_scope()
    }

    fn check(&mut self) -> bool {
        if self.repo.busy() {
            return false;
        }
        if self.repo.poll(&[]) {
            self.load();
        }
        true
    }

    fn key(&mut self, key: Key) -> bool {
        match key {
            Key::Enter => self.toggle_files(),
            Key::Char('r') => self.load(),
            _ => {
                if let Some(delta) = key.rows(self.shown) {
                    self.scroll(delta);
                }
            }
        }
        false
    }

    fn draw(&mut self, s: &mut Screen) {
        s.clear();
        if s.w < 12 || s.h == 0 {
            return;
        }
        if !self.repo.git {
            s.put_fit(1, 0, s.w - 2, "No git history here.", FAINT);
            return;
        }
        let compact = s.h < HEADING_ROWS;
        if self.compact != compact {
            self.compact = compact;
            self.build_rows(self.selected());
        }
        let dots = s.h >= DOT_ROWS && s.w >= DOT_COLS && self.entries.len() > 1;
        let hint = s.h >= HINT_ROWS;
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
        if hint {
            let text =
                if s.w >= 42 { "↑↓ move · Enter files · r reload · ? help" } else { "Enter files · ? help" };
            s.put_fit(1, s.h - 1, s.w - 2, text, FAINT);
        }
    }
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
    s.put_middle(left, y, name_end - left, &file.path, ui::kind_style(file.kind));
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
        History::new(Repo::open(&f.root).unwrap())
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
        f.push();
        assert!(history.check());
        let heading = &draw(&mut history, 65, 8).text()[0];
        assert!(heading.ends_with("0 commits to push"), "{heading}");
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
        assert_eq!(rows[5], "   pushed to origin/main");
        assert_eq!(rows[6], " ● oldest");
        assert!(rows[11].contains("Enter files") && rows[11].contains("? help"));
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
    fn a_plain_folder_has_no_git_history() {
        let f = Fixture::folder();
        f.write("page.txt", "a file\n");
        let mut history = open(&f);
        assert!(history.check());
        assert_eq!(draw(&mut history, 40, 3).text(), [" No git history here.", "", ""]);
    }
}
