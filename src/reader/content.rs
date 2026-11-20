//! Locates and loads the working file or the version stored by Git.

use std::fs;
use std::path::{Component, Path, PathBuf};

use super::working_path::{Entry, LookupError, Root, Stamp};
use super::{Body, Reader, Source, Text, WorkingFile};
use crate::diff::{self, FileDiff, Line, LineKind};
use crate::git::run::OutputError;
use crate::git::{CommitId, GIT_UNAVAILABLE, Kind, PendingFile, Repo};
use crate::picture::{self, Picture};
use crate::read::Content;
use crate::syntax;

/// How many bytes to read when there is no diff text to show.
const LIMIT: u64 = 2 << 20;
pub(super) const NOT_TEXT: &str = "This file is not text.";
const NOT_FILE: &str = "Not a file, so there is nothing to show.";

struct Location {
    repo: Repo,
    relative: PathBuf,
    path: String,
}

impl Location {
    fn find(file: &Path) -> Option<Self> {
        let file = std::path::absolute(file).ok()?;
        let dir = file.ancestors().skip(1).find(|dir| dir.is_dir())?;
        // Resolve the directory, but keep the final component even if it is a link.
        let resolved = dir.canonicalize().ok()?;
        if resolved != dir && leaves_project(dir, &resolved) {
            return None;
        }
        let absolute = resolved.join(file.strip_prefix(dir).ok()?);
        let repo = Repo::open(&resolved)?;
        let relative = absolute.strip_prefix(&repo.root).ok()?.to_path_buf();
        let path = relative.to_string_lossy().into_owned();
        Some(Self { repo, relative, path })
    }
}

#[derive(PartialEq, Eq)]
pub(super) struct WorkingStamp {
    entry: Result<Stamp, LookupError>,
    compare: bool,
}

impl WorkingStamp {
    fn at<'a>(repo: &Repo, root: &'a Root, relative: &Path) -> (Result<Entry<'a>, LookupError>, Self) {
        let at_path = root.at_path(&repo.root);
        let entry = at_path.and_then(|_| root.entry(&repo.root, relative));
        let compare = at_path == Ok(true)
            && repo.git
            && relative.to_str().is_some()
            && match &entry {
                Ok(entry) => !entry.linked_ancestor && !entry.is_special(),
                Err(error) => *error == LookupError::Gone,
            };
        let stamp = Self { entry: entry.as_ref().map(Entry::stamp).map_err(|e| *e), compare };
        (entry, stamp)
    }
}

/// Don't let a directory link move the reader out of a project named by the path.
fn leaves_project(dir: &Path, resolved: &Path) -> bool {
    let mut lexical = PathBuf::new();
    for part in dir.components() {
        if part == Component::ParentDir {
            lexical.pop();
        } else {
            lexical.push(part);
        }
    }
    lexical.ancestors().any(|root| {
        fs::symlink_metadata(root.join(".git")).is_ok()
            && root.canonicalize().is_ok_and(|root| !resolved.starts_with(root))
    })
}

impl Reader {
    /// Opens `file`, which git may remember even if its directory is gone.
    pub fn open(file: &Path) -> Option<Self> {
        let Location { mut repo, relative, path } = Location::find(file)?;
        let root = Root::open(&repo.root).ok()?;
        repo.only(&path);
        let (icons, note) = Self::appearance_for(&repo);
        let note = if repo.git_unavailable() { Some(GIT_UNAVAILABLE) } else { note };
        let source = Source::Working(Box::new(WorkingFile { repo, root, relative, stamp: None }));
        let mut reader = Self::new(path, source, icons, note);
        reader.refresh(true);
        Some(reader)
    }

