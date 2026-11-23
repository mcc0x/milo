//! Reads a working file for editing and writes it back only where it was read from.
//!
//! A save goes through the open file, so its permissions, owner, links and
//! attributes stay as they were. Before writing, Milo reads the file again.
//! An overwrite confirms one particular version, not anything a different
//! program may have written since the question appeared.

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};

use super::content::LIMIT;
use super::working_path::{Entry, LookupError};

pub(super) const BOM: &str = "\u{feff}";

/// Why a file is shown but cannot be edited, in words that fit a narrow pane after "Read-only:".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ReadOnly(pub(crate) &'static str);

impl ReadOnly {
    pub(super) const COMMIT: Self = Self("this is a commit");
    pub(super) const HANDED: Self = Self("this shows a patch");
    pub(super) const LINK: Self = Self("this is a link");
    pub(super) const LINKED: Self = Self("reached through a link");
    pub(super) const MOVED: Self = Self("the project moved");
    pub(super) const OUTSIDE: Self = Self("outside the project");
    pub(super) const GONE: Self = Self("the file is gone");
    pub(super) const NOT_FILE: Self = Self("not a regular file");
    pub(super) const NOT_TEXT: Self = Self("not text");
    pub(super) const PICTURE: Self = Self("shown as a picture");
    pub(super) const NOT_SHOWN: Self = Self("it cannot be shown");
    pub(super) const TOO_LARGE: Self = Self("too large");
    pub(super) const NO_PERMISSION: Self = Self("no write permission");
    pub(super) const UNREADABLE: Self = Self("it cannot be read");

    pub(super) fn of(error: LookupError) -> Self {
        match error {
            LookupError::Gone => Self::GONE,
            LookupError::Outside => Self::OUTSIDE,
            LookupError::Unreadable => Self::UNREADABLE,
        }
    }
}

/// The identity and exact bytes seen during a read, including any byte-order mark.
#[derive(PartialEq, Eq)]
pub(super) struct Snapshot {
    identity: (u64, u64),
    text: String,
}

impl Snapshot {
    pub(super) fn text(&self) -> &str {
        &self.text
    }
}

/// The file as it was when read: what a save expects to replace.
pub(super) struct Original {
    snapshot: Snapshot,
    /// A mark typed into the draft does not decide whether to restore the original one.
    marked: bool,
}

impl Original {
    /// The file's text exactly as read.
    pub(super) fn read(&self) -> &str {
        self.snapshot.text()
    }

    /// The text to edit, without the byte-order mark a save puts back.
    pub(super) fn text(&self) -> &str {
        let text = self.snapshot.text();
        if self.marked { &text[BOM.len()..] } else { text }
    }

    /// Whether the mark is part of the file's encoding, rather than editable text.
    pub(super) fn marked(&self) -> bool {
        self.marked
    }

    /// The text of the file when it holds the draft.
    pub(super) fn with_mark(&self, mut text: String) -> String {
        if self.marked {
            text.insert_str(0, BOM);
        }
        text
    }
}

