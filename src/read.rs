//! Reading a file's content without trusting what the path leads to.

use std::fs::{self, OpenOptions};
use std::io::{self, Read as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub enum Content {
    Bytes(Vec<u8>),
    /// The path a symbolic link points to, not what is there.
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

/// Reads up to `limit` bytes of a regular file, or returns a link's target path.
///
/// The file is opened without following links or waiting on pipes. Once open,
/// it is checked to be a regular file before any bytes are read.
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
