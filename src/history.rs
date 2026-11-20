//! Shows recent commits and the files they touched in a pane of their own.

mod draw;
mod input;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use crate::git::{Commit, HistoryVersion, PendingFile, Repo};
use crate::help::Page;
use crate::keys::Key;
use crate::pane::{After, Pane};
use crate::preferences::{IconMode, Preferences, ProjectMode};
use crate::reader::{OpenReader, Reader};
use crate::screen::Screen;
use crate::watch::Scope;

/// Keep enough history to page back without another git read.
const LIMIT: usize = 300;
/// Show the first few files without losing the commit or the next row.
const FILE_PREVIEW: usize = 8;

struct Entry {
    commit: Commit,
    /// Arrived since this pane first read the history.
    fresh: bool,
    /// File lists stay cached while their commit is in the timeline.
    files: Option<Vec<PendingFile>>,
}

/// Rows the cursor can point at, with the guides between them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TimelineRow {
    Uncommitted,
    Divider,
    Commit(usize),
    File(usize),
    More,
    Note(&'static str),
}

impl TimelineRow {
    fn selectable(self) -> bool {
        matches!(self, Self::Commit(_) | Self::File(_) | Self::More)
    }
}

/// The part of the pane last drawn as a list, where a click can land.
struct Painted {
    width: usize,
    body_top: usize,
    top: usize,
}

struct Detail {
    commit: usize,
    start: usize,
    next: usize,
}

struct OpenCommit {
    index: usize,
    all_files: bool,
}

/// A click anchors its selected row and may be the first of a double click.
#[derive(Clone, Copy)]
struct Click {
    row: TimelineRow,
    offset: usize,
    at: Instant,
}

pub struct History {
    repo: Repo,
    version: Option<HistoryVersion>,
    entries: Vec<Entry>,
    uncommitted: usize,
    open: Option<OpenCommit>,
    rows: Vec<TimelineRow>,
    cursor: usize,
    top: usize,
    shown: usize,
    painted: Option<Painted>,
    clicked: Option<Click>,
    compact: bool,
    focused: bool,
    detail: Option<Detail>,
    follow_new: bool,
    icons: IconMode,
    text_git_marks: bool,
    promotion: Option<(ProjectMode, Option<&'static str>)>,
    opener: OpenReader,
    reading: Option<Reader>,
    read_error: Option<&'static str>,
}

impl History {
    pub fn new(repo: Repo, inside_rex: bool) -> Self {
        let mut history = Self {
            repo: repo.for_history(),
            version: None,
            entries: Vec::new(),
            uncommitted: 0,
            open: None,
            rows: Vec::new(),
            cursor: 0,
            top: 0,
            shown: 0,
            painted: None,
            clicked: None,
            compact: false,
            focused: !inside_rex,
            detail: None,
            follow_new: false,
            icons: if cfg!(test) { IconMode::Plain } else { IconMode::Nerd },
            text_git_marks: false,
            promotion: None,
            opener: OpenReader::new(inside_rex),
            reading: None,
            read_error: None,
        };
        history.load();
        history
    }

    pub fn with_follow_new(mut self, follow: bool) -> Self {
        self.follow_new = follow;
        self
    }

    pub fn with_appearance(mut self, icons: IconMode, text_git_marks: bool) -> Self {
        self.icons = icons;
        self.text_git_marks = text_git_marks;
        self
    }

    fn refresh(&mut self) {
        if !self.repo.git {
            let Some(repo) = self.repo.discover_git() else { return };
            let (prefs, notice) = Preferences::for_pane(Some(&repo.root));
            self.promotion = Some((prefs.project_mode, notice));
            if !cfg!(test) {
                self.follow_new = prefs.history_follow_new;
                self.icons = prefs.icons.effective();
                self.text_git_marks = prefs.text_git_marks;
            }
            self.repo = repo.for_history();
        }
        self.load();
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
        if !self.version.as_ref().is_some_and(|version| version.matches(&self.repo)) {
            let Some(commits) = self.repo.history(LIMIT) else { return };
            self.replace_commits(commits, selected, uncommitted);
        } else if self.uncommitted != uncommitted {
            self.uncommitted = uncommitted;
            self.rebuild_in_place(selected);
        }
    }

    /// Remembers the open commit and the cursor by hash when commits move.
    fn replace_commits(&mut self, commits: Vec<Commit>, selected: Option<TimelineRow>, uncommitted: usize) {
        self.detail = None;
        let remap = |i: usize| commits.iter().position(|c| c.hash == self.entries[i].commit.hash);
        let open = self
            .open
            .as_ref()
            .and_then(|open| Some(OpenCommit { index: remap(open.index)?, all_files: open.all_files }));
        let follow = self.follow_new && selected == Some(TimelineRow::Commit(0)) && self.open_index() != Some(0);
        let selected = match selected {
            Some(TimelineRow::Commit(0)) if follow => TimelineRow::Commit(0),
            Some(TimelineRow::Commit(i)) => TimelineRow::Commit(remap(i).unwrap_or(0)),
            Some(row @ (TimelineRow::File(_) | TimelineRow::More)) if open.is_some() => row,
            _ => TimelineRow::Commit(0),
        };
        let mut before: HashMap<_, _> = self.entries.drain(..).map(|e| (e.commit.hash, (e.fresh, e.files))).collect();
        // Detached checkouts share Git's "(detached)" label, not a branch timeline.
        let branch = self.repo.branch();
        let same_branch = branch != "(detached)" && self.version.as_ref().is_some_and(|version| version.branch == branch);
        self.entries.extend(commits.into_iter().map(|commit| {
            let (fresh, files) = before.remove(&commit.hash).unwrap_or((same_branch, None));
            Entry { commit, fresh, files }
        }));
        self.version = Some(self.repo.history_version());
        self.clicked = None;
        self.open = open;
        self.uncommitted = uncommitted;
        if follow {
            self.top = 0;
            self.build_rows(Some(selected));
        } else {
            self.rebuild_in_place(Some(selected));
        }
    }