pub(super) enum SaveError {
    /// The file no longer holds what was read. Writing needs the user's say.
    Changed,
    /// Nothing was written, for this reason.
    Refused(&'static str),
}

/// Opens the file itself and checks that it is a regular file.
fn open(entry: &Entry<'_>, write: bool) -> io::Result<(File, std::fs::Metadata)> {
    let file = if write { entry.open_edit() } else { entry.open_read() }?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok((file, metadata))
}

/// Reads all of a file that is within the limit.
fn contents(file: &File, size: u64) -> io::Result<Option<Vec<u8>>> {
    if size > LIMIT {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    let mut offset = 0;
    let mut chunk = [0; 1 << 16];
    loop {
        let read = match file.read_at(&mut chunk, offset) {
            Ok(0) => return Ok(Some(bytes)),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        // Check the incoming part too: the file could have grown after opening.
        offset += read as u64;
        if offset > LIMIT {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

/// Reads a file to edit, or says why it stays read-only. The file is not
/// opened for writing until a save.
pub(super) fn load(entry: &Entry<'_>) -> Result<Original, ReadOnly> {
    if entry.is_link() {
        return Err(ReadOnly::LINK);
    }
    if !entry.is_regular() {
        return Err(ReadOnly::NOT_FILE);
    }
    if !entry.writable() {
        return Err(ReadOnly::NO_PERMISSION);
    }
    let (file, meta) = open(entry, false).map_err(|error| match error.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => ReadOnly::NO_PERMISSION,
        io::ErrorKind::NotFound => ReadOnly::GONE,
        io::ErrorKind::InvalidInput => ReadOnly::NOT_FILE,
        // A link put in the file's place since the lookup.
        _ if error.raw_os_error() == Some(libc::ELOOP) => ReadOnly::LINK,
        _ => ReadOnly::UNREADABLE,
    })?;
    let bytes = contents(&file, meta.len()).map_err(|_| ReadOnly::UNREADABLE)?.ok_or(ReadOnly::TOO_LARGE)?;
    if bytes.contains(&0) {
        return Err(ReadOnly::NOT_TEXT);
    }
    let marked = bytes.starts_with(BOM.as_bytes());
    let text = String::from_utf8(bytes).map_err(|_| ReadOnly::NOT_TEXT)?;
    Ok(Original { snapshot: Snapshot { text, identity: (meta.dev(), meta.ino()) }, marked })
}

/// What the file must hold for a save to write over it.
pub(super) enum Basis<'a> {
    /// What the edit started from.
    Original,
    /// The exact version the user agreed to replace.
    Confirmed(&'a Snapshot),
    /// Nothing yet: the draft was not written over what the edit started from, so ask first.
    Unconfirmed,
}

/// Writes the draft, or returns the current snapshot if the file does not hold what `basis` expects.
pub(super) fn save(
    entry: Result<&Entry<'_>, LookupError>,
    original: &mut Original,
    draft: impl FnOnce() -> String,
    basis: Basis<'_>,
) -> Result<Option<Snapshot>, SaveError> {
    let entry = entry.map_err(|error| {
        SaveError::Refused(match error {
            LookupError::Gone => "Not saved: the file is gone.",
            LookupError::Outside => "Not saved: the path leaves the project.",
            LookupError::Unreadable => "Not saved: the file cannot be reached.",
        })
    })?;
    if !entry.is_regular() {
        return Err(SaveError::Refused("Not saved: no longer a regular file."));
    }
    let (file, meta) = open(entry, true).map_err(|error| {
        SaveError::Refused(match error.kind() {
            io::ErrorKind::NotFound => "Not saved: the file is gone.",
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => "Not saved: no write permission.",
            _ => "Not saved: the file cannot be opened.",
        })
    })?;
    let bytes = contents(&file, meta.len())
        .map_err(|_| SaveError::Refused("Not saved: the file cannot be read back."))?
        .ok_or(SaveError::Refused("Not saved: the file is now too large."))?;
    if bytes.contains(&0) {
        return Err(SaveError::Refused("Not saved: the file is no longer text."));
    }
    let text = String::from_utf8(bytes).map_err(|_| SaveError::Refused("Not saved: the file is no longer text."))?;
    let identity = (meta.dev(), meta.ino());
    let current = Snapshot { identity, text };
    let expected = match basis {
        Basis::Original => current == original.snapshot,
        Basis::Confirmed(confirmed) => &current == confirmed,
        Basis::Unconfirmed => false,
    };
    let replacement = expected.then(|| original.with_mark(draft()));
    // Only present snapshots from stable reads, and do not write over an edit that happened during one.
    let after = file.metadata().map_err(|_| SaveError::Refused("Not saved: the file cannot be read back."))?;
    let stamp = |m: &std::fs::Metadata| (m.len(), m.ctime(), m.ctime_nsec(), m.mtime(), m.mtime_nsec());
    if stamp(&after) != stamp(&meta) {
        return Err(SaveError::Refused("Not saved: the file changed while reading. Save again."));
    }
    let Some(text) = replacement else { return Ok(Some(current)) };
    let bytes = text.as_bytes();
    let written = file.write_all_at(bytes, 0).and_then(|()| file.set_len(bytes.len() as u64));
    if written.is_err() {
        // Leave what was there rather than part of each.
        let old = current.text.as_bytes();
        let restored = file.write_all_at(old, 0).and_then(|()| file.set_len(old.len() as u64)).is_ok();
        return Err(SaveError::Refused(if restored {
            "Not saved: writing failed. File is as it was."
        } else {
            "Write failed: check the file before closing."
        }));
    }
    original.snapshot = Snapshot { text, identity };
    if file.sync_all().is_err() {
        return Err(SaveError::Refused("Not confirmed on disk: save again."));
    }
    Ok(None)
}
