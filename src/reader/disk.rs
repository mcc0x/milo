//! Reads a working file for editing and writes it back only where it was read from.
//!
//! A save goes through the open file, so its permissions, owner, links and
//! attributes stay as they were. Before writing, the file is read again: if
//! it no longer holds what the edit started from, nothing is written unless
//! the user says to overwrite it.

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};

use super::content::LIMIT;
use super::working_path::{Entry, LookupError};

const BOM: &str = "\u{feff}";

/// Why a file is shown but cannot be edited, in words that fit a narrow pane after "Read-only:".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ReadOnly(pub(crate) &'static str);

impl ReadOnly {
    pub(super) const COMMIT: Self = Self("this is a commit");
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

/// The file as it was when read: what a save expects to replace.
pub(super) struct Original {
    bytes: Vec<u8>,
    /// The device and file numbers, which a file put in its place does not share.
    identity: (u64, u64),
}

impl Original {
    /// The text to edit, without the byte-order mark a save puts back.
    pub(super) fn text(&self) -> &str {
        let text = std::str::from_utf8(&self.bytes).expect("only UTF-8 files are loaded");
        text.strip_prefix(BOM).unwrap_or(text)
    }

    /// The bytes of the file when it holds `text`.
    pub(super) fn bytes_for(&self, text: &str) -> Vec<u8> {
        let bom = if self.bytes.starts_with(BOM.as_bytes()) { BOM } else { "" };
        [bom.as_bytes(), text.as_bytes()].concat()
    }
}

pub(super) enum SaveError {
    /// The file no longer holds what was read. Writing needs the user's say.
    Changed,
    /// Nothing was written, for this reason.
    Refused(&'static str),
}

/// Text that milo shows and can write back unchanged: UTF-8 with no NUL bytes.
fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// Opens the file itself and checks that it is the regular file the lookup found.
fn open(entry: &Entry<'_>) -> io::Result<(File, (u64, u64))> {
    let file = entry.open_edit()?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok((file, (metadata.dev(), metadata.ino())))
}

/// Reads all of a file that is within the limit.
fn contents(file: &File) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let mut offset = 0;
    let mut chunk = [0; 1 << 16];
    loop {
        let read = match file.read_at(&mut chunk, offset) {
            Ok(0) => return Ok(Some(bytes)),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        bytes.extend_from_slice(&chunk[..read]);
        offset += read as u64;
        if offset > LIMIT {
            return Ok(None);
        }
    }
}

/// Reads a file to edit, or says why it stays read-only.
pub(super) fn load(entry: &Entry<'_>) -> Result<Original, ReadOnly> {
    if entry.is_link() {
        return Err(ReadOnly::LINK);
    }
    if !entry.is_regular() {
        return Err(ReadOnly::NOT_FILE);
    }
    let (file, identity) = open(entry).map_err(|error| match error.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => ReadOnly::NO_PERMISSION,
        io::ErrorKind::NotFound => ReadOnly::GONE,
        io::ErrorKind::InvalidInput => ReadOnly::NOT_FILE,
        // A link put in the file's place since the lookup.
        _ if error.raw_os_error() == Some(libc::ELOOP) => ReadOnly::LINK,
        _ => ReadOnly::UNREADABLE,
    })?;
    let bytes = contents(&file).map_err(|_| ReadOnly::UNREADABLE)?.ok_or(ReadOnly::TOO_LARGE)?;
    if !is_text(&bytes) {
        return Err(ReadOnly::NOT_TEXT);
    }
    Ok(Original { bytes, identity })
}

/// Writes `text` over the file `original` was read from. With `overwrite`, a
/// regular file that has changed, or has been put in its place, is written too.
pub(super) fn save(
    entry: Result<&Entry<'_>, LookupError>,
    original: &mut Original,
    text: &str,
    overwrite: bool,
) -> Result<(), SaveError> {
    let entry = entry.map_err(|error| {
        SaveError::Refused(match error {
            LookupError::Gone => "The file is gone, so nothing was saved.",
            LookupError::Outside => "The path now leads outside the project. Not saved.",
            LookupError::Unreadable => "The file cannot be reached. Not saved.",
        })
    })?;
    if !entry.is_regular() {
        return Err(SaveError::Refused("This is no longer a regular file. Not saved."));
    }
    let (file, identity) = open(entry).map_err(|error| {
        SaveError::Refused(match error.kind() {
            io::ErrorKind::NotFound => "The file is gone, so nothing was saved.",
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => "No write permission. Not saved.",
            _ => "The file cannot be opened to write. Not saved.",
        })
    })?;
    let current = contents(&file).map_err(|_| SaveError::Refused("The file cannot be read back. Not saved."))?;
    let unchanged = identity == original.identity && current.as_deref() == Some(original.bytes.as_slice());
    if !unchanged && !overwrite {
        return Err(SaveError::Changed);
    }
    let bytes = original.bytes_for(text);
    let written = file.write_all_at(&bytes, 0).and_then(|()| file.set_len(bytes.len() as u64));
    if written.is_err() {
        // Leave what was there rather than part of each.
        let restored =
            current.is_some_and(|old| file.write_all_at(&old, 0).and_then(|()| file.set_len(old.len() as u64)).is_ok());
        return Err(SaveError::Refused(if restored {
            "Could not write the file. It is as it was."
        } else {
            "Could not write the file. Check it before closing."
        }));
    }
    *original = Original { bytes, identity };
    if file.sync_all().is_err() {
        return Err(SaveError::Refused("Saved, but the disk did not confirm it."));
    }
    Ok(())
}
