use std::ffi::CStr;
use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;

use crate::fs_ops;

fn make_directory(path: &Path) -> io::Result<()> {
    if path.try_exists()? {
        return Ok(());
    }
    let parent = path.parent().ok_or(ErrorKind::InvalidInput)?;
    make_directory(parent)?;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    File::open(parent)?.sync_all()
}

pub(super) fn lock_file(dir: &Path, name: &CStr) -> io::Result<File> {
    super::absolute(dir)?;
    make_directory(dir)?;
    let directory = fs_ops::open_dir(dir)?;
    let file = fs_ops::open_at(&directory, name, libc::O_CREAT | libc::O_RDWR, 0o600)?;
    if !file.metadata()?.is_file() {
        return Err(ErrorKind::InvalidData.into());
    }
    Ok(file)
}

pub(super) fn publish(temp: fs_ops::Temp, dir: &Path, name: &str) -> io::Result<()> {
    temp.replace(&dir.join(name))?;
    fs_ops::open_dir(dir)?.sync_all()
}
