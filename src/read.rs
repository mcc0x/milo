//! Reads regular files and symbolic link targets without following the final link.

use std::fs::{self, OpenOptions};
use std::io::{self, Read as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

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
/// Opens the last path component without following links and without waiting
/// on a pipe, then checks that it is a regular file before reading bytes.
pub fn file(path: &Path, limit: u64) -> Content {
    let file = match OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW).open(path) {
        Ok(file) => file,
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) && fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) => {
            return fs::read_link(path).map_or_else(unavailable, Content::Link);
        }
        Err(e) => return unavailable(e),
    };
    match file.metadata() {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Content::NotFile,
        Err(e) => return unavailable(e),
    }
    let mut bytes = Vec::new();
    match file.take(limit + 1).read_to_end(&mut bytes) {
        Ok(_) if bytes.len() as u64 > limit => Content::TooLarge,
        Ok(_) => Content::Bytes(bytes),
        Err(e) => unavailable(e),
    }
}
