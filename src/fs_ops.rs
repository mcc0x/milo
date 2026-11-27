//! Opens untrusted files without following their final link, stages atomic replacements,
//! and starts programs without lending one the pipes of another.

use std::fs::{self, File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Mutex, PoisonError};

/// Do not wait on a pipe, follow a link, or pass the file to a child process.
#[cfg(unix)]
pub const READ_FLAGS: libc::c_int = libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// Open a link, junction or other reparse point itself rather than what it leads to, so it can be refused.
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// Needed to open a directory at all.
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// What an open reports when it refused to follow a link: Unix's ELOOP, Windows' reparse point error.
#[cfg(unix)]
pub(crate) const LINK_REFUSED: i32 = libc::ELOOP;
#[cfg(windows)]
pub(crate) const LINK_REFUSED: i32 = ERROR_REPARSE_POINT_ENCOUNTERED;
/// Windows reports this where Unix reports a link that `O_NOFOLLOW` refused.
#[cfg(windows)]
pub(crate) const ERROR_REPARSE_POINT_ENCOUNTERED: i32 = 4319;

/// Windows has no open flag that fails on a link, so the open takes the link itself and it is
/// refused here, along with any other reparse point a program could swap in.
#[cfg(windows)]
fn refuse_reparse_point(file: File) -> io::Result<File> {
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::from_raw_os_error(ERROR_REPARSE_POINT_ENCOUNTERED));
    }
    Ok(file)
}

/// The caller checks the file type because different readers report it differently.
pub fn open_read(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    return OpenOptions::new().read(true).custom_flags(READ_FLAGS).open(path);
    #[cfg(windows)]
    refuse_reparse_point(OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path)?)
}

pub fn open_dir(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    return OpenOptions::new().read(true).custom_flags(READ_FLAGS | libc::O_DIRECTORY).open(path);
    #[cfg(windows)]
    {
        let flags = FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS;
        let dir = match OpenOptions::new().read(true).custom_flags(flags).open(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound && through_a_file(path) => {
                return Err(io::ErrorKind::NotADirectory.into());
            }
            opened => refuse_reparse_point(opened?)?,
        };
        if !dir.metadata()?.is_dir() {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        Ok(dir)
    }
}

/// Windows reports a path that runs through a file as missing, where Unix says it isn't a directory.
#[cfg(windows)]
fn through_a_file(path: &Path) -> bool {
    path.ancestors().skip(1).find_map(|ancestor| fs::symlink_metadata(ancestor).ok()).is_some_and(|info| !info.is_dir())
}

/// The file to lock to hold a whole directory. Unix locks the directory itself. Windows can't
/// lock a directory handle, so it locks a fixed file inside the directory instead: one only this
/// user and SYSTEM may open, which nobody can remove or replace while it is open.
pub fn dir_lock(dir: &Path) -> io::Result<File> {
    #[cfg(unix)]
    return open_dir(dir);
    #[cfg(windows)]
    {
        open_dir(dir)?;
        crate::sys::private_file(&dir.join(".milo.lock"), crate::sys::Existing::Open)
    }
}

/// Waits for the lock on a [`dir_lock`] file. On Windows another user may hold a lock file in a
/// shared folder, so the wait is bounded there rather than stalling a save for ever.
pub fn lock_dir(lock: &File) -> io::Result<()> {
    #[cfg(unix)]
    return lock.lock();
    #[cfg(windows)]
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(()),
                Err(fs::TryLockError::Error(error)) => return Err(error),
                Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "another save is still holding the folder"));
                }
            }
        }
    }
}

/// Open a fixed child name through an already-open directory, not its path.
#[cfg(unix)]
pub(crate) fn open_at(dir: &File, name: &std::ffi::CStr, flags: libc::c_int, mode: libc::c_uint) -> io::Result<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    // SAFETY: Both the directory descriptor and NUL-terminated name remain valid during the call.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags | READ_FLAGS, mode) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: A successful open returns a new, owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

/// Starts a program while no other thread of this process is starting one.
///
/// macOS cannot make a pipe that is closed to other programs from the start: the
/// standard library makes the pipe, then marks it. A program another thread starts in
/// between inherits the pipe and holds it open for as long as it runs, so whoever reads
/// the pipe waits for that program too. One destination's request could then hold up the
/// other destination's, or a reader of a program that has long since ended.
pub(crate) fn spawn(command: &mut Command) -> io::Result<Child> {
    static STARTING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = STARTING.lock().unwrap_or_else(PoisonError::into_inner);
    command.spawn()
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
        #[cfg(unix)]
        let file = OpenOptions::new().read(read).write(true).create_new(true).mode(0o600).open(&path)?;
        // Only this user and SYSTEM may open it, whoever else its folder lets in; it is always readable.
        #[cfg(windows)]
        let file = {
            let _ = read;
            crate::sys::private_file(&path, crate::sys::Existing::Refuse)?
        };
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

    #[cfg(unix)]
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

    #[test]
    fn links_are_refused_rather_than_followed() {
        let fixture = Fixture::folder();
        fixture.write("target.txt", "text");
        fs::create_dir(fixture.root.join("folder")).unwrap();
        assert!(open_read(&fixture.root.join("target.txt")).is_ok());
        assert!(open_dir(&fixture.root.join("folder")).is_ok());
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(fixture.root.join("target.txt"), fixture.root.join("file-link"))
            .and_then(|()| std::os::unix::fs::symlink(fixture.root.join("folder"), fixture.root.join("folder-link")));
        #[cfg(windows)]
        let linked =
            std::os::windows::fs::symlink_file(fixture.root.join("target.txt"), fixture.root.join("file-link"))
                .and_then(|()| {
                    std::os::windows::fs::symlink_dir(fixture.root.join("folder"), fixture.root.join("folder-link"))
                });
        // Creating a link on Windows needs Developer Mode or an elevated user.
        if linked.is_err() && cfg!(windows) {
            return;
        }
        linked.unwrap();
        assert!(open_read(&fixture.root.join("file-link")).is_err());
        assert!(open_dir(&fixture.root.join("folder-link")).is_err());
    }
}
