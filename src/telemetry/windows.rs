use std::ffi::{CStr, OsString};
use std::fs::{self, File};
use std::io;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{fs_ops, sys};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
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

pub(super) fn curl() -> Option<Command> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut buffer = [0_u16; 260];
    // SAFETY: buffer is writable for exactly the advertised number of UTF-16 code units.
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), 260) };
    let len = usize::try_from(len).ok().filter(|&len| len > 0 && len < buffer.len())?;
    let system = PathBuf::from(OsString::from_wide(&buffer[..len]));
    super::absolute(&system).ok()?;
    let mut command = Command::new(system.join("curl.exe"));
    // The system directory also supplies the working directory for DLL searches.
    // Do not inherit SystemRoot, proxy settings, or certificate overrides.
    command.env_clear().env("SystemRoot", system.parent()?).current_dir(&system).creation_flags(CREATE_NO_WINDOW);
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkout_cannot_substitute_its_own_curl_or_system_root() {
        const CHILD: &str = "MILO_TEST_COUNTS_FAKE_ROOT";
        if let Some(fake) = std::env::var_os(CHILD) {
            let command = curl().unwrap();
            let system = command.get_current_dir().unwrap();
            let program = Path::new(command.get_program());
            assert!(program.is_absolute() && !program.starts_with(&fake));
            assert_eq!(program, system.join("curl.exe"));
            assert_eq!(command.get_envs().count(), 1);
            let (name, root) = command.get_envs().next().unwrap();
            assert_eq!(name, "SystemRoot");
            assert_eq!(root.map(Path::new), system.parent());
            return;
        }
        let f = crate::fixture::Fixture::folder();
        f.write("System32/curl.exe", "Not a system binary.");
        f.write("curl.exe", "Not a system binary.");
        let test = "telemetry::platform::tests::a_checkout_cannot_substitute_its_own_curl_or_system_root";
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test])
            .current_dir(&f.root)
            .env(CHILD, &f.root)
            .env("PATH", &f.root)
            .env("SystemRoot", &f.root)
            .env("WINDIR", &f.root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }

    #[test]
    fn network_and_device_names_do_not_open_state_or_consent() {
        for path in [r"\\server\share\milo", r"\\?\UNC\server\share\milo", r"\\.\pipe\milo", r"C:relative"] {
            assert!(super::super::enabled(Path::new(path)).is_err());
            assert!(lock_file(Path::new(path), c"lock").is_err());
        }
    }
}
