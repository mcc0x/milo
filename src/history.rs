//! Shows recent commits and the files they touched in a pane of their own.

mod draw;
mod input;
mod listing;
mod refresh;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ops::Range;
use std::os::fd::RawFd;
use std::path::Path;
#[cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use self::listing::Listing;

use crate::git::{Commit, CommitFilesError, DiffFiles, HistoryVersion, Repo};
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
/// How long a key waits for Git to list a commit's files before the pane
/// says it is reading them and takes the next key. Most commits are listed
/// within it, and then show no such note.
const PATIENCE: Duration = Duration::from_millis(40);
/// The commits to show once the list drawn from `version` is no longer the
/// repository's, or `Some(None)` while it still is. `None` if Git cannot answer.
fn commits_since(repo: &Repo, version: Option<&HistoryVersion>) -> Option<Option<Vec<Commit>>> {
    if !repo.ok() {
        return None;
    }
    if version.is_some_and(|version| version.matches(repo)) {
        return Some(None);
    }
    repo.history(LIMIT + 1).map(Some)
}

/// How soon the pane looks again for files that Git is still reading.
const LISTING_LOOK: Duration = Duration::from_millis(10);

struct Entry {
    commit: Commit,
    /// Arrived since this pane first read the history.
    fresh: bool,
    /// File lists stay cached while their commit is in the timeline.
    files: Option<DiffFiles>,
}

/// Rows the cursor can point at, with the guides between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Only one commit can be open, so its consecutive file rows need just a range.
#[derive(Default)]
struct Timeline {
    markers: Vec<TimelineRow>,
    files: Range<usize>,
}

impl Timeline {
    fn clear(&mut self) {
        self.markers.clear();
        self.files = 0..0;
    }

    fn push(&mut self, row: TimelineRow) {
        self.markers.push(row);
    }

    fn push_files(&mut self, count: usize) {
        let start = self.markers.len();
        self.files = start..start + count;
    }

    fn len(&self) -> usize {
        self.markers.len() + self.files.len()
    }

    fn get(&self, index: usize) -> Option<TimelineRow> {
        if index < self.files.start {
            self.markers.get(index).copied()
        } else if index < self.files.end {
            Some(TimelineRow::File(index - self.files.start))
        } else {
            self.markers.get(index - self.files.len()).copied()
        }
    }

    fn position(&self, row: TimelineRow) -> Option<usize> {
        if let TimelineRow::File(i) = row {
            return (i < self.files.len()).then_some(self.files.start + i);
        }
        let i = self.markers.iter().position(|&marker| marker == row)?;
        Some(if i < self.files.start { i } else { i + self.files.len() })
    }

