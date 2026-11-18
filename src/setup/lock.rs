//! Keeps one `rex do` listener alive across configuration reloads.

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const LOCK: &str = "listener.lock";
// A short lock covers publishing and reading the pid. The long lock can be given up independently.
const GUARD: &str = "listener.guard";

fn open(path: &Path, create: bool) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(create).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)
}

fn flock(file: &File, how: i32) -> io::Result<bool> {
    loop {
        // SAFETY: The descriptor is live for the call; how is a flock operation.
        if unsafe { libc::flock(file.as_raw_fd(), how) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            ErrorKind::WouldBlock => return Ok(false),
            ErrorKind::Interrupted => {}
            _ => return Err(error),
        }
    }
}

const TRY_LOCK: i32 = libc::LOCK_EX | libc::LOCK_NB;

fn begin(dir: &Path, pid: u32) -> io::Result<Option<File>> {
    fs::create_dir_all(dir)?;
    let guard = open(&dir.join(GUARD), true)?;
    if !flock(&guard, TRY_LOCK)? {
        return Ok(None);
    }
    let mut lock = open(&dir.join(LOCK), true)?;
    if !flock(&lock, TRY_LOCK)? {
        return Ok(None);
    }
    lock.set_len(0)?;
    writeln!(lock, "{pid}")?;
    Ok(Some(lock))
}

pub(super) fn listen(dir: &Path, script: &Path) -> io::Result<()> {
    let Some(lock) = begin(dir, std::process::id())? else { return Ok(()) };
    // SAFETY: The descriptor is live. Clearing close-on-exec holds the lock until Rex exits.
    if unsafe { libc::fcntl(lock.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Err(crate::rex::exec_listener(script))
}

fn stop_with(dir: &Path, signal: impl FnOnce(libc::pid_t, &File) -> io::Result<()>) -> io::Result<()> {
    let guard = match open(&dir.join(GUARD), false) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    flock(&guard, libc::LOCK_EX)?;
    let mut lock = match open(&dir.join(LOCK), false) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut text = String::new();
    (&mut lock).take(16).read_to_string(&mut text)?;
    if let Ok(pid) = text.trim().parse::<libc::pid_t>()
        && pid > 1
        && !flock(&lock, TRY_LOCK)?
    {
        signal(pid, &lock)?;
    }
    Ok(())
}

pub(super) fn stop(dir: &Path) -> io::Result<()> {
    stop_with(dir, |pid, lock| {
        // SAFETY: A positive pid was published by a listener and that listener still holds its lock.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(error) };
        }
        // Do not return from a successful stop until another listener could
        // take the lock: up to a second for the old one to wind down.
        for _ in 0..100 {
            if flock(lock, TRY_LOCK)? {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(ErrorKind::TimedOut.into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    /// Whether `happens` comes true within a second. A lock belongs to an
    /// open file, and a program another test is starting at that moment
    /// holds a copy of it until it has started, so a lock can outlive its
    /// holder by an instant. What is promised is that it comes free, not
    /// that it is free at once.
    fn soon(mut happens: impl FnMut() -> bool) -> bool {
        (0..200).any(|_| {
            let done = happens();
            if !done {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            done
        })
    }

    #[test]
    fn only_one_listener_holds_the_lock() {
        let f = Fixture::folder();
        let first = begin(&f.root, 123).unwrap().unwrap();
        assert!(begin(&f.root, 456).unwrap().is_none());
        drop(first);
        assert!(soon(|| begin(&f.root, 456).unwrap().is_some()));
    }

    #[test]
    fn stop_only_signals_a_real_lock_holder() {
        let f = Fixture::folder();
        stop_with(&f.root, |_, _| panic!("no listener has started")).unwrap();
        let holder = begin(&f.root, 123).unwrap().unwrap();
        let mut signaled = None;
        stop_with(&f.root, |pid, _| {
            signaled = Some(pid);
            Ok(())
        })
        .unwrap();
        assert_eq!(signaled, Some(123));
        drop(holder);
        // Once the holder is gone, its recorded process is left alone.
        let left_alone = || {
            let mut signaled = false;
            stop_with(&f.root, |_, _| {
                signaled = true;
                Ok(())
            })
            .unwrap();
            !signaled
        };
        assert!(soon(left_alone));
    }
}
