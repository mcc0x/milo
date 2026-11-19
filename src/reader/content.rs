//! Locates and loads the working file or the version stored by Git.

use std::fs;
use std::path::{Component, Path, PathBuf};

use super::{Body, Reader, Source, Stamp, Text, WorkingFile};
use crate::diff::{self, FileDiff, Line, LineKind};
use crate::git::run::OutputError;
use crate::git::{CommitId, Kind, PendingFile, Repo};
use crate::picture::{self, Picture};
use crate::read::{self, Content};
use crate::syntax;

/// How many bytes to read when there is no diff text to show.
const LIMIT: u64 = 2 << 20;
pub(super) const NOT_TEXT: &str = "This file is not text.";
const NOT_FILE: &str = "Not a file, so there is nothing to show.";

struct Location {
    repo: Repo,
    absolute: PathBuf,
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
        let path = absolute.strip_prefix(&repo.root).ok()?.to_string_lossy().into_owned();
        Some(Self { repo, absolute, path })
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
        let Location { mut repo, absolute, path } = Location::find(file)?;
        repo.only(&path);
        let mut reader = Self::new(path, Source::Working(Box::new(WorkingFile { repo, absolute, stamp: None })));
        reader.load();
        reader.reveal_first_change();
        Some(reader)
    }

    /// The current repository takes precedence over whatever is at the old path today.
    pub fn at_commit(file: &Path, id: CommitId<'_>) -> Result<Self, &'static str> {
        let cwd = std::env::current_dir().map_err(|_| "Cannot find the current directory.")?;
        let absolute = cwd.join(file);
        let (repo, path) = if let Some(repo) = Repo::open(&cwd).filter(|r| r.git)
            && let Ok(tail) = absolute.strip_prefix(&repo.root)
            && tail.components().all(|part| matches!(part, Component::Normal(_)))
        {
            (repo, tail.to_string_lossy().into_owned())
        } else {
            let location =
                Location::find(file).filter(|l| l.repo.git).ok_or("This file is not in a git repository.")?;
            (location.repo, location.path)
        };
        let commit = repo.resolve_commit(id).ok_or("No commit with that ID.")?;
        let files = repo.files_at(&commit.hash).ok_or("Git did not answer for this commit.")?;
        let changed = files.iter().find(|file| file.path == path);
        Ok(Self::committed(&repo, &path, &commit.hash, commit.time, changed))
    }

    pub(super) fn committed(repo: &Repo, path: &str, hash: &str, time: i64, file: Option<&PendingFile>) -> Self {
        let short_id = hash[..hash.len().min(7)].to_string();
        let mut reader = Self::new(path.to_string(), Source::Committed { short_id, time });
        reader.body = Body::from_lines(committed_lines(repo, path, hash, file), syntax::for_file(path));
        reader.reveal_first_change();
        reader
    }

    /// Rebuilds the working file on screen, keeping the last good result if git fails.
    pub(super) fn load(&mut self) {
        let Source::Working(work) = &mut self.source else { return };
        let metadata = work.absolute.symlink_metadata().ok();
        let stamp = metadata.as_ref().map(Stamp::of);
        let outside = work
            .absolute
            .parent()
            .and_then(|dir| dir.canonicalize().ok())
            .is_some_and(|dir| !dir.starts_with(&work.repo.root));
        // Git cannot diff a tracked file replaced by a pipe or device.
        if outside || metadata.is_some_and(|m| !m.is_file() && !m.is_symlink()) {
            self.kind = Kind::None;
            self.body = Body::Note(if outside { "This path now leads outside the project." } else { NOT_FILE });
            work.stamp = stamp;
            return;
        }
        work.repo.read();
        let Some(patch) = work.repo.ok().then(|| work.file_diff(&self.path)).flatten() else {
            let note = match read::file(&work.absolute, 0) {
                Content::Unreadable => "This file cannot be read.",
                Content::NotFile => NOT_FILE,
                _ if !work.repo.ok() => return,
                Content::Gone => "This file is gone.",
                _ => "Too large to show.",
            };
            self.kind = Kind::None;
            self.body = Body::Note(note);
            work.stamp = stamp;
            return;
        };
        let picture = picture::is_picture(&self.path);
        let kept = picture && stamp == work.stamp && matches!(self.body, Body::Picture(_));
        self.kind = patch.kind;
        work.stamp = stamp;
        if kept {
            return;
        }
        self.body = working_body(&work.absolute, patch, picture, syntax::for_file(&self.path));
    }
}

fn working_body(path: &Path, patch: FileDiff, picture: bool, lang: Option<&'static syntax::Lang>) -> Body {
    if !picture {
        if patch.binary {
            return Body::Note(NOT_TEXT);
        }
        if !patch.lines.is_empty() {
            return Body::Text(Text::new(patch.lines, lang));
        }
    }
    let line_kind = if patch.kind == Kind::Untracked { LineKind::Add } else { LineKind::Same };
    let lines = match read::file(path, if picture { picture::FILE_LIMIT } else { LIMIT }) {
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

impl WorkingFile {
    fn file_diff(&mut self, path: &str) -> Option<FileDiff> {
        let kind = if self.repo.is_untracked(path) { Kind::Untracked } else { Kind::None };
        if kind == Kind::Untracked || !self.repo.git {
            return Some(FileDiff { lines: Vec::new(), kind, binary: false });
        }
        let mut diff = diff::parse(&self.repo.file_diff(path, None)?);
        // A file that looks new may have been moved here; compare it with its old path.
        if diff.kind == Kind::Added
            && let Some(from) = self.repo.moved_from(path)
            && let Some(raw) = self.repo.file_diff(path, Some(&from))
        {
            diff = diff::parse(&raw);
        }
        if self.repo.is_unmerged(path) {
            diff.kind = Kind::Conflict;
        }
        self.repo.ok().then_some(diff)
    }
}

/// A patch covers all the lines when there are edits. Otherwise, read the stored file.
fn committed_lines(repo: &Repo, path: &str, hash: &str, file: Option<&PendingFile>) -> Result<Vec<Line>, &'static str> {
    if let Some(file) = file {
        let raw = repo.file_diff_at(hash, file).map_err(|err| match err {
            OutputError::TooLarge => "Too large to show.",
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
        OutputError::Failed => "There is no file here in this commit.",
    })?;
    byte_lines(&raw, LineKind::Same)
}