    fn selectable(&self, index: usize) -> bool {
        self.get(index).is_some_and(TimelineRow::selectable)
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
    error: Option<CommitFilesError>,
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
    truncated: bool,
    /// First row of the all-pushed suffix, if there are commits on both sides.
    remote_boundary: Option<usize>,
    uncommitted: usize,
    open: Option<OpenCommit>,
    /// The one read of a commit's files that is still running.
    listing: Option<Listing>,
    /// Whether to poll Git on a thread of its own, as the pane loop can wait for.
    refresh_away: bool,
    refresh: Option<refresh::Running>,
    patience: Duration,
    #[cfg(test)]
    listing_releases: Option<Arc<AtomicUsize>>,
    rows: Timeline,
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
            truncated: false,
            remote_boundary: None,
            uncommitted: 0,
            open: None,
            listing: None,
            refresh_away: false,
            refresh: None,
            // Tests see a commit's files in the frame after the key, unless they ask otherwise.
            patience: if cfg!(test) { Duration::from_secs(60) } else { PATIENCE },
            #[cfg(test)]
            listing_releases: None,
            rows: Timeline::default(),
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

    /// Never waits for Git on a key, and takes each result only when the test releases it.
    #[cfg(test)]
    pub(crate) fn impatient(mut self, releases: Arc<AtomicUsize>) -> Self {
        self.patience = Duration::ZERO;
        self.listing_releases = Some(releases);
        self
    }

    /// Polls Git away from the loop, which must then wait for `unfinished_check`.
    pub fn refreshing_away(mut self) -> Self {
        self.refresh_away = true;
        self
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

    /// Reads everything again, and adopts a repository made where there was none.
    fn reload(&mut self) {
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
        // What a refresh started before this read finds is no longer news.
        self.refresh = None;
        self.repo.read();
        if let Some(commits) = commits_since(&self.repo, self.version.as_ref()) {
            self.take_in(commits);
        }
    }

    /// Shows the repository as just read: its commits if they moved, or else
    /// the same ones with the uncommitted work and the open commit's files of now.
    fn take_in(&mut self, commits: Option<Vec<Commit>>) {
        let selected = self.selected();
        let uncommitted = self.repo.uncommitted_count();
        if let Some(commits) = commits {
            self.replace_commits(commits, selected, uncommitted);
        } else if self.load_files(self.patience) || self.uncommitted != uncommitted {
            self.uncommitted = uncommitted;
            self.rebuild_in_place(selected);
        }
    }

    /// Remembers the open commit and the cursor by hash when commits move.
    fn replace_commits(&mut self, mut commits: Vec<Commit>, selected: Option<TimelineRow>, uncommitted: usize) {
        self.detail = None;
        self.truncated = commits.len() > LIMIT;
        commits.truncate(LIMIT);
        let remap = |i: usize| commits.iter().position(|c| c.hash == self.entries[i].commit.hash);
        let open = self.open.as_ref().and_then(|open| {
            Some(OpenCommit { index: remap(open.index)?, all_files: open.all_files, error: None })
        });
        let tip_changed = commits.first().map(|c| &c.hash) != self.entries.first().map(|e| &e.commit.hash);
        let follow = self.follow_new
            && tip_changed
            && selected == Some(TimelineRow::Commit(0))
            && self.open_index() != Some(0);
        let selected = match selected {
            Some(TimelineRow::Commit(0)) if follow => TimelineRow::Commit(0),
            Some(TimelineRow::Commit(i)) => TimelineRow::Commit(remap(i).unwrap_or(0)),
            Some(row @ (TimelineRow::File(_) | TimelineRow::More)) if open.is_some() => row,
            _ => TimelineRow::Commit(0),
        };
        self.remote_boundary = commits.iter().rposition(|c| c.unpushed).map(|i| i + 1).filter(|&i| i < commits.len());
        let mut before: HashMap<_, _> = self.entries.drain(..).map(|e| (e.commit.hash, (e.fresh, e.files))).collect();
        // Detached checkouts share Git's "(detached)" label, not a branch timeline.
        let branch = self.repo.branch();
        let same_branch =
            branch != "(detached)" && self.version.as_ref().is_some_and(|version| version.branch == branch);
        self.entries.extend(commits.into_iter().map(|commit| {
            let (fresh, files) = before.remove(&commit.hash).unwrap_or((same_branch && tip_changed, None));
            Entry { commit, fresh, files }
        }));
        self.version = Some(self.repo.history_version());
        self.clicked = None;
        self.open = open;
        self.load_files(self.patience);
        self.uncommitted = uncommitted;
        if follow {
            self.top = 0;
            self.build_rows(Some(selected));
        } else {
            self.rebuild_in_place(Some(selected));
        }
    }

    fn selected(&self) -> Option<TimelineRow> {
        self.rows.get(self.cursor)
    }

    fn open_index(&self) -> Option<usize> {
        self.open.as_ref().map(|open| open.index)
    }

    fn open_files(&self) -> Option<&DiffFiles> {
        self.entries[self.open_index()?].files.as_ref()
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
            if !self.compact && self.remote_boundary == Some(i) {
                self.rows.push(TimelineRow::Divider);
            }
            self.rows.push(TimelineRow::Commit(i));
            if let Some(open) = self.open.as_ref().filter(|open| open.index == i) {
                match &entry.files {
                    Some(diff) if diff.files.is_empty() && diff.unrepresented == 0 => {
                        self.rows.push(TimelineRow::Note("No files changed."));
                    }
                    Some(diff) => {
                        let n = diff.files.len();
                        let shown = if open.all_files { n } else { n.min(FILE_PREVIEW) };
                        self.rows.push_files(shown);
                        if shown < n {
                            self.rows.push(TimelineRow::More);
                        }
                        if diff.unrepresented != 0 {
                            self.rows.push(TimelineRow::Note("Some file names cannot be shown."));
                        }
                    }
                    None => self.rows.push(TimelineRow::Note(match open.error {
                        Some(CommitFilesError::ShallowBoundary) => "Shallow history: parent unavailable.",
                        Some(CommitFilesError::Git) => "Git did not answer for this commit.",
                        None => "Reading files…",
                    })),
                }
            }
        }
        if self.entries.is_empty() {
            self.rows.push(TimelineRow::Note("No commits yet."));
        } else if self.truncated {
            self.rows.push(TimelineRow::Note("Older commits not shown."));
        }
        self.cursor = selected.and_then(|selected| self.rows.position(selected)).unwrap_or(0);
    }

    fn toggle_files(&mut self, i: usize) {
        self.open = if self.open_index() == Some(i) {
            None
        } else {
            Some(OpenCommit { index: i, all_files: false, error: None })
        };
        self.load_files(self.patience);
        self.build_rows(Some(TimelineRow::Commit(i)));
    }

    /// Starts reading the open commit's files unless they are kept or a read
    /// is running, and takes a result that arrives within the pane's patience.
    /// Returns whether the open commit's rows changed.
    fn load_files(&mut self, patience: Duration) -> bool {
        let Some(open) = &mut self.open else { return false };
        let entry = &self.entries[open.index];
        if entry.files.is_some() {
            return false;
        }
        if self.listing.is_none() {
            match Listing::start(self.repo.commit_files(), &entry.commit.hash) {
                Ok(listing) => self.listing = Some(listing),
                Err(_) => return open.error.replace(CommitFilesError::Git) != Some(CommitFilesError::Git),
            }
        }
        self.collect_files(patience)
    }

    /// Keeps a finished read with its commit, wherever that is in the timeline
    /// now, and then reads the open commit if it was not the one read. A commit
    /// opened in between waits for the one read to end: no second read runs.
    fn collect_files(&mut self, patience: Duration) -> bool {
        #[cfg(test)]
        if self.listing_releases.as_ref().is_some_and(|releases| releases.load(Ordering::Relaxed) == 0) {
            return false;
        }
        let Some(listed) = self.listing.as_ref().and_then(|listing| listing.finished(patience)) else { return false };
        #[cfg(test)]
        if let Some(releases) = &self.listing_releases {
            releases.fetch_sub(1, Ordering::Relaxed);
        }
        let hash = self.listing.take().map(|mut listing| std::mem::take(&mut listing.hash)).unwrap_or_default();
        // A commit that left the timeline while it was read has nowhere to keep its files.
        let index = self.entries.iter().position(|entry| entry.commit.hash == hash);
        let Some(open) = self.open.as_mut().filter(|open| Some(open.index) == index) else {
            if let (Ok(files), Some(index)) = (listed, index) {
                self.entries[index].files = Some(files);
            }
            return self.load_files(Duration::ZERO);
        };
        match listed {
            Ok(files) => {
                self.entries[open.index].files = Some(files);
                open.error = None;
                true
            }
            Err(error) => open.error.replace(error) != Some(error),
        }
    }

    /// Shows files that arrived after the pane stopped waiting for them.
    fn take_listed_files(&mut self) {
        let selected = self.selected();
        if !self.collect_files(Duration::ZERO) {
            return;
        }
        self.rebuild_in_place(selected);
        // As when a key opens a commit at the edge, but not under a click's pointer.
        if self.clicked.is_none() && selected == self.open_index().map(TimelineRow::Commit) {
            self.keep_preview_in_view();
        }
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

    /// While a commit's files are read, the next frame looks for them: this
    /// reaches the loop through a pane that wraps this one, as a descriptor
    /// of its own would not.
    fn next_frame(&self) -> Option<Duration> {
        let reader = self.reading.as_ref().and_then(Pane::next_frame);
        let listing = self.listing.as_ref().map(|_| LISTING_LOOK);
        reader.into_iter().chain(listing).min()
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

    fn draw(&mut self, s: &mut Screen) {
        self.take_listed_files();
        self.draw_contents(s);
    }

    fn check(&mut self) -> bool {
        if self.reading.is_some() {
            return true;
        }
        if !self.repo.git {
            self.reload();
            return true;
        }
        let away = self.refresh.is_some() || self.refresh_away && self.start_refresh();
        if !away && self.repo.poll(&[]) {
            self.load();
        }
        true
    }

    fn unfinished_check(&self) -> Option<RawFd> {
        self.refresh.as_ref().map(refresh::Running::fd)
    }

    fn finish_check(&mut self) -> Option<bool> {
        self.finish_refresh()
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
}
