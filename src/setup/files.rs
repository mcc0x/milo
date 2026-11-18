//! A setup save can touch several files. Keep their old bytes until Rex accepts the save.

use std::borrow::Cow;
use std::env;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Only the user's settings may be links. Edit their existing target, never
/// replace the link or create a new file through a dangling link.
pub(super) fn resolve(path: &Path) -> io::Result<Cow<'_, Path>> {
    match fs::symlink_metadata(path) {
        Ok(info) if info.is_symlink() => fs::canonicalize(path).map(Cow::Owned),
        Ok(_) => Ok(Cow::Borrowed(path)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Cow::Borrowed(path)),
        Err(error) => Err(error),
    }
}

#[derive(PartialEq, Eq)]
pub(super) struct Image {
    pub bytes: Vec<u8>,
    pub mode: u32,
}

impl Image {
    pub(super) fn read(path: &Path) -> io::Result<Option<Self>> {
        let mut file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let info = file.metadata()?;
        if !info.is_file() {
            return Err(ErrorKind::InvalidInput.into());
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some(Self { bytes, mode: info.permissions().mode() & 0o7777 }))
    }
}

pub(super) struct Temp(PathBuf);

impl Temp {
    pub(super) fn new(dir: &Path, bytes: &[u8]) -> io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        for _ in 0..32 {
            let path = dir.join(format!(".milo-{}-{}.lua", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
                Ok(mut file) => {
                    let temp = Self(path);
                    file.write_all(bytes)?;
                    return Ok(temp);
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(ErrorKind::AlreadyExists.into())
    }

    /// Checking and trying a layout do not create the directory it would be installed in.
    pub(super) fn staged(near: &Path, bytes: &[u8]) -> io::Result<Self> {
        let dir = near.ancestors().find(|p| p.is_dir()).map_or_else(env::temp_dir, Path::to_path_buf);
        Self::new(&dir, bytes)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(super) struct Change {
    path: PathBuf,
    before: Option<Image>,
    after: Option<Image>,
}

impl Change {
    pub(super) fn new(path: PathBuf, before: Option<Image>, bytes: Option<Vec<u8>>) -> Option<Self> {
        let after = bytes.map(|bytes| Image { bytes, mode: before.as_ref().map_or(0o600, |file| file.mode) });
        (after != before).then_some(Self { path, before, after })
    }

    fn replace(&self, expected: &Option<Image>, image: Option<&Image>) -> io::Result<()> {
        if &Image::read(&self.path)? != expected {
            return Err(ErrorKind::Interrupted.into());
        }
        if let Some(image) = image {
            let dir = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            fs::create_dir_all(dir)?;
            let file = Temp::new(dir, &image.bytes)?;
            fs::set_permissions(file.path(), Permissions::from_mode(image.mode))?;
            fs::rename(file.path(), &self.path)
        } else {
            fs::remove_file(&self.path)
        }
    }
}

#[cfg(test)]
impl Change {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

fn undo(changes: &[Change]) -> bool {
    let mut restored = true;
    for change in changes.iter().rev() {
        restored &= change.replace(&change.after, change.before.as_ref()).is_ok();
    }
    restored
}

pub(super) fn install(changes: &[Change], accept: impl FnOnce() -> bool) -> Result<(), &'static str> {
    for (i, change) in changes.iter().enumerate() {
        if change.replace(&change.before, change.after.as_ref()).is_err() {
            return Err(if undo(&changes[..i]) {
                "Could not save. No files were changed."
            } else {
                "Could not save or put every file back."
            });
        }
    }
    if accept() {
        Ok(())
    } else if undo(changes) {
        Err("Rex could not reload. Your files were put back.")
    } else {
        Err("Rex could not reload. Could not put every file back.")
    }
}
