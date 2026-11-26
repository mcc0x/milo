use std::ffi::CStr;
use std::fs::{self, File};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;

use crate::{fs_ops, sys};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
}

pub(super) fn lock_file(dir: &Path, name: &CStr) -> io::Result<File> {
    super::absolute(dir)?;
    fs::create_dir_all(dir)?;
    let directory = fs_ops::open_dir(dir)?;
    let file = sys::private_file(&dir.join(name.to_str().map_err(io::Error::other)?), sys::Existing::Open)?;
    if !sys::inside(&file, &directory)? {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(file)
}

/// Windows cannot flush an ordinary directory handle. Publish the already-flushed
/// sibling with a write-through replacement; never fall back to a copy or deletion.
pub(super) fn publish(temp: fs_ops::Temp, dir: &Path, name: &str) -> io::Result<()> {
    const REPLACE_EXISTING: u32 = 1;
    const WRITE_THROUGH: u32 = 8;
    let from: Vec<_> = temp.path().as_os_str().encode_wide().chain([0]).collect();
    let to: Vec<_> = dir.join(name).as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: Both paths are NUL-terminated and live until MoveFileExW returns.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), REPLACE_EXISTING | WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_and_device_names_do_not_open_state_or_consent() {
        for path in [r"\\server\share\milo", r"\\?\UNC\server\share\milo", r"\\.\pipe\milo", r"C:relative"] {
            assert!(super::super::enabled(Path::new(path)).is_err());
            assert!(lock_file(Path::new(path), c"lock").is_err());
        }
    }
}
