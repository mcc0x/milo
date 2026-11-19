//! A setup save can touch several files. Keep their old bytes until Rex accepts the save.

use std::borrow::Cow;
use std::env;
use std::fs::{self, File, Metadata, Permissions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::fs_ops::{self, Temp};

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

/// Keep both bytes and permissions so a rollback restores the previous file exactly.
#[derive(PartialEq, Eq)]
pub(super) struct Image {
    pub bytes: Vec<u8>,
    pub mode: u32,
}

pub(super) fn open(path: &Path) -> io::Result<Option<(File, Metadata)>> {
    let file = match fs_ops::open_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let info = file.metadata()?;
    if !info.is_file() {
        return Err(ErrorKind::InvalidInput.into());
    }
    Ok(Some((file, info)))
}

impl Image {
    pub(super) fn read(path: &Path) -> io::Result<Option<Self>> {
        let Some((mut file, info)) = open(path)? else { return Ok(None) };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some(Self { bytes, mode: info.permissions().mode() & 0o7777 }))
    }
}

/// Use the same naming for staged Lua and for a settings replacement.
fn temp(dir: &Path, bytes: &[u8], mode: u32) -> io::Result<Temp> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    for _ in 0..32 {
        let path = dir.join(format!(".milo-{}-{}.lua", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        match Temp::create(path) {
            Ok((temp, mut file)) => {
                file.write_all(bytes)?;
                file.set_permissions(Permissions::from_mode(mode))?;
                return Ok(temp);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(ErrorKind::AlreadyExists.into())
}

/// Validation can outlive the setup process. Keep only an open handle, not
/// a named Lua file that a killed setup would leave in the user's config.
pub(super) fn staged(bytes: &[u8]) -> io::Result<File> {
    let scratch = temp(&env::temp_dir(), bytes, 0o600)?;
    fs_ops::open_read(scratch.path())
}

/// A planned edit carries the old state, so a concurrent edit is not overwritten.
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

    /// Stage before rechecking the old file so the check is close to publication.
    fn replace(&self, expected: &Option<Image>, image: Option<&Image>) -> io::Result<()> {
        let staged = if let Some(image) = image {
            let dir = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            fs::create_dir_all(dir)?;
            Some(temp(dir, &image.bytes, image.mode)?)
        } else {
            None
        };
        if expected.is_none()
            && let Some(staged) = &staged
        {
            // Publishing a new file must not overwrite a file that just appeared.
            return fs::hard_link(staged.path(), &self.path);
        }
        if &Image::read(&self.path)? != expected {
            return Err(ErrorKind::Interrupted.into());
        }
        if let Some(staged) = staged { staged.replace(&self.path) } else { fs::remove_file(&self.path) }
    }
}

#[cfg(test)]
impl Change {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

/// Best effort restores every applied change, even if one cannot be restored.
fn undo(changes: &[Change]) -> bool {
    let mut restored = true;
    for change in changes.iter().rev() {
        restored &= change.replace(&change.after, change.before.as_ref()).is_ok();
    }
    restored
}

/// Rex must see the new settings before accepting them, but rejection rolls them back.
pub(super) fn install(changes: &[Change], accept: impl FnOnce() -> bool) -> Result<(), &'static str> {
    if changes.iter().any(|change| change.before.as_ref().is_some_and(|file| file.mode & 0o222 == 0)) {
        return Err("A settings file is read-only. No files were changed.");
    }
    for (i, change) in changes.iter().enumerate() {
        if let Err(error) = change.replace(&change.before, change.after.as_ref()) {
            return Err(if !undo(&changes[..i]) {
                "Could not save or put every file back."
            } else if error.kind() == ErrorKind::PermissionDenied {
                "Could not write to a settings folder. Your files were left as they were."
            } else if matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::AlreadyExists) {
                "A settings file changed while saving. Reopen setup and try again."
            } else {
                "Could not save. No files were changed."
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
