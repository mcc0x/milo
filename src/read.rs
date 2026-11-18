//! Reading a file's content without trusting what the path leads to.

use std::fs::OpenOptions;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub enum Content {
    Bytes(Vec<u8>),
    /// More than the limit asked for.
    TooLarge,
    /// A directory, device, pipe or socket: nothing to read as a file.
    NotFile,
    Gone,
}

/// Reads a regular file of at most `limit` bytes.
///
/// A name in a repository can be a link to a device or a pipe, which a plain
/// read would wait on forever or never reach the end of. The file is opened
/// without blocking, checked to be a regular file once open, and read no
/// further than the limit.
pub fn file(path: &Path, limit: u64) -> Content {
    let Ok(file) = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path) else {
        return Content::Gone;
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Content::NotFile;
    }
    let mut bytes = Vec::new();
    match file.take(limit + 1).read_to_end(&mut bytes) {
        Ok(_) if bytes.len() as u64 > limit => Content::TooLarge,
        Ok(_) => Content::Bytes(bytes),
        Err(_) => Content::Gone,
    }
}
