//! Locates and loads the working file or the version stored by Git.

use std::fs;
use std::path::{Component, Path, PathBuf};

use super::disk::ReadOnly;
use super::media_load::{self, Load, State};
use super::working_path::{Entry, LookupError, Root, Stamp};
use super::{Body, Reader, Source, Text, WorkingFile};
use crate::diff::{self, FileDiff, Line, LineKind};
use crate::git::run::OutputError;
use crate::git::{CommitId, CommitVersion, GIT_UNAVAILABLE, Kind, PendingFile, Repo};
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
        let Location { repo, relative } = Location::find(file, Repo::open_to_read)?;
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
        let WorkingFile { repo, root, relative, attributes, stamp: last, media, .. } = work.as_mut();
        let (mut entry, mut stamp) = WorkingStamp::at(repo, root, relative);
        let same = *last == Some(stamp);
        let position = match (&*media, same) {
            (Some(State::Parked(position)), true) => *position,
            (Some(_), true) => return,
            _ => None,
        };
        let was_media = media.take().is_some();
        if was_media {
            // An old player must release its snapshot before another preview starts.
            self.body = Body::Note("Git did not answer.");
            *last = None;
        }
        if (was_media || !same)
            && let Ok(entry) = &entry
            && let Some(input) = media_load::Input::inspect(&self.path, relative, entry)
        {
            let (state, note) = match Load::start(input, stamp, position) {
                Ok(load) => (State::Loading(load), media_load::PENDING),
                Err(_) => (State::Ready, media_load::FAILED),
            };
            *media = Some(state);
            *last = Some(stamp);
            self.kind = Kind::None;
            self.replace_body(Body::Note(note));
            return;
        }
        if was_media && stamp.compare && repo.busy() {
            self.replace_body(Body::Note("Waiting for Git."));
            return;
        }
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

    /// Reaps a completed preview and publishes it only if the inspected source still matches.
    pub(super) fn tick_media(&mut self) {
        let Source::Working(work) = &mut self.source else { return };
        if !matches!(&work.media, Some(State::Loading(load)) if load.ready()) {
            return;
        }
        let Some(State::Loading(load)) = work.media.replace(State::Ready) else { unreachable!() };
        let (stamp, body) = load.finish().unwrap_or_else(|| {
            (work.stamp.expect("a media job starts with a file stamp"), Body::Note(media_load::FAILED))
        });
        let (_, observed) = WorkingStamp::at(&work.repo, &work.root, &work.relative);
        if stamp == observed {
            self.replace_body(body);
            self.read_only = self.edit_refusal();
        } else {
            drop(body);
            self.refresh(false);
        }
    }

    /// Keeps the working source, but gives up all media resources when another tab is shown.
    pub(super) fn park_media(&mut self) {
        let Source::Working(work) = &mut self.source else { return };
        let position = match (&work.media, &self.body) {
            (Some(State::Loading(load)), _) => load.position,
            (Some(State::Ready), Body::Video(video)) => Some(video.position()),
            (Some(State::Ready), Body::Picture(_)) => None,
            _ => return,
        };
        // The stamp remains the one this player or worker opened, not the file
        // currently at the path. Only that same source can use the position.
        work.media = Some(State::Parked(position));
        self.body = Body::Note(media_load::PENDING);
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
    if !picture_suffix && patch.binary {
        return Body::Note(NOT_TEXT);
    }
    if !patch.lines.is_empty() {
        return Body::Text(Text::new(patch.lines, lang));
    }
    let line_kind = if patch.kind == Kind::Untracked { LineKind::Add } else { LineKind::Same };
    let lines = match working_content(entry, LIMIT) {
        Content::Gone => Err("This file is gone."),
        Content::Unreadable => Err("This file cannot be read."),
        Content::NotFile => Err(NOT_FILE),
        Content::TooLarge => Err("Too large to show."),
        Content::Link(target) => {
            return Body::Text(Text::new(vec![Line::new(line_kind, 1, &target.to_string_lossy())], None));
        }
        Content::Bytes(bytes) => byte_lines(&bytes, line_kind),
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
