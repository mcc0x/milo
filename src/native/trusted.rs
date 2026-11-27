//! Read-only discovery for programs that must not come from the checkout.

#![cfg_attr(windows, allow(dead_code))]

#[cfg(unix)]
use std::ffi::CString;
use std::ffi::OsStr;
#[cfg(unix)]
use std::fs::Metadata;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};

pub(crate) fn safe_path_text(path: &Path) -> bool {
    path.to_str().is_some_and(|text| !text.chars().any(char::is_control))
}

#[cfg(unix)]
/// Prefer a direct parent reported by the kernel, then examine absolute PATH
/// entries in order. Neither route may traverse the checkout or current directory.
pub(crate) fn find(
    program: &str,
    root: Option<&Path>,
    cwd: &Path,
    parent: Option<&Path>,
    path: &OsStr,
) -> Option<PathBuf> {
    let cwd = directory_id(cwd)?;
    let root = match root {
        Some(path) => Some(directory_id(path)?),
        None => None,
    };
    let trust = ToolTrust {
        // SAFETY: geteuid has no caller preconditions.
        user: unsafe { libc::geteuid() },
        cwd,
        root,
    };
    if let Some(parent) = parent.filter(|file| file.file_name() == Some(OsStr::new(program)))
        && absolute_candidate(parent)
    {
        let mut resolved = PathBuf::new();
        let mut links = 40;
        if let Some(metadata) = trust.resolve(parent, &mut resolved, &mut links)
            && executable(&resolved, &metadata)
        {
            return Some(resolved);
        }
    }
    for dir in std::env::split_paths(path).filter(|dir| absolute_candidate(dir)) {
        let mut resolved = PathBuf::new();
        let mut links = 40;
        if trust.resolve(&dir, &mut resolved, &mut links).is_none_or(|m| !m.is_dir()) {
            continue;
        }
        if let Some(metadata) = trust.resolve(Path::new(program), &mut resolved, &mut links)
            && executable(&resolved, &metadata)
        {
            return Some(resolved);
        }
    }
    None
}

/// Ownership of a program's folders is not checked on Windows yet, so no program is trusted there.
#[cfg(windows)]
pub(crate) fn find(_: &str, _: Option<&Path>, _: &Path, _: Option<&Path>, _: &OsStr) -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn absolute_candidate(path: &Path) -> bool {
    path.is_absolute() && safe_path_text(path) && !path.components().any(|part| part == Component::ParentDir)
}

#[cfg(unix)]
fn executable(path: &Path, metadata: &Metadata) -> bool {
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o6000 != 0
        || !safe_path_text(path)
    {
        return false;
    }
    let Ok(name) = CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: name is a live, terminated pathname. access does not execute it.
    unsafe { libc::access(name.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(unix)]
fn directory_id(path: &Path) -> Option<(u64, u64)> {
    let metadata = path.metadata().ok()?;
    metadata.is_dir().then_some((metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
struct ToolTrust {
    user: libc::uid_t,
    cwd: (u64, u64),
    root: Option<(u64, u64)>,
}

#[cfg(unix)]
impl ToolTrust {
    /// Check each encountered entry, including the lexical parents of symlinks
    /// and the links' destinations. Identity catches alternate spellings on
    /// case-insensitive filesystems as well as symlink aliases of the checkout.
    fn resolve(&self, path: &Path, resolved: &mut PathBuf, links: &mut u8) -> Option<Metadata> {
        let mut parts = path.components().peekable();
        let mut metadata = None;
        while let Some(part) = parts.next() {
            match part {
                Component::RootDir => {
                    resolved.clear();
                    resolved.push("/");
                }
                Component::CurDir => continue,
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::Normal(name) => resolved.push(name),
                Component::Prefix(_) => return None,
            }
            let mut entry = resolved.symlink_metadata().ok()?;
            if entry.uid() != 0 && entry.uid() != self.user {
                return None;
            }
            if entry.is_symlink() {
                *links = links.checked_sub(1)?;
                let target = resolved.read_link().ok()?;
                resolved.pop();
                entry = self.resolve(&target, resolved, links)?;
            }
            if entry.is_dir() {
                let id = (entry.dev(), entry.ino());
                if id == self.cwd || Some(id) == self.root {
                    return None;
                }
            } else if parts.peek().is_some() {
                return None;
            }
            // A sticky shared directory (e.g. /tmp) still protects paths
            // belonging to root or this user, whose ownership we check above.
            let sticky_dir = entry.is_dir() && entry.mode() & 0o1000 != 0;
            if !sticky_dir && entry.mode() & 0o022 != 0 {
                return None;
            }
            metadata = Some(entry);
        }
        match metadata {
            Some(entry) => Some(entry),
            None => resolved.symlink_metadata().ok(), // e.g. link target `.`
        }
    }
}
