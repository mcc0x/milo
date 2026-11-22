//! Locates and loads the working file or the version stored by Git.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::disk::ReadOnly;
use super::media_load::{self, Format, Load, Outcome, SourceJob, State};
use super::working_path::{Entry, LookupError, Root, Stamp};
use super::{Body, Reader, Source, Text, WorkingFile};
use crate::diff::{self, FileDiff, Line, LineKind};
use crate::git::run::OutputError;
use crate::git::{CommitId, CommitVersion, DIFF_LIMIT, GIT_UNAVAILABLE, Kind, PendingFile, Repo};
use crate::read::Content;
use crate::syntax;

/// How many bytes to read when there is no diff text to show, and the most that can be edited.
pub(super) const LIMIT: u64 = 2 << 20;
pub(super) const NOT_TEXT: &str = "This file is not text.";
pub(super) const EMPTY: &str = "Empty.";
pub(super) const NOT_FILE: &str = "Not a file, so there is nothing to show.";

struct Location {
    repo: Repo,
    relative: PathBuf,
}

impl Location {
    /// Finds the project that holds `file`, opened by `open`.
    fn find(file: &Path, open: fn(&Path) -> Option<Repo>) -> Option<Self> {
        let file = std::path::absolute(file).ok()?;
        let dir = file.ancestors().skip(1).find(|dir| dir.is_dir())?;
        // Keep the final component even if it is a link. Within a known
        // worktree also keep the directory aliases the user actually opened.
        let resolved = dir.canonicalize().ok()?;
        let tail = file.strip_prefix(dir).ok()?;
        if resolved != dir {
            if dir.components().any(|part| part == Component::ParentDir) {
                // Explicit parent traversals retain their filesystem meaning;
                // just check the project named by the lexical path.
                let mut lexical = PathBuf::new();
                for part in dir.components() {
                    if part == Component::ParentDir {
                        lexical.pop();
                    } else {
                        lexical.push(part);
                    }
                }
                let _ = enclosing_worktree(&lexical, &resolved)?;
            } else if let Some((logical, root)) = enclosing_worktree(dir, &resolved)? {
                let repo = open(&root)?;
                let relative = root.strip_prefix(&repo.root).ok()?.join(file.strip_prefix(logical).ok()?);
                return Some(Self { repo, relative });
            }
        }
        let repo = open(&resolved)?;
        let relative = resolved.join(tail).strip_prefix(&repo.root).ok()?.to_path_buf();
        Some(Self { repo, relative })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkingStamp {
    entry: Result<Stamp, LookupError>,
    compare: bool,
    pub(super) at_root_path: bool,
    pub(super) linked_ancestor: bool,
}

impl WorkingStamp {
    pub(super) fn at<'a>(repo: &Repo, root: &'a Root, relative: &Path) -> (Result<Entry<'a>, LookupError>, Self) {
        let at_path = root.at_path(&repo.root);
        let mut links = 0;
        let entry = at_path.and_then(|_| root.entry(&repo.root, relative, &mut links));
        let linked_ancestor = links != 0;
        let at_root_path = at_path == Ok(true);
        let compare = at_root_path
            && repo.git
            && relative.to_str().is_some()
            && !linked_ancestor
            && match &entry {
                Ok(entry) => !entry.is_special(),
                Err(error) => *error == LookupError::Gone,
            };
        let entry_stamp = entry.as_ref().map(Entry::stamp).map_err(|e| *e);
        let stamp = Self { entry: entry_stamp, compare, at_root_path, linked_ancestor };
        (entry, stamp)
    }

    /// Whether the file was reached where the project still is, through no link.
    /// Only such a file is edited: a save must land in the file that was read.
    pub(super) fn reachable(&self) -> Result<(), ReadOnly> {
        if !self.at_root_path {
            Err(ReadOnly::MOVED)
        } else if self.linked_ancestor {
            Err(ReadOnly::LINKED)
        } else {
            Ok(())
        }
    }
}

/// Finds the innermost worktree named by the lexical path, not one reached
/// through an alias. A target outside any worktree named by the path is refused.
fn enclosing_worktree<'a>(dir: &'a Path, resolved: &Path) -> Option<Option<(&'a Path, PathBuf)>> {
    let mut project: Option<(&Path, PathBuf)> = None;
    for name in dir.ancestors() {
        if fs::symlink_metadata(name.join(".git")).is_err() {
            continue;
        }
        let root = name.canonicalize().ok()?;
        if !resolved.starts_with(&root) {
            return None;
        }
        if project.as_ref().is_none_or(|(alias, target)| {
            alias.strip_prefix(name).is_ok_and(|tail| root.join(tail) != *target)
        }) {
            project = Some((name, root));
        }
    }
    Some(project)
}

