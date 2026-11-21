//! Process-owned file locks are not inherited by children. The setup lock
//! lasts through a save; the listener lock stays open when milo becomes Rex;
//! the alerts lock lasts as long as the process that mirrors Rex alerts.

use std::ffi::CStr;
use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::fs_ops;

const LISTENER_LOCK: &CStr = c"listener.lock";
const ALERTS_LOCK: &CStr = c"alerts.lock";
/// How long a listener asked to stop is given to wind down.
const WIND_DOWN: Duration = Duration::from_secs(1);
/// How long a process started in the background is given to take its lock.
/// It must first be loaded and find its settings, on a machine that may be busy.
const START: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(10);

/// Never resolve a link while opening a lock that may name a process.
fn open(dir: &File, name: &CStr, flags: libc::c_int) -> io::Result<File> {
    let file = fs_ops::open_at(dir, name, flags, 0o600)?;
    if !file.metadata()?.is_file() {
        return Err(ErrorKind::InvalidInput.into());
    }
    Ok(file)
}

fn create(dir: &Path, name: &CStr) -> io::Result<File> {
    fs::create_dir_all(dir)?;
    open(&fs_ops::open_dir(dir)?, name, libc::O_RDWR | libc::O_CREAT)
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

/// A write lock covering the entire file.
fn whole_file_write_lock() -> libc::flock {
    // SAFETY: A flock is plain numbers, for which all zeros is a valid
    // value: from the start of the file, with no length, meaning to its end.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    // POSIX lock constants are int on Linux, short on macOS; l_type is short on both.
    lock.l_type = libc::F_WRLCK as _;
    lock
}

/// Takes the lock for this process. False if another process holds it.
fn take(file: &File) -> io::Result<bool> {
    let lock = whole_file_write_lock();
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
    let mut lock = whole_file_write_lock();
    // SAFETY: The descriptor is open for the call, and asking about a lock
    // writes one flock, into `lock`.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &raw mut lock) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // The answer keeps the kind "unlocked" when nothing stands in the way.
    Ok((lock.l_type != libc::F_UNLCK as _ && lock.l_pid > 1).then_some(lock.l_pid))
}

fn existing(dir: &Path, name: &CStr) -> io::Result<Option<File>> {
    match fs_ops::open_dir(dir).and_then(|directory| open(&directory, name, libc::O_RDONLY)) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The marker can outlive its process; only a kernel lock proves it is running.
fn held(dir: &Path, name: &CStr) -> io::Result<bool> {
    match existing(dir, name)? {
        Some(lock) => holder(&lock).map(|pid| pid.is_some()),
        None => Ok(false),
    }
}

pub(super) fn running(dir: &Path) -> io::Result<bool> {
    held(dir, LISTENER_LOCK)
}

/// Whether a process is mirroring Rex alerts.
pub(crate) fn alerts_running(dir: &Path) -> io::Result<bool> {
    held(dir, ALERTS_LOCK)
}

/// Makes this process the one that mirrors Rex alerts. None if another
/// already is. Dropping the file, or opening the lock again, gives it up.
pub(crate) fn hold_alerts(dir: &Path) -> io::Result<Option<File>> {
    let lock = create(dir, ALERTS_LOCK)?;
    Ok(take(&lock)?.then_some(lock))
}

/// Ends the process mirroring Rex alerts, if one is running.
pub(crate) fn stop_alerts(dir: &Path) -> io::Result<()> {
    stop_when(dir, ALERTS_LOCK, |asked| asked.elapsed() >= WIND_DOWN)
}

/// A process started in the background needs a moment to take its lock.
pub(crate) fn wait_alerts_running(dir: &Path) -> io::Result<bool> {
    wait_held(dir, ALERTS_LOCK, START)
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

/// Becomes the listener, unless one is already running: then this returns
/// at once. On success it never returns, having turned into `rex`.
pub(super) fn listen(dir: &Path, script: &Path) -> io::Result<()> {
    let lock = create(dir, LISTENER_LOCK)?;
    if !take(&lock)? {
        return Ok(());
    }
    if !fs::symlink_metadata(script)?.is_file() {
        return Err(ErrorKind::InvalidInput.into());
    }
    keep_across_exec(&lock)?;
    Err(crate::rex::exec_listener(script))
}

fn keep_across_exec(lock: &File) -> io::Result<()> {
    // SAFETY: The descriptor is open. Left open in `rex`, it keeps the lock:
    // a process gives its lock up when it closes the file.
    if unsafe { libc::fcntl(lock.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Ends the listener if one is running, and returns once another could
/// start in its place.
pub(super) fn stop(dir: &Path) -> io::Result<()> {
    stop_when(dir, LISTENER_LOCK, |asked| asked.elapsed() >= WIND_DOWN)
}

fn stop_when(dir: &Path, name: &CStr, mut expired: impl FnMut(Instant) -> bool) -> io::Result<()> {
    let Some(lock) = existing(dir, name)? else { return Ok(()) };
    for _ in 0..2 {
        let Some(listener) = holder(&lock)? else { return Ok(()) };
        // SAFETY: The system has just identified the holder of the lock.
        // The only race is PID reuse before the signal arrives.
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
                Some(pid) if pid != listener || expired(asked) => break,
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
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;

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
        create(&f.root, LISTENER_LOCK).unwrap();
        let lock = existing(&f.root, LISTENER_LOCK).unwrap().unwrap();
        // SAFETY: Asking for file status flags does not access user memory.
        let flags = unsafe { libc::fcntl(lock.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        assert_eq!(holder(&lock).unwrap(), None);
        stop(&f.root).unwrap();
    }

    #[test]
    fn stopping_retries_when_the_listener_holds_the_lock_after_the_wait() {
        const ROOT: &str = "MILO_TEST_STOP_RETRY";
        const TEST_WAIT: Duration = Duration::from_secs(30);
        if let Some(root) = std::env::var_os(ROOT) {
            let lock = create(Path::new(&root), LISTENER_LOCK).unwrap();
            assert!(take(&lock).unwrap());
            keep_across_exec(&lock).unwrap();
            let error = Command::new("/bin/sh")
                .arg("-c")
                .arg(
                    "trap 'if [ -f \"$1/term\" ]; then exit; fi; : > \"$1/term\"' TERM\n\
                      : > \"$1/started\"\nwhile :; do /bin/sleep 0.02; done",
                )
                .arg("listener")
                .arg(root)
                .exec();
            panic!("{error}");
        }
        let f = Fixture::folder();
        let test = "setup::lock::tests::stopping_retries_when_the_listener_holds_the_lock_after_the_wait";
        let mut child =
            Command::new(std::env::current_exe().unwrap()).args(["--exact", test]).env(ROOT, &f.root).spawn().unwrap();
        let started = f.root.join("started");
        let first_signal = f.root.join("term");
        let deadline = Instant::now() + TEST_WAIT;
        while !started.exists() && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        let mut retried = false;
        let stopped = started.exists()
            && stop_when(&f.root, LISTENER_LOCK, |asked| {
                // Expire the first grace period only once the child has handled the signal.
                if !retried && first_signal.exists() {
                    retried = true;
                    return true;
                }
                asked.elapsed() >= TEST_WAIT
            })
            .is_ok();
        if !stopped {
            let _ = child.kill();
        }
        assert!(child.wait().unwrap().success() && stopped && retried);
    }
}
