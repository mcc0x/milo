//! Reads regular files and symbolic link targets without following the final link.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use crate::fs_ops;

pub enum Content {
    Bytes(Vec<u8>),
    /// The destination written in a symbolic link, not its contents.
    Link(PathBuf),
    /// More than the limit asked for.
    TooLarge,
    /// A directory, device, pipe or socket: nothing to read as a file.
    NotFile,
    Gone,
    Unreadable,
}

fn unavailable(error: io::Error) -> Content {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => Content::Gone,
        _ => Content::Unreadable,
    }
}

/// Reads a regular file up to `limit` bytes, or returns a link's destination.
/// Rejects special files before opening and checks the descriptor again before
/// reading, in case the name was replaced with a non-regular file.
pub fn file(path: &Path, limit: u64) -> Content {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_symlink() => return fs::read_link(path).map_or_else(unavailable, Content::Link),
        Ok(m) if m.is_file() => {}
        Ok(_) => return Content::NotFile,
        Err(e) => return unavailable(e),
    }
    let file = match fs_ops::open_read(path) {
        Ok(file) => file,
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return fs::read_link(path).map_or_else(unavailable, Content::Link);
        }
        Err(e) => return unavailable(e),
    };
    let size = match file.metadata() {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => return Content::NotFile,
        Err(e) => return unavailable(e),
    };
    if size > limit {
        return Content::TooLarge;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    // A file can grow after the metadata read.
    match file.take(limit.saturating_add(1)).read_to_end(&mut bytes) {
        Ok(_) if bytes.len() as u64 > limit => Content::TooLarge,
        Ok(_) => Content::Bytes(bytes),
        Err(e) => unavailable(e),
    }
}