/// Poll only the rules that can affect the selected path, not sibling directories.
pub(super) struct Attributes(Vec<(PathBuf, Option<Stamp>)>);

impl Attributes {
    fn new(repo: &Repo, relative: &Path) -> Self {
        if !repo.git || relative.to_str().is_none() {
            return Self(Vec::new());
        }
        let info = repo
            .common_dir()
            .map_or_else(|| repo.root.join(".git/info/attributes"), |dir| dir.join("info/attributes"));
        let rules = relative.ancestors().skip(1).map(|dir| repo.root.join(dir).join(".gitattributes"));
        Self(rules.chain(std::iter::once(info)).map(|path| (path, None)).collect())
    }

    fn changed(&mut self) -> bool {
        let mut changed = false;
        for (path, before) in &mut self.0 {
            let current = Stamp::at(path);
            changed |= *before != current;
            *before = current;
        }
        changed
    }
}

/// A requested working path and its held worktree directory, before reading the file.
pub(super) struct WorkingTarget {
    repo: Repo,
    root: Root,
    relative: PathBuf,
}

impl WorkingTarget {
    pub(super) fn resolve(file: &Path) -> Option<Self> {
        let open = if Format::at(&file.to_string_lossy(), file).is_some() { Repo::open } else { Repo::open_to_read };
        let Location { repo, relative } = Location::find(file, open)?;
        let root = Root::open(&repo.root).ok()?;
        Some(Self { repo, root, relative })
    }

    pub(super) fn matches(&self, reader: &Reader) -> bool {
        let Source::Working(work) = &reader.source else { return false };
        work.relative == self.relative && work.repo.root == self.repo.root && work.root.same_directory(&self.root)
    }

    pub(super) fn load(self) -> Reader {
        let Self { mut repo, root, relative } = self;
        if let Some(path) = relative.to_str() {
            repo.only(path);
        }
        let path = relative.to_string_lossy().into_owned();
        let (icons, note) = Reader::appearance_for(&repo);
        let note = if repo.git_unavailable() { Some(GIT_UNAVAILABLE) } else { note };
        let watch_only = repo.reader_filter(&relative);
        let attributes = Attributes::new(&repo, &relative);
        let source = Source::Working(Box::new(WorkingFile {
            repo,
            root,
            relative,
            watch_only,
            watch_all: Default::default(),
            attributes,
            notified: false,
            stamp: None,
            media: None,
            media_check: None,
            media_recheck: false,
        }));
        let mut reader = Reader::new(path, source, icons, note);
        reader.refresh(true);
        reader
    }
}

/// A validated committed path whose body has not yet been read.
pub(super) struct CommittedTarget {
    repo: Repo,
    path: String,
    commit: CommitVersion,
    changed: Option<PendingFile>,
}

