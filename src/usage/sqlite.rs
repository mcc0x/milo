//! Runs bounded, read-only SQLite queries against databases other tools keep.

use std::ffi::OsStr;
use std::io::{self, PipeReader, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::fs_ops;

use super::files;

const INTERVAL: Duration = Duration::from_secs(20);
pub(super) const QUERY_WAIT: Duration = Duration::from_secs(1);
const MAX_OUTPUT: usize = 64 * 1024;

/// Reads one database at `path` as of `now`, within the given wait.
pub(super) type Reader<T> = fn(&Path, i64, Duration) -> Option<T>;

/// A slow database read runs off the pane's event loop.
pub(super) struct Poll<T> {
    path: Option<PathBuf>,
    wait: Duration,
    read: Reader<T>,
    last: Option<Instant>,
    job: Option<Job<T>>,
}

/// One read on its own thread. Its descriptor becomes readable when the result is ready.
pub(super) struct Job<T> {
    ready: PipeReader,
    result: Receiver<Option<T>>,
    thread: JoinHandle<()>,
    pub(super) checking: bool,
}

impl<T> Job<T> {
    pub(super) fn fd(&self) -> RawFd {
        self.ready.as_raw_fd()
    }

    /// None while the read is still running; a finished read can also have failed.
    pub(super) fn finished(&self) -> Option<Option<T>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Disconnected) => Some(None),
            Err(TryRecvError::Empty) => None,
        }
    }
}

impl<T: Send + 'static> Job<T> {
    pub(super) fn start(checking: bool, read: impl FnOnce() -> Option<T> + Send + 'static) -> io::Result<Self> {
        let (ready, done) = io::pipe()?;
        let (send, result) = mpsc::sync_channel(1);
        let thread = thread::Builder::new().spawn(move || {
            let _ = send.send(read());
            // Closing the write end wakes the terminal after the result is ready.
            drop(done);
        })?;
        Ok(Self { ready, result, thread, checking })
    }
}

impl<T: Send + 'static> Poll<T> {
    pub(super) fn new(path: Option<PathBuf>, wait: Duration, read: Reader<T>) -> Self {
        Self { path, wait, read, last: None, job: None }
    }

    /// Only the initial read and deliberate reloads announce themselves.
    pub(super) fn checking(&self) -> bool {
        self.job.as_ref().is_some_and(|job| job.checking)
    }

    pub(super) fn fd(&self) -> Option<RawFd> {
        self.job.as_ref().map(Job::fd)
    }

    /// None means there is no completed query; a completed query can also fail.
    pub(super) fn collect(&mut self) -> Option<Option<T>> {
        let result = self.job.as_ref()?.finished()?;
        self.job = None;
        Some(result)
    }

    /// Limit background reads; a deliberate reload is not held by the cooldown.
    pub(super) fn start(&mut self, now: i64, clock: Instant) {
        if self.last.is_some_and(|last| clock.saturating_duration_since(last) < INTERVAL) {
            return;
        }
        self.launch(now, clock, self.last.is_none());
    }

    /// Reuse an in-flight read instead of making the database serve both.
    pub(super) fn reload(&mut self, now: i64, clock: Instant) {
        self.launch(now, clock, true);
    }

    fn launch(&mut self, now: i64, clock: Instant, checking: bool) {
        if let Some(job) = &mut self.job {
            job.checking |= checking;
            return;
        }
        let Some(path) = self.path.clone() else { return };
        let (read, wait) = (self.read, self.wait);
        self.last = Some(clock);
        self.job = Job::start(checking, move || read(&path, now, wait)).ok();
    }
}

impl<T> Drop for Poll<T> {
    fn drop(&mut self) {
        // Let the worker reap SQLite before this process goes away.
        if let Some(job) = self.job.take() {
            let _ = job.thread.join();
        }
    }
}

pub(super) fn executable() -> &'static OsStr {
    OsStr::new("/usr/bin/sqlite3")
}

/// The start of a regular file that begins as a SQLite database does, read
/// without following a link.
fn header(path: &Path) -> Option<[u8; 20]> {
    let mut header = [0; 20];
    let mut file = fs_ops::open_read(path).ok()?;
    let database = file.metadata().is_ok_and(|m| m.is_file())
        && file.read_exact(&mut header).is_ok()
        && &header[..16] == b"SQLite format 3\0";
    database.then_some(header)
}

/// Whether a tool saved a database at `path`. A missing, empty or foreign
/// file under that name is no sign of the tool.
pub(super) fn is_database(path: &Path) -> bool {
    header(path).is_some()
}

