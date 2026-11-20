//! Resolves reader paths from a held project directory, never through an unguarded open.

use std::ffi::{CStr, CString, OsString};
use std::fs::{self, File};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use crate::fs_ops::{self, READ_FLAGS};
use crate::read::{self, Content};

pub(super) struct Root {
    dir: File,
    identity: (u64, u64),
}

impl Root {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let dir = fs_ops::open_dir(path)?;
        let metadata = dir.metadata()?;
        Ok(Self { dir, identity: (metadata.dev(), metadata.ino()) })
    }

    /// Git still needs a pathname. Check that it names the directory we are holding.
    pub(super) fn at_path(&self, path: &Path) -> Result<bool, LookupError> {
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_symlink() => Err(LookupError::Outside),
            Ok(m) => Ok(m.is_dir() && self.identity == (m.dev(), m.ino())),
            Err(_) => Ok(false),
        }
    }

    pub(super) fn entry(&self, base: &Path, relative: &Path, links: &mut u8) -> Result<Entry<'_>, LookupError> {
        let name = relative.file_name().ok_or(LookupError::Gone)?;
        let name = CString::new(name.as_bytes()).map_err(|_| LookupError::Unreadable)?;
        let mut dirs = Vec::new();
        if let Some(parent) = relative.parent() {
            walk(&self.dir, base, &mut dirs, parent, links)?;
        }
        let parent = dirs.pop();
        let dir = parent.as_ref().unwrap_or(&self.dir);
        let stat = stat_at(dir, &name).map_err(LookupError::of)?;
        Ok(Entry { root: &self.dir, parent, name, stat })
    }
}

/// Follow directory links explicitly; `..` can only pop a directory already opened here.
fn walk(root: &File, base: &Path, dirs: &mut Vec<File>, path: &Path, links: &mut u8) -> Result<(), LookupError> {
    for part in path.components() {
        let name = match part {
            Component::Normal(name) => CString::new(name.as_bytes()).map_err(|_| LookupError::Unreadable)?,
            Component::CurDir => continue,
            Component::ParentDir if dirs.pop().is_some() => continue,
            _ => return Err(LookupError::Outside),
        };
        let dir = dirs.last().unwrap_or(root);
        let target = match open_at(dir, &name, libc::O_DIRECTORY) {
            Ok(dir) => {
                dirs.push(dir);
                continue;
            }
            Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => match link_at(dir, &name) {
                Err(err) if err.raw_os_error() == Some(libc::EINVAL) => return Err(LookupError::of(e)),
                result => result.map_err(LookupError::of)?,
            },
            Err(e) => return Err(LookupError::of(e)),
        };
        *links += 1;
        if *links > 40 {
            return Err(LookupError::Unreadable);
        }
        let target = if target.is_absolute() {
            let tail = target.strip_prefix(base).map_err(|_| LookupError::Outside)?;
            dirs.clear();
            tail
        } else {
            target.as_path()
        };
        walk(root, base, dirs, target, links)?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LookupError {
    Outside,
    Gone,
    Unreadable,
}

impl LookupError {
    fn of(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => Self::Gone,
            _ => Self::Unreadable,
        }
    }

    pub(super) fn content(self) -> Content {
        match self {
            Self::Gone => Content::Gone,
            Self::Outside | Self::Unreadable => Content::Unreadable,
        }
    }
}

/// Includes file identity and change time: saving by replacement can preserve length and mtime.
#[derive(PartialEq, Eq)]
pub(super) struct Stamp {
    identity: (i128, i128),
    changed: (i128, i128),
}

pub(super) struct Entry<'a> {
    root: &'a File,
    parent: Option<File>,
    name: CString,
    stat: libc::stat,
}

impl Entry<'_> {
    pub(super) fn stamp(&self) -> Stamp {
        Stamp {
            identity: (self.stat.st_dev.into(), self.stat.st_ino.into()),
            changed: (self.stat.st_ctime.into(), self.stat.st_ctime_nsec.into()),
        }
    }

    pub(super) fn is_special(&self) -> bool {
        !matches!(self.stat.st_mode & libc::S_IFMT, libc::S_IFREG | libc::S_IFLNK)
    }

    pub(super) fn read(&self, limit: u64) -> Content {
        let dir = self.parent.as_ref().unwrap_or(self.root);
        let link = || link_at(dir, &self.name).map_or_else(|e| LookupError::of(e).content(), Content::Link);
        match self.stat.st_mode & libc::S_IFMT {
            libc::S_IFLNK => link(),
            libc::S_IFREG => match open_at(dir, &self.name, 0) {
                Ok(file) => read::opened(file, limit),
                Err(e) if e.raw_os_error() == Some(libc::ELOOP) => link(),
                Err(e) => LookupError::of(e).content(),
            },
            _ => Content::NotFile,
        }
    }
}

fn open_at(dir: &File, name: &CStr, flags: libc::c_int) -> io::Result<File> {
    // SAFETY: Both arguments are live; these flags do not create files.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), READ_FLAGS | libc::O_RDONLY | flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: A successful open returns a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn stat_at(dir: &File, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = MaybeUninit::uninit();
    // SAFETY: The descriptor and name are live and the output buffer is writable.
    let result = unsafe { libc::fstatat(dir.as_raw_fd(), name.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: On success fstatat has initialized the stat.
    Ok(unsafe { stat.assume_init() })
}

fn link_at(dir: &File, name: &CStr) -> io::Result<PathBuf> {
    let mut target = [0; libc::PATH_MAX as usize];
    // SAFETY: The descriptor and name are live, and target is writable up to its length.
    let len = unsafe { libc::readlinkat(dir.as_raw_fd(), name.as_ptr(), target.as_mut_ptr().cast(), target.len()) };
    let len = usize::try_from(len).map_err(|_| io::Error::last_os_error())?;
    if len == target.len() {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    Ok(PathBuf::from(OsString::from_vec(target[..len].to_vec())))
}
