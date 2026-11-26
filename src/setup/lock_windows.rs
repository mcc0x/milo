//! Per-user process locks. A lock is the first byte of a file in Milo's state folder, taken with
//! `LockFileEx` for as long as the returned file stays open, and given up when its process ends.
//! Only this user and SYSTEM may open a lock file, and a lock file that anyone else could open is
//! refused, so another user can neither take one of these locks nor make one look held.
//!
//! Windows does not name the process that holds a lock, and has no signal to send it. Whatever
//! needs the holder, such as stopping it, still says it is not supported on Windows yet whenever
//! a holder is running.

use std::ffi::{CStr, c_void};
use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::fs_ops;
use crate::sys::{Existing, private_file};

const LISTENER_LOCK: &CStr = c"listener.lock";
const ALERTS_LOCK: &CStr = c"alerts.lock";
const ALERTS_SUPERVISOR_LOCK: &CStr = c"alerts-supervisor.lock";
/// Held by the one process that may set the user's Discord activity.
pub(crate) const PRESENCE_LOCK: &CStr = c"presence.lock";
/// How long a listener started in the background is given to take its lock.
const WIND_DOWN: Duration = Duration::from_secs(1);
/// How long a process started in the background is given to take its lock.
const START: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(10);

/// Windows has no signal to send; this only keeps the name that Unix code uses.
pub(crate) const REX_STARTED: libc::c_int = 0;

const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x2;
const ERROR_LOCK_VIOLATION: i32 = 33;

#[repr(C)]
#[derive(Default)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LockFileEx(file: *mut c_void, flags: u32, reserved: u32, low: u32, high: u32, at: *mut Overlapped) -> i32;
    fn UnlockFileEx(file: *mut c_void, reserved: u32, low: u32, high: u32, at: *mut Overlapped) -> i32;
}

fn unsupported<T>(what: &str) -> io::Result<T> {
    Err(io::Error::new(ErrorKind::Unsupported, format!("{what} is not supported on Windows yet")))
}

fn path(dir: &Path, name: &CStr) -> io::Result<PathBuf> {
    Ok(dir.join(name.to_str().map_err(|_| ErrorKind::InvalidInput)?))
}

/// Never follows a link to the folder or the file, and refuses a lock file another user could open.
fn create(dir: &Path, name: &CStr) -> io::Result<File> {
    fs::create_dir_all(dir)?;
    fs_ops::open_dir(dir)?;
    private_file(&path(dir, name)?, Existing::Open)
}

fn existing(dir: &Path, name: &CStr) -> io::Result<Option<File>> {
    match fs::symlink_metadata(path(dir, name)?) {
        Ok(_) => create(dir, name).map(Some),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Locks the first byte, exclusively or shared, without waiting. False if another handle holds it.
fn lock(file: &File, flags: u32) -> io::Result<bool> {
    let mut at = Overlapped::default();
    // SAFETY: The handle is open for reading and writing, and the zeroed record names offset zero.
    if unsafe { LockFileEx(file.as_raw_handle(), flags | LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &raw mut at) } != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION) { Ok(false) } else { Err(error) }
}

/// Takes the lock for this handle. False if another process, or another handle, holds it.
fn take(file: &File) -> io::Result<bool> {
    lock(file, LOCKFILE_EXCLUSIVE_LOCK)
}

/// Whether some handle holds the lock. A shared lock is enough to ask, so two processes asking
/// at once do not see each other as holders; it is given up at once.
fn holder_exists(file: &File) -> io::Result<bool> {
    if !lock(file, 0)? {
        return Ok(true);
    }
    let mut at = Overlapped::default();
    // SAFETY: The handle is open, and this handle locked the same byte just above.
    if unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &raw mut at) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(false)
}

