//! Opens untrusted files without following their final link and stages atomic replacements.

use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
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

/// Open a fixed child name through an already-open directory, not its path.
pub(crate) fn open_at(dir: &File, name: &CStr, flags: libc::c_int, mode: libc::c_uint) -> io::Result<File> {
    // SAFETY: Both the directory descriptor and NUL-terminated name remain valid during the call.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags | READ_FLAGS, mode) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: A successful open returns a new, owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

/// Darwin can't apply a practical address-space limit before exec. Sample a decoder's
/// resident set in addition to its input and output limits. Linux video uses RLIMIT_AS.
pub(crate) fn over_memory_budget(child: &std::process::Child) -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
        let size = i32::try_from(size_of::<libc::proc_taskinfo>()).expect("task info fits in a Mach message");
        let Ok(pid) = i32::try_from(child.id()) else { return true };
        // SAFETY: PROC_PIDTASKINFO fills exactly the supplied proc_taskinfo on success.
        let count = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast(), size) };
        if count == size {
            // SAFETY: The return count confirms that every byte of info was written.
            return unsafe { info.assume_init() }.pti_resident_size > 384 << 20;
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = child;
    false
}

/// A private sibling file keeps readers from seeing a half-written replacement.
pub struct Temp(PathBuf);

impl Temp {
    pub fn create(path: PathBuf) -> io::Result<(Self, File)> {
        Self::open(path, false)
    }

    pub(crate) fn create_read_write(path: PathBuf) -> io::Result<(Self, File)> {
        Self::open(path, true)
    }

    fn open(path: PathBuf, read: bool) -> io::Result<(Self, File)> {
        let file = OpenOptions::new().read(read).write(true).create_new(true).mode(0o600).open(&path)?;
        Ok((Self(path), file))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn unlink(mut self) -> io::Result<()> {
        fs::remove_file(&self.0)?;
        self.0.clear();
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn opening_a_child_uses_the_held_directory_after_a_rename() {
        let fixture = Fixture::folder();
        let original = fixture.root.join("original");
        let moved = fixture.root.join("moved");
        fs::create_dir(&original).unwrap();
        let directory = open_dir(&original).unwrap();
        fs::rename(&original, &moved).unwrap();
        fs::create_dir(&original).unwrap();

        open_at(&directory, c"child", libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600).unwrap();
        assert!(moved.join("child").is_file());
        assert!(!original.join("child").exists());
    }
}