/// Skip incomplete WAL file sets rather than opening an unusable database.
fn readable(path: &Path) -> bool {
    let Some(header) = header(path) else { return false };
    match (header[18], header[19]) {
        (1, 1) => true,
        (1 | 2, 2) | (2, 1) => ["-wal", "-shm"].iter().all(|suffix| {
            let mut sidecar = path.as_os_str().to_owned();
            sidecar.push(suffix);
            std::fs::symlink_metadata(sidecar).is_ok_and(|m| m.is_file())
        }),
        _ => false,
    }
}

/// Keep SQLite's URI syntax separate from literal path bytes, including non-UTF-8 names.
fn read_only_uri(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    const OPTIONS: &str = "?mode=ro&readonly_shm=1";
    let bytes = path.as_os_str().as_bytes();
    let mut uri = String::with_capacity(5 + bytes.len() + OPTIONS.len());
    uri.push_str("file:");
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
            uri.push(char::from(b));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[usize::from(b >> 4)]));
            uri.push(char::from(HEX[usize::from(b & 15)]));
        }
    }
    // Keep WAL locking, but never create or write the shared-memory sidecar.
    uri.push_str(OPTIONS);
    uri
}

/// Bound both child output and wall time so a busy database cannot keep a worker alive.
pub(super) fn query(executable: &OsStr, path: &Path, sql: &str, wait: Duration) -> Option<Vec<u8>> {
    if !path.is_absolute() {
        return None;
    }
    // `-nofollow` also rejects links in ancestors, including /tmp on macOS.
    let path = path.parent()?.canonicalize().ok()?.join(path.file_name()?);
    if !readable(&path) {
        return None;
    }
    let deadline = Instant::now() + wait;
    let child = Command::new(executable)
        // The database operand always starts with "file:". Older SQLite CLIs reject "--".
        .args(["-batch", "-safe", "-bail", "-nofollow", "-readonly", "-json", "-init", "/dev/null"])
        .arg(read_only_uri(&path))
        .arg(sql)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    capture(child, MAX_OUTPUT, deadline)
}

/// The output of a child that exits successfully in time. Any other child is killed and reaped.
pub(super) fn capture(mut child: Child, max: usize, deadline: Instant) -> Option<Vec<u8>> {
    let captured = child.stdout.take().and_then(|pipe| files::read_pipe(pipe, max, deadline));
    if let Some(bytes) = captured {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success().then_some(bytes),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
                _ => break,
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unused(_: &Path, _: i64, _: Duration) -> Option<()> {
        None
    }

    #[test]
    fn sqlite_that_closes_stdout_without_exiting_is_killed_and_reaped() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exec 1>&-; read -r line"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = child.stdout.take().unwrap();
        // EOF says the child has closed stdout. Its stdin stays open until we kill it.
        assert_eq!(output.read(&mut [0]).unwrap(), 0);
        child.stdout = Some(output);
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        let deadline = Instant::now() + Duration::from_millis(250);
        assert!(capture(child, MAX_OUTPUT, deadline).is_none());
        assert!(Instant::now() >= deadline);
        // SAFETY: WNOHANG does not block and a null status pointer is allowed.
        assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
    }

    #[test]
    fn an_in_flight_query_can_be_checked_and_reloaded_without_waiting_or_spawning_another() {
        let (finish, gate) = mpsc::channel();
        let mut poll = Poll::new(Some(PathBuf::from("/unused.db")), QUERY_WAIT, unused);
        poll.job = Some(
            Job::start(false, move || {
                gate.recv_timeout(Duration::from_secs(5)).ok()?;
                Some(())
            })
            .unwrap(),
        );
        let fd = poll.fd().unwrap();
        assert!(poll.collect().is_none());
        assert!(!poll.checking());
        poll.reload(0, Instant::now());
        assert_eq!(poll.fd(), Some(fd));
        assert!(poll.checking());
        finish.send(()).unwrap();
    }

    #[test]
    fn closing_usage_waits_for_its_sqlite_worker_to_reap_the_child() {
        let (send, receive) = std::sync::mpsc::channel();
        let job = Job::start(false, move || {
            let mut child = Command::new("/bin/sleep").arg("0.1").spawn().unwrap();
            send.send(child.id()).unwrap();
            child.wait().unwrap();
            None
        })
        .unwrap();
        let mut poll = Poll::new(None, QUERY_WAIT, unused);
        poll.job = Some(job);
        let pid = libc::pid_t::try_from(receive.recv().unwrap()).unwrap();
        drop(poll);
        // SAFETY: WNOHANG does not block and a null status pointer is allowed.
        assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
    }
}