/// Keep the marker in place so every setup locks the same file.
pub(super) fn setup(dir: &Path) -> Result<File, &'static str> {
    fn acquire(dir: &Path) -> io::Result<Option<File>> {
        let lock = create(dir, c"setup.lock")?;
        Ok(take(&lock)?.then_some(lock))
    }
    match acquire(dir) {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err("Another setup is saving. Wait a moment, then try again."),
        Err(_) => Err("Could not open Milo's saved settings folder. No files were changed."),
    }
}

/// The marker can outlive its listener; only a held lock proves it is running.
pub(super) fn running(dir: &Path) -> io::Result<bool> {
    held(dir, LISTENER_LOCK)
}

/// Whether a process holds the lock called `name`. Unlike on Unix, a lock this process holds
/// through another file counts too.
pub(crate) fn held(dir: &Path, name: &CStr) -> io::Result<bool> {
    match existing(dir, name)? {
        Some(lock) => holder_exists(&lock),
        None => Ok(false),
    }
}

/// The actual worker. Windows cannot name it, so a running one is reported as unsupported.
pub(crate) fn alerts_worker(dir: &Path) -> io::Result<Option<i32>> {
    if held(dir, ALERTS_LOCK)? { unsupported("Naming the process that mirrors alerts") } else { Ok(None) }
}

pub(crate) fn alerts_supervised(dir: &Path) -> io::Result<bool> {
    held(dir, ALERTS_SUPERVISOR_LOCK)
}

pub(crate) fn hold_alerts(dir: &Path) -> io::Result<Option<File>> {
    hold(dir, ALERTS_LOCK, Duration::ZERO)
}

pub(crate) fn hold_alerts_supervisor(dir: &Path) -> io::Result<Option<File>> {
    hold(dir, ALERTS_SUPERVISOR_LOCK, Duration::ZERO)
}

/// Stop the supervisor before the worker so it cannot race to replace it.
pub(crate) fn stop_alerts(dir: &Path) -> io::Result<()> {
    stop_holder(dir, ALERTS_SUPERVISOR_LOCK)?;
    stop_alerts_worker(dir)
}

pub(crate) fn stop_alerts_worker(dir: &Path) -> io::Result<()> {
    stop_holder(dir, ALERTS_LOCK)
}

pub(crate) fn tell_alerts_worker_rex_started(dir: &Path) -> io::Result<()> {
    alerts_worker(dir).map(drop)
}

/// A process started in the background needs a moment to take its lock.
pub(crate) fn wait_alerts_supervised(dir: &Path) -> io::Result<bool> {
    wait_held(dir, ALERTS_SUPERVISOR_LOCK, START)
}

