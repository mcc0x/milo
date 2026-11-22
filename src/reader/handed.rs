//! Loads a file with only the edits of a handed-over patch marked in it.

use std::path::Path;

use super::content::{self, LIMIT, NOT_FILE, NOT_TEXT};
use super::disk::ReadOnly;
use super::working_path::Root;
use super::{Body, Reader, Source};
use crate::diff::{self, Line};
use crate::git::Repo;
use crate::handoff::{Now, Patch};
use crate::read::Content;
use crate::syntax;

/// Which patch a reader shows, and where its file is among the patch's files.
pub struct Handed {
    /// Who the patch is said to be from. Nothing checks it.
    pub from: Option<String>,
    /// Counted from one.
    pub number: usize,
    pub total: usize,
}

impl Reader {
    /// Shows `path` as it is now, with what the patch says it changed there
    /// and nothing else marked, or the reason that cannot be done. Never editable.
    pub fn handed_over(repo: &Repo, patch: &Patch<'_>, path: &str, handed: Handed) -> Self {
        let (icons, note) = Self::appearance_for(repo);
        // A name the patch made up is shown, never looked for, and may hold anything.
        let name = path.chars().filter(|c| !c.is_control()).collect();
        let mut reader = Self::new(name, Source::Handed(handed), icons, note);
        reader.body = Body::from_lines(handed_lines(repo, patch, path), syntax::for_file(path));
        reader.read_only = Some(ReadOnly::HANDED);
        reader
    }
}

/// The file's lines with the patch's edits, or why they cannot be shown.
/// The file is reached from the project folder through no link, and only read.
pub fn handed_lines(repo: &Repo, patch: &Patch<'_>, path: &str) -> Result<Vec<Line>, &'static str> {
    const UNREADABLE: &str = "This file cannot be read.";
    if let Some(problem) = patch.problem(path) {
        return Err(problem);
    }
    let root = Root::open(&repo.root).map_err(|_| UNREADABLE)?;
    let mut links = 0;
    let entry = root.entry(&repo.root, Path::new(path), &mut links);
    if links != 0 {
        return Err("This file is reached through a link, so it is not shown.");
    }
    let bytes = match entry.as_ref().map_or_else(|error| error.content(), |entry| entry.read(LIMIT)) {
        Content::Bytes(bytes) => Some(bytes),
        Content::Gone => None,
        Content::Link(_) => return Err("This is a link, so it is not shown."),
        Content::TooLarge => return Err("Too large to show."),
        Content::NotFile => return Err(NOT_FILE),
        Content::Unreadable => return Err(UNREADABLE),
    };
    let now = match &bytes {
        Some(bytes) => Now::Text(content::text(bytes).ok_or(NOT_TEXT)?),
        None => Now::Gone,
    };
    let shown = diff::parse(&patch.whole_file_diff(path, now)?);
    Ok(shown.lines)
}