    /// The current repository takes precedence over whatever is at the old path today.
    pub fn at_commit(file: &Path, id: CommitId<'_>) -> Result<Self, &'static str> {
        let cwd = std::env::current_dir().map_err(|_| "Cannot find the current directory.")?;
        Self::at_commit_from(&cwd, file, id)
    }

    pub(super) fn at_commit_from(cwd: &Path, file: &Path, id: CommitId<'_>) -> Result<Self, &'static str> {
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
        let (repo, path) = if let Some(repo) = cwd_repo.filter(|r| r.git)
            && let Ok(tail) = absolute.strip_prefix(&repo.root)
        {
            (repo, tail.to_string_lossy().into_owned())
        } else {
            const NOT_REPO: &str = "This file is not in a git repository.";
            let location = Location::find(&absolute).ok_or(NOT_REPO)?;
            if location.repo.git_unavailable() {
                return Err(GIT_UNAVAILABLE);
            }
            if !location.repo.git {
                return Err(NOT_REPO);
            }
            (location.repo, location.path)
        };
        let commit = repo.resolve_commit(id).ok_or("No commit with that ID.")?;
        let changed = repo.changed_file_at(&commit.hash, &path).ok_or("Git did not answer for this commit.")?;
        Ok(Self::committed(&repo, &path, &commit.hash, commit.time, changed.as_ref()))
    }

    pub(super) fn committed(repo: &Repo, path: &str, hash: &str, time: i64, file: Option<&PendingFile>) -> Self {
        let short_id = hash[..hash.len().min(7)].to_string();
        let (icons, note) = Self::appearance_for(repo);
        let mut reader = Self::new(path.to_string(), Source::Committed { short_id, time }, icons, note);
        reader.body = Body::from_lines(committed_lines(repo, path, hash, file), syntax::for_file(path));
        reader
    }

    /// Rebuilds the working file when it or Git has changed, keeping the last good result if git fails.
    pub(super) fn refresh(&mut self, force: bool) {
        let Source::Working(work) = &mut self.source else { return };
        let WorkingFile { repo, root, relative, stamp: last } = work.as_mut();
        let (mut entry, mut stamp) = WorkingStamp::at(repo, root, relative);
        // A changed file already needs a fresh status read, even if Git's
        // porcelain entry for it is unchanged. Otherwise use the current poll.
        let read_status = force || last.as_ref() != Some(&stamp);
        if !read_status && !(stamp.compare && repo.read_if_changed()) {
            return;
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
            self.body = Body::Note(note);
            *last = Some(stamp);
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
            self.body = Body::Note(note);
            *last = Some(stamp);
            return;
        };
        let picture = picture::is_picture(&self.path);
        let kept = picture && last.as_ref() == Some(&stamp) && matches!(self.body, Body::Picture(_));
        self.kind = patch.kind;
        *last = Some(stamp);
        if kept {
            return;
        }
        self.body = working_body(relative, &entry, patch, picture, syntax::for_file(&self.path));
    }
}

fn working_content(entry: &Result<Entry<'_>, LookupError>, limit: u64) -> Content {
    entry.as_ref().map_or_else(|e| e.content(), |entry| entry.read(limit))
}

fn working_body(
    path: &Path,
    entry: &Result<Entry<'_>, LookupError>,
    patch: FileDiff,
    picture: bool,
    lang: Option<&'static syntax::Lang>,
) -> Body {
    if !picture {
        if patch.binary {
            return Body::Note(NOT_TEXT);
        }
        if !patch.lines.is_empty() {
            return Body::Text(Text::new(patch.lines, lang));
        }
    }
    let line_kind = if patch.kind == Kind::Untracked { LineKind::Add } else { LineKind::Same };
    let lines = match working_content(entry, if picture { picture::FILE_LIMIT } else { LIMIT }) {
        Content::Gone | Content::Link(_) if !patch.lines.is_empty() => Ok(patch.lines),
        Content::Gone => Err("This file is gone."),
        Content::Unreadable => Err("This file cannot be read."),
        Content::NotFile => Err(NOT_FILE),
        Content::TooLarge => Err("Too large to show."),
        Content::Link(target) => {
            return Body::Text(Text::new(vec![Line::new(line_kind, 1, &target.to_string_lossy())], None));
        }
        Content::Bytes(mut bytes) => {
            if picture && could_be_picture(&bytes) {
                bytes = match Picture::decode(bytes, path) {
                    Ok(picture) => return Body::Picture(picture),
                    Err(bytes) if text(&bytes).is_some() => bytes,
                    Err(_) => return Body::Note("This file cannot be shown as a picture."),
                };
            }
            if !patch.lines.is_empty() {
                Ok(patch.lines)
            } else if bytes.len() as u64 > LIMIT {
                Err("Too large to show.")
            } else {
                byte_lines(&bytes, line_kind)
            }
        }
    };
    Body::from_lines(lines, lang)
}

/// SVG and PDF may also be valid UTF-8; let the converter decide for those.
fn could_be_picture(bytes: &[u8]) -> bool {
    let prefix = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes).trim_ascii_start();
    prefix.starts_with(b"<") || prefix.starts_with(b"%PDF-") || text(bytes).is_none()
}

fn text(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) { None } else { std::str::from_utf8(bytes).ok() }
}

/// Turns a stored file into text, or says why it cannot be shown.
fn byte_lines(bytes: &[u8], kind: LineKind) -> Result<Vec<Line>, &'static str> {
    if bytes.is_empty() {
        return Err("Empty.");
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
            return Err("Empty.");
        }
    }
    let raw = repo.blob_at(hash, path).map_err(|err| match err {
        OutputError::TooLarge => "Too large to show.",
        OutputError::Unavailable => GIT_UNAVAILABLE,
        OutputError::Failed => "There is no file here in this commit.",
    })?;
    byte_lines(&raw, LineKind::Same)
}
