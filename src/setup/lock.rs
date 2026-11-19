//! Process-owned file locks are not inherited by children. The setup lock
//! lasts through a save; the listener lock stays open when milo becomes Rex.

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

const LISTENER_LOCK: &str = "listener.lock";
/// How long a listener asked to stop is given to wind down.
const WIND_DOWN: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(10);

/// Never resolve a link while opening a lock that may name a process.
fn open(path: &Path, create: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(ErrorKind::InvalidInput.into());
    }
    Ok(file)
}

/// Keep the marker in place so every setup locks the same file.
pub(super) fn setup(dir: &Path) -> Result<File, &'static str> {
    fn acquire(dir: &Path) -> io::Result<Option<File>> {
        fs::create_dir_all(dir)?;
        let _directory = crate::fs_ops::open_dir(dir)?;
        let lock = open(&dir.join("setup.lock"), true)?;
        Ok(take(&lock)?.then_some(lock))
    }
    match acquire(dir) {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err("Another setup is saving. Wait a moment, then try again."),
        Err(_) => Err("Could not open Milo's saved settings folder. No files were changed."),
    }
}

/// A lock over a whole file, of the kind asked for.
fn whole_file(kind: libc::c_short) -> libc::flock {
    // SAFETY: A flock is plain numbers, for which all zeros is a valid
    // value: from the start of the file, with no length, meaning to its end.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = kind;
    lock
}

/// Takes the lock for this process. False if another process holds it.
fn take(file: &File) -> io::Result<bool> {
    let lock = whole_file(libc::F_WRLCK);
    loop {
        // SAFETY: The descriptor is open for the call, and `lock` is a whole
        // flock, which is what setting a lock reads.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &raw const lock) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EAGAIN | libc::EACCES) => return Ok(false),
            Some(libc::EINTR) => {}
            _ => return Err(error),
        }
    }
}

/// The process that holds the lock, if another process does.
fn holder(file: &File) -> io::Result<Option<libc::pid_t>> {
    let mut lock = whole_file(libc::F_WRLCK);
    // SAFETY: The descriptor is open for the call, and asking about a lock
    // writes one flock, into `lock`.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &raw mut lock) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // The answer keeps the kind "unlocked" when nothing stands in the way.
    Ok((lock.l_type != libc::F_UNLCK && lock.l_pid > 1).then_some(lock.l_pid))
}

fn existing(dir: &Path) -> io::Result<Option<File>> {
    match open(&dir.join(LISTENER_LOCK), false) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The marker can outlive its listener; only a kernel lock proves it is running.
pub(super) fn running(dir: &Path) -> io::Result<bool> {
    match existing(dir)? {
        Some(lock) => holder(&lock).map(|pid| pid.is_some()),
        None => Ok(false),
    }
}

/// Rex starts the listener in the background; give it time to take the lock.
pub(super) fn wait_running(dir: &Path) -> io::Result<bool> {
    let started = Instant::now();
    loop {
        if running(dir)? {
            return Ok(true);
        }
        if started.elapsed() >= WIND_DOWN {
            return Ok(false);
        }
        std::thread::sleep(POLL);
    }
}

/// Becomes the listener, unless one is already running: then this returns
/// at once. On success it never returns, having turned into `rex`.
pub(super) fn listen(dir: &Path, script: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let lock = open(&dir.join(LISTENER_LOCK), true)?;
    if !take(&lock)? {
        return Ok(());
    }
    if !fs::symlink_metadata(script)?.is_file() {
        return Err(ErrorKind::InvalidInput.into());
    }
    // SAFETY: The descriptor is open. Left open in `rex`, it keeps the lock:
    // a process gives its lock up when it closes the file.
    if unsafe { libc::fcntl(lock.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Err(crate::rex::exec_listener(script))
}

/// Ends the listener if one is running, and returns once another could
/// start in its place.
pub(super) fn stop(dir: &Path) -> io::Result<()> {
    let Some(lock) = existing(dir)? else { return Ok(()) };
    for _ in 0..2 {
        let Some(listener) = holder(&lock)? else { return Ok(()) };
        // SAFETY: The system has just identified the holder of the listener
        // lock. The only race is PID reuse before the signal arrives.
        if unsafe { libc::kill(listener, libc::SIGTERM) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        let asked = Instant::now();
        loop {
            match holder(&lock)? {
                None => return Ok(()),
                Some(pid) if pid != listener || asked.elapsed() >= WIND_DOWN => break,
                Some(_) => std::thread::sleep(POLL),
            }
        }
    }
    Err(ErrorKind::TimedOut.into())
}

// A lock of this kind never stands in the way of the process that holds it,
// so one process cannot play both listeners. What matters here is tested
// with real processes, in `tests/listener.rs`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn stopping_when_no_listener_ever_ran_is_fine() {
        let f = Fixture::folder();
        stop(&f.root.join("never-made")).unwrap();
        stop(&f.root).unwrap();
    }

    #[test]
    fn a_lock_nobody_holds_names_no_process() {
        let f = Fixture::folder();
        let lock = open(&f.root.join(LISTENER_LOCK), true).unwrap();
        assert_eq!(holder(&lock).unwrap(), None);
        stop(&f.root).unwrap();
    }
}
