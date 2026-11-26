//! Resolves reader paths from a held project directory, never through an unguarded open.

use crate::sys::AsRawFd as _;
use std::ffi::{CStr, CString, OsString};
use std::fs::{self, File};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::FromRawFd as _;
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

    /// The directory as originally opened, even if its pathname is later reused.
    pub(super) fn identity(&self) -> (u64, u64) {
        self.identity
    }

    pub(super) fn same_directory(&self, other: &Self) -> bool {
        self.identity == other.identity
    }

    pub(super) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { dir: self.dir.try_clone()?, identity: self.identity })
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    identity: (i128, i128),
    changed: (i128, i128),
    file_type: libc::mode_t,
}

impl Stamp {
    fn of_stat(stat: &libc::stat) -> Self {
        Self {
            identity: (stat.st_dev.into(), stat.st_ino.into()),
            changed: (stat.st_ctime.into(), stat.st_ctime_nsec.into()),
            file_type: stat.st_mode & libc::S_IFMT,
        }
    }

    pub(super) fn at(path: &Path) -> Option<Self> {
        Self::regular(fs::symlink_metadata(path).ok()?)
    }

    pub(super) fn of_file(file: &File) -> Option<Self> {
        Self::regular(file.metadata().ok()?)
    }

    fn regular(stat: fs::Metadata) -> Option<Self> {
        stat.is_file().then(|| Self {
            identity: (stat.dev().into(), stat.ino().into()),
            changed: (stat.ctime().into(), stat.ctime_nsec().into()),
            file_type: libc::S_IFREG,
        })
    }
}

pub(super) struct Entry<'a> {
    root: &'a File,
    parent: Option<File>,
    name: CString,
    stat: libc::stat,
}

impl Entry<'_> {
    pub(super) fn stamp(&self) -> Stamp {
        Stamp::of_stat(&self.stat)
    }

    pub(super) fn is_special(&self) -> bool {
        !matches!(self.stat.st_mode & libc::S_IFMT, libc::S_IFREG | libc::S_IFLNK)
    }

    pub(super) fn is_regular(&self) -> bool {
        self.stat.st_mode & libc::S_IFMT == libc::S_IFREG
    }

    pub(super) fn is_dataless(&self) -> bool {
        dataless(&self.stat)
    }

    pub(super) fn is_link(&self) -> bool {
        self.stat.st_mode & libc::S_IFMT == libc::S_IFLNK
    }

    /// Opens the file to read and write through the held directory. A link
    /// that has taken the file's place is refused, not followed.
    pub(super) fn open_edit(&self) -> io::Result<File> {
        open_at(self.parent.as_ref().unwrap_or(self.root), &self.name, libc::O_RDWR)
    }

    /// Opens the file to read only, the same way.
    pub(super) fn open_read(&self) -> io::Result<File> {
        open_at(self.parent.as_ref().unwrap_or(self.root), &self.name, 0)
    }

    /// Transfers a guarded pathname to the source lane without opening its data.
    pub(super) fn source(self) -> io::Result<ReadTarget> {
        let stamp = self.stamp();
        let size = self.size();
        let dir = match self.parent {
            Some(parent) => parent,
            None => self.root.try_clone()?,
        };
        Ok(ReadTarget { dir, name: self.name, stamp, size })
    }

    /// Whether the user may write to the file, asked without opening it: opening
    /// a file to write and closing it looks like a write to whoever watches it.
    pub(super) fn writable(&self) -> bool {
        let dir = self.parent.as_ref().unwrap_or(self.root);
        // SAFETY: The descriptor and name are live; the call only reports on the file.
        unsafe { libc::faccessat(dir.as_raw_fd(), self.name.as_ptr(), libc::W_OK, 0) == 0 }
    }

    /// The file's length when it was looked up.
    pub(super) fn size(&self) -> u64 {
        u64::try_from(self.stat.st_size).unwrap_or(0)
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

pub(super) struct ReadTarget {
    dir: File,
    name: CString,
    stamp: Stamp,
    pub(super) size: u64,
}

impl ReadTarget {
    /// Refuse data-less placeholders before an open can request materialization.
    pub(super) fn open(&self) -> Result<File, &'static str> {
        const UNREADABLE: &str = "This file cannot be read.";
        let stat = stat_at(&self.dir, &self.name).map_err(|_| UNREADABLE)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG || Stamp::of_stat(&stat) != self.stamp {
            return Err(UNREADABLE);
        }
        if dataless(&stat) {
            return Err("Download this file before opening its preview.");
        }
        let file = open_at(&self.dir, &self.name, 0).map_err(|_| UNREADABLE)?;
        if !self.matches(&file) {
            return Err(UNREADABLE);
        }
        Ok(file)
    }

    pub(super) fn matches(&self, file: &File) -> bool {
        Stamp::of_file(file) == Some(self.stamp)
    }
}

fn dataless(_stat: &libc::stat) -> bool {
    #[cfg(target_os = "macos")]
    {
        // SF_DATALESS from Darwin sys/stat.h; libc does not expose it.
        _stat.st_flags & 0x4000_0000 != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn the_stamp_distinguishes_file_types_with_the_same_identity_and_change_time() {
        let fixture = Fixture::folder();
        fixture.write("notes.md", "# Notes");
        let dir = File::open(&fixture.root).unwrap();
        let mut stat = stat_at(&dir, c"notes.md").unwrap();
        let regular = Stamp::of_stat(&stat);
        assert!(Stamp::at(&fixture.root.join("notes.md")) == Some(regular));
        stat.st_mode = (stat.st_mode & !libc::S_IFMT) | libc::S_IFLNK;
        assert!(regular != Stamp::of_stat(&stat));
    }
}