/// Takes the lock called `name` for as long as the returned file stays open.
/// A holder that is winding down is waited for, up to `patience`.
pub(crate) fn hold(dir: &Path, name: &CStr, patience: Duration) -> io::Result<Option<File>> {
    let lock = create(dir, name)?;
    let asked = Instant::now();
    loop {
        if take(&lock)? {
            return Ok(Some(lock));
        }
        if asked.elapsed() >= patience {
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

/// Rex starts the listener in the background; give it time to take the lock.
pub(super) fn wait_running(dir: &Path) -> io::Result<bool> {
    wait_held(dir, LISTENER_LOCK, WIND_DOWN)
}

fn wait_held(dir: &Path, name: &CStr, limit: Duration) -> io::Result<bool> {
    let started = Instant::now();
    loop {
        if held(dir, name)? {
            return Ok(true);
        }
        if started.elapsed() >= limit {
            return Ok(false);
        }
        std::thread::sleep(POLL);
    }
}

pub(super) fn listen(_dir: &Path, _script: &Path) -> io::Result<()> {
    unsupported("The Rex listener")
}

pub(crate) fn hold_in_child(_command: &mut Command, _dir: &Path, _name: &CStr) -> io::Result<()> {
    unsupported("Handing a lock to another process")
}

/// Ends the listener if one is running.
pub(super) fn stop(dir: &Path) -> io::Result<()> {
    stop_holder(dir, LISTENER_LOCK)
}

/// Nothing to do when no process holds the lock; stopping a holder is not ported.
pub(crate) fn stop_holder(dir: &Path, name: &CStr) -> io::Result<()> {
    if held(dir, name)? { unsupported("Stopping another Milo process") } else { Ok(()) }
}

pub(crate) fn signal_holder(dir: &Path, name: &CStr) -> io::Result<()> {
    stop_holder(dir, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::process::Stdio;

    const REQUEST: &CStr = c"request.lock";
    const BUSY: &str = "Another setup is saving. Wait a moment, then try again.";

    #[test]
    fn a_lock_is_held_by_one_handle_until_it_is_closed() {
        let f = Fixture::folder();
        assert!(!held(&f.root, REQUEST).unwrap());
        assert!(!f.root.join("request.lock").exists(), "asking creates nothing");
        let lock = hold(&f.root, REQUEST, Duration::ZERO).unwrap().unwrap();
        assert!(held(&f.root, REQUEST).unwrap());
        assert!(hold(&f.root, REQUEST, Duration::from_millis(50)).unwrap().is_none());
        assert_eq!(stop_holder(&f.root, REQUEST).unwrap_err().kind(), ErrorKind::Unsupported);
        drop(lock);
        assert!(!held(&f.root, REQUEST).unwrap());
        stop_holder(&f.root, REQUEST).unwrap();
        assert!(hold(&f.root, REQUEST, Duration::ZERO).unwrap().is_some());
    }

    #[test]
    fn only_one_setup_saves_at_a_time() {
        let f = Fixture::folder();
        let first = setup(&f.root).unwrap();
        assert_eq!(setup(&f.root).unwrap_err(), BUSY);
        drop(first);
        setup(&f.root).unwrap();
    }

    #[test]
    fn a_lock_file_is_private_to_this_user() {
        let f = Fixture::folder();
        let lock = hold(&f.root, REQUEST, Duration::ZERO).unwrap().unwrap();
        assert_eq!(crate::sys::file_security(&lock).unwrap(), crate::sys::private_security().unwrap());
    }

    #[test]
    fn a_lock_file_that_others_could_open_is_refused() {
        let f = Fixture::folder();
        // Made with the folder's inherited access list, as a file another user could reach would be.
        fs::write(f.root.join("request.lock"), "").unwrap();
        fs::write(f.root.join("setup.lock"), "").unwrap();
        assert_eq!(hold(&f.root, REQUEST, Duration::ZERO).unwrap_err().kind(), ErrorKind::PermissionDenied);
        assert!(held(&f.root, REQUEST).is_err(), "an unknown holder is never reported as free");
        assert!(setup(&f.root).is_err());
    }

    #[test]
    fn another_process_holds_the_lock_until_it_ends() {
        const ROOT: &str = "MILO_TEST_WINDOWS_LOCK";
        if let Some(root) = std::env::var_os(ROOT) {
            let root = PathBuf::from(root);
            let _lock = hold(&root, REQUEST, Duration::ZERO).unwrap().unwrap();
            let _setup = setup(&root).unwrap();
            fs::write(root.join("taken"), "").unwrap();
            std::thread::sleep(Duration::from_secs(60));
            return;
        }
        let f = Fixture::folder();
        let test = "setup::lock::tests::another_process_holds_the_lock_until_it_ends";
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test])
            .env(ROOT, &f.root)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !f.root.join("taken").exists() && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        let seen = (held(&f.root, REQUEST), hold(&f.root, REQUEST, Duration::ZERO).map(|lock| lock.is_some()));
        let busy = setup(&f.root).err();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(matches!(seen, (Ok(true), Ok(false))), "{seen:?}");
        assert_eq!(busy, Some(BUSY));
        assert!(!held(&f.root, REQUEST).unwrap(), "a lock is given up when its process ends");
        assert!(setup(&f.root).is_ok());
    }
}