impl CommittedTarget {
    /// The current repository takes precedence over whatever is at the old path today.
    pub(super) fn resolve(file: &Path, id: CommitId<'_>) -> Result<Self, &'static str> {
        let cwd = std::env::current_dir().map_err(|_| "Cannot find the current directory.")?;
        Self::resolve_from(&cwd, file, id)
    }

    pub(super) fn resolve_from(cwd: &Path, file: &Path, id: CommitId<'_>) -> Result<Self, &'static str> {
        let base = if file.is_absolute() { Path::new("") } else { cwd };
        let mut absolute = PathBuf::with_capacity(base.as_os_str().len() + file.as_os_str().len() + 1);
        absolute.push(base);
        for part in file.components() {
            match part {
                Component::ParentDir => return Err("File paths at a commit cannot contain '..'."),
                Component::CurDir => {}
                _ => absolute.push(part),
            }
        }
        let cwd_repo = Repo::open(cwd);
        if cwd_repo.as_ref().is_some_and(Repo::git_unavailable) {
            return Err(GIT_UNAVAILABLE);
        }
        let (repo, relative) = if let Some(repo) = cwd_repo.filter(|r| r.git)
            && let Ok(tail) = absolute.strip_prefix(&repo.root)
        {
            (repo, tail.to_path_buf())
        } else {
            const NOT_REPO: &str = "This file is not in a git repository.";
            let location = Location::find(&absolute, Repo::open).ok_or(NOT_REPO)?;
            if location.repo.git_unavailable() {
                return Err(GIT_UNAVAILABLE);
            }
            if !location.repo.git {
                return Err(NOT_REPO);
            }
            (location.repo, location.relative)
        };
        let path = relative.into_os_string().into_string().map_err(|_| "File paths at a commit must be UTF-8.")?;
        let commit = repo.resolve_commit(id).ok_or("No commit with that ID.")?;
        // Validate the resolved commit and its history even when a tab already holds its body.
        let changed = repo.changed_file_at(&commit.hash, &path).ok_or("Git did not answer for this commit.")?;
        Ok(Self { repo, path, commit, changed })
    }

    pub(super) fn matches(&self, reader: &Reader) -> bool {
        reader.path == self.path
            && matches!(&reader.source, Source::Committed { project, hash, .. }
                if project == &self.repo.root && hash == &self.commit.hash)
    }

    pub(super) fn load(self) -> Reader {
        Reader::committed(&self.repo, self.path, self.commit, self.changed.as_ref())
    }
}

enum CheckTask {
    Text { force: bool, read_status: bool },
    Media { read_worktree: bool },
}

enum Checked {
    Text { repo: Box<Repo>, patch: Option<FileDiff> },
    Media(Kind),
}

pub(super) struct MediaCheck {
    stamp: WorkingStamp,
    job: SourceJob<Option<Checked>>,
}

fn can_read_text_patch(entry: &Result<Entry<'_>, LookupError>) -> bool {
    // Git limits patch output, not how much of the working file it can read.
    entry.as_ref().is_ok_and(|entry| entry.is_regular() && !entry.is_dataless() && entry.size() <= DIFF_LIMIT)
}

impl MediaCheck {
    fn start(work: &WorkingFile, stamp: WorkingStamp, task: CheckTask) -> Option<Self> {
        let root = work.root.try_clone().ok()?;
        let relative = work.relative.clone();
        let mut repo = Box::new(work.repo.clone());
        let job = SourceJob::start(move |stop| {
            if stop.load(Ordering::Relaxed) || repo.busy() {
                return None;
            }
            let (entry, observed) = WorkingStamp::at(&repo, &root, &relative);
            if stamp != observed {
                return None;
            }
            let path = relative.to_str()?;
            match task {
                CheckTask::Media { read_worktree } => {
                    let read_worktree = read_worktree && entry.is_ok_and(|entry| !entry.is_dataless());
                    repo.media_kind(path, read_worktree).map(Checked::Media)
                }
                CheckTask::Text { force, read_status } if can_read_text_patch(&entry) => {
                    if read_status {
                        repo.read();
                    } else if !repo.read_if_changed() && !force {
                        return None;
                    }
                    if stop.load(Ordering::Relaxed) || repo.busy() || WorkingStamp::at(&repo, &root, &relative).1 != stamp {
                        return None;
                    }
                    let patch = repo.ok().then(|| file_diff(&mut repo, path)).flatten();
                    Some(Checked::Text { repo, patch })
                }
                CheckTask::Text { .. } => None,
            }
        });
        Some(Self { stamp, job })
    }
}

impl Reader {
    /// Opens a working file, which Git may remember even if its directory is gone.
    pub fn open(file: &Path) -> Option<Self> {
        WorkingTarget::resolve(file).map(WorkingTarget::load)
    }

    pub fn at_commit(file: &Path, id: CommitId<'_>) -> Result<Self, &'static str> {
        CommittedTarget::resolve(file, id).map(CommittedTarget::load)
    }

    pub(super) fn committed(repo: &Repo, path: String, commit: CommitVersion, file: Option<&PendingFile>) -> Self {
        let (icons, note) = Self::appearance_for(repo);
        let body = Body::from_lines(committed_lines(repo, &path, &commit.hash, file), syntax::for_file(&path));
        let source = Source::Committed { project: repo.root.clone(), hash: commit.hash, time: commit.time };
        let mut reader = Self::new(path, source, icons, note);
        reader.body = body;
        reader.read_only = reader.edit_refusal();
        reader
    }

    /// Rebuilds the working file when it or Git has changed, keeping the last good result if git fails.
    pub(super) fn refresh(&mut self, force: bool) {
        self.refresh_body(force);
        self.read_only = self.edit_refusal();
    }

    fn refresh_body(&mut self, force: bool) {
        let Source::Working(work) = &mut self.source else { return };
        let (mut entry, mut stamp) = WorkingStamp::at(&work.repo, &work.root, &work.relative);
        let same = work.stamp == Some(stamp);
        let format = Format::at(&self.path, &work.relative);
        let position = match (&work.media, same) {
            (Some(State::Parked(position)), true) => *position,
            _ => None,
        };
        if !same || matches!(work.media, Some(State::Parked(_))) {
            work.media_check = None;
            work.media_recheck = false;
            let replaced_media = work.media.take().is_some();
            if replaced_media {
                // Release the old player and its snapshot before admitting another.
                self.body = Body::Note("Git did not answer.");
                work.stamp = None;
                self.kind = Kind::None;
            }
            if let Some(format) = format
                && matches!(&entry, Ok(entry) if entry.is_regular())
            {
                let Ok(entry) = entry else { unreachable!() };
                let (state, note) = start_preview(format, entry, stamp, position);
                work.stamp = Some(stamp);
                work.media = Some(state);
                self.kind = Kind::None;
                self.replace_body(Body::Note(note));
                return;
            }
            if replaced_media && stamp.compare && work.repo.busy() {
                self.replace_body(Body::Note("Waiting for Git."));
                return;
            }
        }
        match &mut work.media {
            Some(State::Loading(_) | State::Parked(_)) => return,
            Some(State::Ready { status_reads }) => {
                let read_worktree = *status_reads;
                self.check_media_status(stamp, CheckTask::Media { read_worktree });
                return;
            }
            Some(State::Text { content, shown }) => {
                // TooLarge here means the source lane validated a text prefix
                // but didn't load a fallback body. Git can still supply a patch.
                if stamp.compare && can_read_text_patch(&entry) {
                    if work.repo.busy() {
                        return;
                    }
                    let force = work.attributes.changed() || force;
                    let read_status = !*shown;
                    self.check_media_status(stamp, CheckTask::Text { force, read_status });
                } else if !*shown {
                    *shown = true;
                    let body = body_from_working_content(content, LineKind::Same, syntax::for_file(&self.path));
                    self.replace_body(body);
                }
                return;
            }
            None => {}
        }
        let (repo, root, relative, attributes, last) =
            (&mut work.repo, &work.root, &work.relative, &mut work.attributes, &mut work.stamp);
        let attrs_changed = stamp.compare && attributes.changed();
        // A changed entry needs fresh status. Otherwise poll status once and
        // also refresh the patch when rules changed without changing porcelain.
        let read_status = last.as_ref() != Some(&stamp);
        if !read_status {
            let status_changed = stamp.compare && repo.read_if_changed();
            if !(status_changed || stamp.compare && force || attrs_changed) {
                return;
            }
        }
        let unchanged = || FileDiff { lines: Vec::new(), kind: Kind::None, binary: false };
        let patch = if stamp.compare {
            if read_status {
                repo.read();
            }
            let patch = repo.ok().then(|| file_diff(repo, &self.path)).flatten();
            // Git works by pathname. Do not present its output if that name no
            // longer refers to the repository and entry we inspected.
            let (after, observed) = WorkingStamp::at(repo, root, relative);
            if observed != stamp {
                if let Some(format) = format
                    && matches!(&after, Ok(entry) if entry.is_regular())
                {
                    // Never let the synchronous source fallback read a new candidate.
                    let Ok(entry) = after else { unreachable!() };
                    let (state, note) = start_preview(format, entry, observed, None);
                    *last = Some(observed);
                    work.media = Some(state);
                    self.kind = Kind::None;
                    self.replace_body(Body::Note(note));
                    return;
                }
                entry = after;
                stamp = WorkingStamp { compare: false, ..observed };
                Some(unchanged())
            } else {
                patch
            }
        } else {
            Some(unchanged())
        };
        let note = match &entry {
            Err(LookupError::Outside) => Some("This path now leads outside the project."),
            Ok(entry) if entry.is_special() => Some(NOT_FILE),
            _ => None,
        };
        if let Some(note) = note {
            self.kind = Kind::None;
            *last = Some(stamp);
            self.replace_body(Body::Note(note));
            return;
        }
        let Some(patch) = patch else {
            let note = match working_content(&entry, 0) {
                Content::Unreadable => "This file cannot be read.",
                Content::NotFile => NOT_FILE,
                _ if !repo.ok() => return,
                Content::Gone => "This file is gone.",
                _ => "Too large to show.",
            };
            self.kind = Kind::None;
            *last = Some(stamp);
            self.replace_body(Body::Note(note));
            return;
        };
        let picture_suffix = crate::picture::is_picture(&self.path);
        self.kind = patch.kind;
        *last = Some(stamp);
        let mut body = working_body(&entry, patch, picture_suffix, syntax::for_file(&self.path));
        if let Body::Text(text) = &mut body
            && entry.as_ref().is_ok_and(Entry::is_regular)
            && relative.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
        {
            let previous = match &mut self.body {
                Body::Text(old) => old.preview.take(),
                _ => None,
            };
            text.preview = Some(if let Some(mut preview) = previous {
                preview.replace(&text.lines);
                preview
            } else {
                Box::new(super::markdown::Preview::new(&text.lines))
            });
        }
        self.replace_body(body);
    }

    fn check_media_status(&mut self, stamp: WorkingStamp, task: CheckTask) {
        let Source::Working(work) = &mut self.source else { return };
        if !stamp.compare || work.repo.busy() {
            return;
        }
        if work.media_check.is_some() {
            work.media_recheck = true;
        } else {
            work.media_check = MediaCheck::start(work, stamp, task);
        }
    }

    /// Only the current inspected source may be decoded or published.
    pub(super) fn tick_media(&mut self) {
        let Source::Working(work) = &mut self.source else { return };
        if let Some(State::Loading(load)) = &mut work.media
            && let Some(result) = load.poll(|stamp| WorkingStamp::at(&work.repo, &work.root, &work.relative).1 == stamp)
        {
            match result {
                Outcome::Stale => {
                    work.stamp = None;
                    self.refresh(false);
                }
                Outcome::Text(content) => {
                    work.media = Some(State::Text { content, shown: false });
                    self.refresh(false);
                }
                Outcome::Media { body, status_reads } => {
                    work.media = Some(State::Ready { status_reads });
                    self.replace_body(body);
                    self.refresh(false);
                }
            }
            return;
        }
        let Some(check) = &mut work.media_check else { return };
        let Some(result) = check.job.poll() else { return };
        let stamp = check.stamp;
        work.media_check = None;
        if WorkingStamp::at(&work.repo, &work.root, &work.relative).1 != stamp {
            self.refresh(false);
            return;
        }
        let recheck = std::mem::take(&mut work.media_recheck);
        match result.flatten() {
            Some(Checked::Media(kind)) => self.kind = kind,
            Some(Checked::Text { repo, patch }) => {
                work.repo = *repo;
                let Some(State::Text { content, shown }) = &mut work.media else { return };
                let lang = syntax::for_file(&self.path);
                let next = match patch {
                    Some(patch) => {
                        *shown = true;
                        self.kind = patch.kind;
                        body_from_patch(patch, crate::picture::is_picture(&self.path), lang)
                            .unwrap_or_else(|kind| body_from_working_content(content, kind, lang))
                    }
                    None if work.repo.ok() => Body::Note("Too large to show."),
                    None if !*shown => Body::Note("Git did not answer."),
                    None => return,
                };
                self.replace_body(next);
                self.read_only = self.edit_refusal();
                self.top = self.top.min(self.last_top());
            }
            None => {}
        }
        if recheck {
            self.refresh(true);
        }
    }

    /// Keeps the working source, but gives up media jobs and snapshots when hidden.
    pub(super) fn park_media(&mut self) {
        let Source::Working(work) = &mut self.source else { return };
        work.media_check = None;
        work.media_recheck = false;
        let position = match (&work.media, &self.body) {
            (Some(State::Loading(load)), _) => load.position,
            (Some(State::Ready { .. }), Body::Video(video)) => Some(video.position()),
            (Some(State::Ready { .. }), Body::Picture(_)) => None,
            _ => return,
        };
        // Only that same source stamp can use the player's saved position.
        work.media = Some(State::Parked(position));
        self.body = Body::Note(media_load::PENDING);
    }
}

fn start_preview(format: Format, entry: Entry<'_>, stamp: WorkingStamp, position: Option<Duration>) -> (State, &'static str) {
    match entry.source() {
        Ok(target) => (State::Loading(Load::start(format, target, stamp, position)), media_load::PENDING),
        Err(_) => (State::Ready { status_reads: false }, media_load::FAILED),
    }
}

fn working_content(entry: &Result<Entry<'_>, LookupError>, limit: u64) -> Content {
    entry.as_ref().map_or_else(|e| e.content(), |entry| entry.read(limit))
}

fn working_body(
    entry: &Result<Entry<'_>, LookupError>,
    patch: FileDiff,
    picture_suffix: bool,
    lang: Option<&'static syntax::Lang>,
) -> Body {
    body_from_patch(patch, picture_suffix, lang)
        .unwrap_or_else(|kind| body_from_working_content(&working_content(entry, LIMIT), kind, lang))
}

fn body_from_patch(patch: FileDiff, picture_suffix: bool, lang: Option<&'static syntax::Lang>) -> Result<Body, LineKind> {
    if !picture_suffix && patch.binary {
        return Ok(Body::Note(NOT_TEXT));
    }
    if !patch.lines.is_empty() {
        return Ok(Body::Text(Text::new(patch.lines, lang)));
    }
    Err(if patch.kind == Kind::Untracked { LineKind::Add } else { LineKind::Same })
}

fn body_from_working_content(content: &Content, kind: LineKind, lang: Option<&'static syntax::Lang>) -> Body {
    let lines = match content {
        Content::Gone => Err("This file is gone."),
        Content::Unreadable => Err("This file cannot be read."),
        Content::NotFile => Err(NOT_FILE),
        Content::TooLarge => Err("Too large to show."),
        Content::Link(target) => {
            return Body::Text(Text::new(vec![Line::new(kind, 1, &target.to_string_lossy())], None));
        }
        Content::Bytes(bytes) => byte_lines(bytes, kind),
    };
    Body::from_lines(lines, lang)
}

pub(super) fn text(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) { None } else { std::str::from_utf8(bytes).ok() }
}

/// Turns a stored file into text, or says why it cannot be shown.
fn byte_lines(bytes: &[u8], kind: LineKind) -> Result<Vec<Line>, &'static str> {
    if bytes.is_empty() {
        return Err(EMPTY);
    }
    let text = text(bytes).ok_or(NOT_TEXT)?;
    Ok((1..).zip(text.lines()).map(|(no, line)| Line::new(kind, no, line)).collect())
}

fn file_diff(repo: &mut Repo, path: &str) -> Option<FileDiff> {
    if repo.is_untracked(path) {
        return Some(FileDiff { lines: Vec::new(), kind: Kind::Untracked, binary: false });
    }
    let mut diff = diff::parse(&repo.file_diff(path, None)?);
    // A file that looks new may have been moved here; compare it with its old path.
    if diff.kind == Kind::Added
        && let Some(from) = repo.moved_from(path)
        && let Some(raw) = repo.file_diff(path, Some(&from))
    {
        diff = diff::parse(&raw);
    }
    if repo.is_unmerged(path) {
        diff.kind = Kind::Conflict;
    }
    repo.ok().then_some(diff)
}

/// A patch covers all the lines when there are edits. Otherwise, read the stored file.
fn committed_lines(repo: &Repo, path: &str, hash: &str, file: Option<&PendingFile>) -> Result<Vec<Line>, &'static str> {
    if let Some(file) = file {
        let raw = repo.file_diff_at(hash, file).map_err(|err| match err {
            OutputError::TooLarge => "Too large to show.",
            OutputError::Unavailable => GIT_UNAVAILABLE,
            OutputError::Failed => "Git did not answer for this file.",
        })?;
        let patch = diff::parse(&raw);
        if patch.binary {
            return Err(NOT_TEXT);
        }
        if !patch.lines.is_empty() {
            return Ok(patch.lines);
        }
        if file.kind == Kind::Deleted {
            return Err(EMPTY);
        }
    }
    let raw = repo.blob_at(hash, path).map_err(|err| match err {
        OutputError::TooLarge => "Too large to show.",
        OutputError::Unavailable => GIT_UNAVAILABLE,
        OutputError::Failed => "There is no file here in this commit.",
    })?;
    byte_lines(&raw, LineKind::Same)
}
