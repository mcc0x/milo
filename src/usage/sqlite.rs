//! Runs bounded, read-only SQLite queries against databases other tools keep.

use crate::sys::{AsRawFd, RawFd};
use std::ffi::OsStr;
use std::io::{self, PipeReader, Read};
use std::path::{Path, PathBuf};
use std::process::Child;
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::fs_ops;

use super::files;
use super::signature::Signature;

#[cfg(windows)]
mod windows;
#[cfg(all(test, windows))]
pub(super) use windows::Writer;

const INTERVAL: Duration = Duration::from_secs(20);
/// A database whose files have not changed is still read again this often,
/// in case a write left its files looking as they did.
const BACKSTOP: Duration = Duration::from_secs(5 * 60);
pub(super) const QUERY_WAIT: Duration = Duration::from_secs(1);
const MAX_OUTPUT: usize = 64 * 1024;

/// What a reader found in a database.
pub(super) struct Outcome<T> {
    pub(super) value: T,
    /// The first second at which the same database would read differently,
    /// as when a record dated ahead of the clock comes due or the local day
    /// ends. None when the answer does not stand for the database at all.
    pub(super) stands_until: Option<i64>,
}

/// Reads one database at `path` as of `now`, within the given wait.
pub(super) type Reader<T> = fn(&Path, i64, Duration) -> Option<Outcome<T>>;

/// A read that succeeded on files that were the same before and after it,
/// and had been left alone for a while before it.
struct Settled {
    signature: Signature,
    /// The seconds for which the answer stands, from the read's own.
    from: i64,
    until: i64,
    read: Instant,
}

/// A slow database read runs off the pane's event loop.
pub(super) struct Poll<T> {
    path: Option<PathBuf>,
    wait: Duration,
    read: Reader<T>,
    last: Option<Instant>,
    job: Option<Job<(T, Option<Settled>)>>,
    settled: Option<Settled>,
    /// Whether a read of a database with a write-ahead log may settle.
    with_a_log: bool,
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
        Self { path, wait, read, last: None, job: None, settled: None, with_a_log: true }
    }

    /// For figures that must never be older than the last look: a database
    /// with a write-ahead log is read at every one, as if nothing were known of it.
    pub(super) fn never_settled_with_a_log(mut self) -> Self {
        self.with_a_log = false;
        self
    }

    /// Only the initial read and deliberate reloads announce themselves.
    pub(super) fn checking(&self) -> bool {
        self.job.as_ref().is_some_and(|job| job.checking)
    }

    pub(super) fn fd(&self) -> Option<RawFd> {
        self.job.as_ref().map(Job::fd)
    }

    /// None means there is no completed query; a completed query can also fail.
    /// A failed query settles nothing, so the next look reads again.
    pub(super) fn collect(&mut self) -> Option<Option<T>> {
        let result = self.job.as_ref()?.finished()?;
        self.job = None;
        let (value, settled) = result.map_or((None, None), |(value, settled)| (Some(value), settled));
        self.settled = settled;
        Some(value)
    }

    /// Limit background reads, and skip one whose answer is already known:
    /// the files are as a settled read left them and its answer still stands.
    /// A deliberate reload is held by neither.
    pub(super) fn start(&mut self, now: i64, clock: Instant) {
        if self.last.is_some_and(|last| clock.saturating_duration_since(last) < INTERVAL) {
            return;
        }
        if self.job.is_none() && self.unchanged(now, clock) {
            self.last = Some(clock);
            return;
        }
        self.launch(now, clock, self.last.is_none());
    }

    fn unchanged(&self, now: i64, clock: Instant) -> bool {
        let (Some(settled), Some(path)) = (&self.settled, &self.path) else { return false };
        (settled.from..settled.until).contains(&now)
            && clock.saturating_duration_since(settled.read) < BACKSTOP
            && Signature::of(path).as_ref() == Some(&settled.signature)
    }

    /// Reads at the next look, whatever the files say.
    pub(super) fn forget(&mut self) {
        self.settled = None;
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
        let (read, wait, with_a_log) = (self.read, self.wait, self.with_a_log);
        self.last = Some(clock);
        self.job = Job::start(checking, move || {
            let before = Signature::of(&path);
            let outcome = read(&path, now, wait)?;
            // A write during the read may or may not be in the answer.
            let settled = outcome
                .stands_until
                .zip(before)
                .filter(|(_, before)| with_a_log || !before.write_ahead())
                .filter(|(_, before)| before.quiet(now) && Signature::of(&path).as_ref() == Some(before))
                .map(|(until, signature)| Settled { signature, from: now, until, read: clock });
            Some((outcome.value, settled))
        })
        .ok();
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
    let bytes = path.as_os_str().as_encoded_bytes();
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
/// Windows has no sqlite3 program; its own SQLite library answers there instead.
pub(super) fn query(
    #[cfg_attr(windows, allow(unused_variables))] executable: &OsStr,
    path: &Path,
    sql: &str,
    wait: Duration,
) -> Option<Vec<u8>> {
    if !path.is_absolute() {
        return None;
    }
    // `-nofollow` also rejects links in ancestors, including /tmp on macOS.
    let path = crate::sys::canonicalize(path.parent()?).ok()?.join(path.file_name()?);
    if !readable(&path) {
        return None;
    }
    let deadline = Instant::now() + wait;
    #[cfg(windows)]
    return windows::query(&path, &read_only_uri(&path), sql, deadline, MAX_OUTPUT);
    #[cfg(unix)]
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
    #[cfg(unix)]
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

    fn unused(_: &Path, _: i64, _: Duration) -> Option<Outcome<()>> {
        None
    }

    #[cfg(unix)]
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
                Some(((), None))
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

    #[cfg(unix)]
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

    #[test]
    fn a_read_that_fails_on_unchanged_files_is_tried_again_at_the_next_look() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static READS: AtomicUsize = AtomicUsize::new(0);
        /// Succeeds once, then fails as a busy database does.
        fn once(_: &Path, _: i64, _: Duration) -> Option<Outcome<()>> {
            (READS.fetch_add(1, Ordering::Relaxed) == 0).then_some(Outcome { value: (), stands_until: Some(i64::MAX) })
        }
        fn finish(poll: &mut Poll<()>) -> Option<()> {
            loop {
                if let Some(read) = poll.collect() {
                    return read;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }

        let folder = crate::fixture::Fixture::folder();
        let path = folder.root.join("any.db");
        std::fs::write(&path, [0; 512]).unwrap();
        // The file was written just now; the looks come a minute later.
        let (now, clock) = (crate::when::now() + 60, Instant::now());
        let mut poll = Poll::new(Some(path), QUERY_WAIT, once);
        poll.start(now, clock);
        assert!(finish(&mut poll).is_some());
        poll.start(now, clock + INTERVAL);
        assert!(poll.fd().is_none(), "the files are as the read left them");

        poll.reload(now, clock + INTERVAL);
        assert!(finish(&mut poll).is_none());
        poll.start(now, clock + INTERVAL * 2);
        assert!(poll.fd().is_some(), "a failed read says nothing of files that did not change");
        finish(&mut poll);
    }
}
