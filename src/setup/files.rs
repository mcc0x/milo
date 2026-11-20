//! A setup save can touch several files. Keep their old bytes until Rex accepts the save.

use std::borrow::Cow;
use std::env;
use std::fs::{self, File, Metadata, Permissions};
use std::io::{self, ErrorKind, Read, Seek, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::fs_ops::{self, Temp};

/// Setup keeps several snapshots in memory for a transaction and its rollback.
const SNAPSHOT_LIMIT: usize = 4 * 1024 * 1024;

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
        let Some((file, info)) = open(path)? else { return Ok(None) };
        Self::from_open(file, info).map(Some)
    }

    fn from_open(file: File, info: Metadata) -> io::Result<Self> {
        let size = usize::try_from(info.len()).map_err(|_| ErrorKind::FileTooLarge)?;
        if size > SNAPSHOT_LIMIT {
            return Err(ErrorKind::FileTooLarge.into());
        }
        // The file can grow after we read its metadata.
        let mut bytes = Vec::with_capacity(size);
        file.take(SNAPSHOT_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > SNAPSHOT_LIMIT {
            return Err(ErrorKind::FileTooLarge.into());
        }
        Ok(Self { bytes, mode: info.permissions().mode() & 0o7777 })
    }
}

/// Use the same naming for staged Lua and for a settings replacement.
fn temp(dir: &Path, create: impl Fn(PathBuf) -> io::Result<(Temp, File)>) -> io::Result<(Temp, File)> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    for _ in 0..32 {
        let path = dir.join(format!(".milo-{}-{}.lua", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        match create(path) {
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            result => return result,
        }
    }
    Err(ErrorKind::AlreadyExists.into())
}

/// Validation can outlive the setup process. Keep only an open handle, not
/// a named Lua file that a killed setup would leave in the user's config.
pub(super) fn staged(bytes: &[u8]) -> io::Result<File> {
    let (scratch, mut file) = temp(&env::temp_dir(), Temp::create_read_write)?;
    scratch.unlink()?;
    file.write_all(bytes)?;
    file.set_permissions(Permissions::from_mode(0o600))?;
    file.rewind()?;
    Ok(file)
}

/// Only the explicitly owned directory is private; missing ancestors use normal permissions.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    let result = match builder.create(dir) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                fs::create_dir_all(parent)?;
            }
            builder.create(dir)
        }
        result => result,
    };
    match result {
        Err(error) if error.kind() == ErrorKind::AlreadyExists && fs::symlink_metadata(dir)?.is_dir() => Ok(()),
        result => result,
    }
}

/// A planned edit carries the old state, so a concurrent edit is not overwritten.
pub(super) struct Change {
    path: PathBuf,
    before: Option<Image>,
    after: Option<Image>,
    private_parent: bool,
}

impl Change {
    pub(super) fn new(path: PathBuf, before: Option<Image>, bytes: Option<Vec<u8>>) -> Option<Self> {
        let after = bytes.map(|bytes| Image { bytes, mode: before.as_ref().map_or(0o600, |file| file.mode) });
        (after != before).then_some(Self { path, before, after, private_parent: false })
    }

    pub(super) fn in_private_dir(path: PathBuf, before: Option<Image>, bytes: Option<Vec<u8>>) -> Option<Self> {
        let mut change = Self::new(path, before, bytes)?;
        change.private_parent = true;
        Some(change)
    }

    /// Stage before rechecking the old file so the check is close to publication.
    fn replace(&self, expected: &Option<Image>, image: Option<&Image>) -> io::Result<()> {
        let staged = if let Some(image) = image {
            let dir = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            if self.private_parent {
                create_private_dir(dir)?;
            } else {
                fs::create_dir_all(dir)?;
            }
            let (staged, mut file) = temp(dir, Temp::create)?;
            file.write_all(&image.bytes)?;
            file.set_permissions(Permissions::from_mode(image.mode))?;
            Some(staged)
        } else {
            None
        };
        if expected.is_none()
            && let Some(staged) = &staged
        {
            // Publishing a new file must not overwrite a file that just appeared.
            return fs::hard_link(staged.path(), &self.path);
        }
        let current = Image::read(&self.path).map_err(|error| {
            if error.kind() == ErrorKind::FileTooLarge { ErrorKind::Interrupted.into() } else { error }
        })?;
        if &current != expected {
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
    for change in changes {
        if change.before.as_ref().is_some_and(|file| file.mode & 0o222 == 0) {
            return Err("A settings file is read-only. No files were changed.");
        }
        // Every image we publish must fit in a later snapshot, including on rollback.
        if change.after.as_ref().is_some_and(|file| file.bytes.len() > SNAPSHOT_LIMIT) {
            return Err("A settings file would be too large to save. No files were changed.");
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::os::unix::fs::MetadataExt as _;

    #[test]
    fn staged_lua_is_unlinked_and_readable_from_the_start() {
        let lua = b"return true\n";
        let mut file = staged(lua).unwrap();
        let info = file.metadata().unwrap();
        assert_eq!(info.nlink(), 0);
        assert_eq!(info.permissions().mode() & 0o777, 0o600);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, lua);
    }

    #[test]
    fn a_snapshot_accepts_the_limit_but_not_more() {
        let fixture = Fixture::folder();
        let path = fixture.root.join("init.lua");
        let writer = File::create(&path).unwrap();
        writer.set_len(SNAPSHOT_LIMIT as u64).unwrap();
        assert_eq!(Image::read(&path).unwrap().unwrap().bytes.len(), SNAPSHOT_LIMIT);

        writer.set_len(SNAPSHOT_LIMIT as u64 + 1).unwrap();
        assert_eq!(Image::read(&path).err().unwrap().kind(), ErrorKind::FileTooLarge);
    }

    #[test]
    fn a_snapshot_rejects_growth_after_reading_metadata() {
        let fixture = Fixture::folder();
        let path = fixture.root.join("init.lua");
        let writer = File::create(&path).unwrap();
        let (file, info) = open(&path).unwrap().unwrap();
        writer.set_len(SNAPSHOT_LIMIT as u64 + 1).unwrap();
        assert_eq!(Image::from_open(file, info).err().unwrap().kind(), ErrorKind::FileTooLarge);
    }

    #[test]
    fn a_change_that_would_exceed_the_snapshot_limit_leaves_all_files_alone() {
        let fixture = Fixture::folder();
        let first = fixture.root.join("first");
        let second = fixture.root.join("second");
        let changes = [
            Change::new(first.clone(), None, Some(b"new".to_vec())).unwrap(),
            Change::new(second.clone(), None, Some(vec![0; SNAPSHOT_LIMIT + 1])).unwrap(),
        ];
        assert_eq!(
            install(&changes, || panic!("an oversized save reached reload")),
            Err("A settings file would be too large to save. No files were changed."),
        );
        assert!(!first.exists() && !second.exists());
    }
}