    fn selected(&self) -> Option<TimelineRow> {
        self.rows.get(self.cursor).copied()
    }

    fn open_index(&self) -> Option<usize> {
        self.open.as_ref().map(|open| open.index)
    }

    fn open_files(&self) -> Option<&[PendingFile]> {
        self.entries[self.open_index()?].files.as_deref()
    }

    fn rebuild_in_place(&mut self, selected: Option<TimelineRow>) {
        let offset = self.cursor.saturating_sub(self.top);
        self.build_rows(selected);
        self.top = self.cursor.saturating_sub(offset);
    }

    fn build_rows(&mut self, selected: Option<TimelineRow>) {
        self.painted = None;
        self.rows.clear();
        if self.uncommitted > 0 && (!self.compact || self.entries.is_empty()) {
            self.rows.push(TimelineRow::Uncommitted);
        }
        for (i, entry) in self.entries.iter().enumerate() {
            if !self.compact && i > 0 && crosses_remote(&self.entries[i - 1].commit, &entry.commit) {
                self.rows.push(TimelineRow::Divider);
            }
            self.rows.push(TimelineRow::Commit(i));
            if let Some(open) = self.open.as_ref().filter(|open| open.index == i) {
                match entry.files.as_deref() {
                    Some([]) => self.rows.push(TimelineRow::Note("No files changed.")),
                    Some(files) => {
                        let n = files.len();
                        let shown = if open.all_files { n } else { n.min(FILE_PREVIEW) };
                        self.rows.extend((0..shown).map(TimelineRow::File));
                        if shown < n {
                            self.rows.push(TimelineRow::More);
                        }
                    }
                    None => self.rows.push(TimelineRow::Note("Git did not answer for this commit.")),
                }
            }
        }
        if self.entries.is_empty() {
            self.rows.push(TimelineRow::Note("No commits yet."));
        }
        self.cursor = selected.and_then(|selected| self.rows.iter().position(|&row| row == selected)).unwrap_or(0);
    }

    fn toggle_files(&mut self, i: usize) {
        self.open = if self.open_index() == Some(i) {
            None
        } else {
            let entry = &mut self.entries[i];
            if entry.files.is_none() {
                entry.files = self.repo.files_at(&entry.commit.hash);
            }
            Some(OpenCommit { index: i, all_files: false })
        };
        self.build_rows(Some(TimelineRow::Commit(i)));
    }

    fn show_all_files(&mut self) {
        if let Some(open) = &mut self.open {
            open.all_files = true;
            self.build_rows(Some(TimelineRow::File(FILE_PREVIEW)));
        }
    }
}

impl Pane for History {
    fn help(&self) -> Page {
        self.reading.as_ref().map_or_else(Self::help_page, Pane::help)
    }

    fn notice(&self) -> Option<&'static str> {
        self.promotion.and_then(|(_, notice)| notice)
    }

    fn promoted_worktree(&self) -> Option<(&Path, ProjectMode)> {
        self.promotion.map(|(mode, _)| (self.repo.root.as_path(), mode))
    }

    fn watch_scope(&self) -> Scope {
        self.repo.watch_scope()
    }

    fn reader_opener(&mut self) -> Option<&mut OpenReader> {
        Some(&mut self.opener)
    }

    fn git_busy(&self) -> bool {
        self.reading.is_none() && self.repo.busy()
    }

    fn check(&mut self) -> bool {
        if self.reading.is_some() {
            return true;
        }
        if !self.repo.git || self.repo.poll(&[]) {
            self.refresh();
        }
        true
    }

    fn key(&mut self, key: Key) -> After {
        if let Key::Focus(focused) = key {
            if focused == self.focused {
                return After::Ignored;
            }
            self.focused = focused;
            self.clicked = None;
            self.detail = None;
            return After::Stay;
        }
        if let Some(reader) = &mut self.reading {
            match reader.key(key) {
                After::Leave => {
                    self.reading = None;
                    // A watch check may have been skipped while the reader was open.
                    if !self.git_busy() {
                        self.check();
                    }
                    After::Stay
                }
                after => after,
            }
        } else if key == Key::Char('q') {
            After::Leave
        } else {
            let after = self.input(key, Instant::now());
            if after == After::Stay && !matches!(key, Key::WheelUp | Key::WheelDown) {
                self.focused = true;
            }
            after
        }
    }

    fn draw(&mut self, s: &mut Screen) {
        self.draw_contents(s);
    }
}

/// Adjacent commits on different sides of the comparison branch need a divider.
fn crosses_remote(before: &Commit, c: &Commit) -> bool {
    before.unpushed && !c.unpushed
}
