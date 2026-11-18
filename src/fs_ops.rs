//! Opens untrusted files without following their final link and stages atomic replacements.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Do not wait on a pipe, follow a link, or pass the file to a child process.
pub const READ_FLAGS: libc::c_int = libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// The caller checks the file type because different readers report it differently.
pub fn open_read(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(READ_FLAGS).open(path)
}

pub fn open_dir(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(READ_FLAGS | libc::O_DIRECTORY).open(path)
}

/// A private sibling file keeps readers from seeing a half-written replacement.
pub struct Temp(PathBuf);

impl Temp {
    pub fn create(path: PathBuf) -> io::Result<(Self, File)> {
        let file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        Ok((Self(path), file))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn replace(mut self, path: &Path) -> io::Result<()> {
        fs::rename(&self.0, path)?;
        self.0.clear();
        Ok(())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.0);
        }
    }
}
