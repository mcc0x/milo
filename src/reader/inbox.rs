//! Carries a request to open one more file to a reader that is already running.
//!
//! A reader in a Rex block listens on a pipe in milo's state folder, named
//! after that block. Whoever opened the reader writes there what `milo view`
//! would have been given: a file, and a commit if it is to be read at one or
//! a line if it is to be shown there. Nothing comes back. A pipe that no
//! reader holds open refuses the request, and that is how a reader that has
//! gone is told from one that is there.

use crate::sys::{AsRawFd as _, RawFd};
use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use super::Spot;
use super::rescue::{hashed, private_dir};

/// More than a path and a commit ID can take. A longer request is dropped whole.
const LIMIT: usize = 8192;
/// What begins a request's second field when it is a place in the working file, not a commit ID.
const SPOT: char = '@';

/// Which version of a file is asked for, and where in it.
#[derive(Clone, Copy)]
pub(super) enum At<'a> {
    /// The working file, from wherever the reader shows it.
    Now,
    /// The working file at a line.
    Spot(Spot),
    /// The file as of a commit, named as it was given.
    Commit(&'a str),
}

impl From<Option<Spot>> for At<'_> {
    fn from(spot: Option<Spot>) -> Self {
        spot.map_or(Self::Now, Self::Spot)
    }
}

/// A file a running reader is asked to show in a tab.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub(super) file: PathBuf,
    /// The commit to read the file at, as it was given.
    pub(super) commit: Option<String>,
    /// The place in the working file to show.
    pub(super) spot: Option<Spot>,
}

fn pipe_path(state: &Path, block: &str) -> PathBuf {
    state.join("readers").join(hashed(block.as_bytes()))
}

/// Where the reader in `block` notes the project it was opened for.
fn project_path(state: &Path, block: &str) -> PathBuf {
    state.join("readers").join(format!("{}.project", hashed(block.as_bytes())))
}

/// The project the reader in `block` noted, for a pane that did not open that
/// reader to tell whether a file belongs in it. A reader that noted none is
/// offered nothing by such a pane.
pub(super) fn project(state: &Path, block: &str) -> Option<PathBuf> {
    let mut note =
        OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(project_path(state, block)).ok()?;
    let mut bytes = Vec::new();
    let whole = note.metadata().ok()?.is_file()
        && (&mut note).take(LIMIT as u64 + 1).read_to_end(&mut bytes).is_ok()
        && !bytes.is_empty()
        && bytes.len() <= LIMIT;
    whole.then(|| OsStr::from_bytes(&bytes).into())
}

/// Opens one end of the pipe at `path` without waiting for the other, and only if it is a pipe.
fn open_end(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    let end = options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW).open(path)?;
    if end.metadata()?.file_type().is_fifo() { Ok(end) } else { Err(io::ErrorKind::InvalidInput.into()) }
}

/// Asks the reader running in `block` to show `file`. False if no reader took the request.
/// A reader from before places were sent says that the place is not a commit ID.
pub(super) fn send(state: &Path, block: &str, file: &Path, at: At<'_>) -> bool {
    let path = pipe_path(state, block);
    let mut pipe = match open_end(&path, OpenOptions::new().write(true)) {
        Ok(pipe) => pipe,
        Err(error) => {
            // No process has the pipe open: its reader died without removing it.
            if error.raw_os_error() == Some(libc::ENXIO) {
                let _ = fs::remove_file(path);
                let _ = fs::remove_file(project_path(state, block));
            }
            return false;
        }
    };
    let file = file.as_os_str().as_bytes();
    let at = match at {
        At::Now => String::new(),
        At::Spot(spot) => format!("{SPOT}{spot}"),
        At::Commit(id) => id.to_owned(),
    };
    let request = [file, b"\0", at.as_bytes(), b"\0"].concat();
    // One write, so that a request is never left half in the pipe.
    request.len() <= LIMIT && pipe.write(&request).is_ok_and(|written| written == request.len())
}

/// The reader's end of its pipe, which lasts as long as the reader does.
pub(super) struct Inbox {
    pipe: File,
    /// Held so that the pipe never reads as ended between two requests.
    _writer: File,
    path: PathBuf,
    project: PathBuf,
    /// Bytes of a request that has not wholly arrived.
    partial: Vec<u8>,
}

impl Inbox {
    /// Starts listening in `block`, and notes the project the reader keeps to, if it has one.
    pub(super) fn open(state: &Path, block: &str, project: Option<&Path>) -> Option<Self> {
        private_dir(state, "readers").ok()?;
        let (path, note) = (pipe_path(state, block), project_path(state, block));
        // Whatever an earlier reader in this block left behind is not this one's.
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&note);
        if let Some(project) = project {
            // Without the note, the pane that opened this reader can still send it files.
            let made = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&note);
            if made.and_then(|mut file| file.write_all(project.as_os_str().as_bytes())).is_err() {
                let _ = fs::remove_file(&note);
            }
        }
        let name = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `name` is a live NUL-terminated string, and the call keeps no pointer to it.
        let ends = if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } == 0 {
            open_end(&path, OpenOptions::new().read(true))
                .and_then(|pipe| Ok((pipe, open_end(&path, OpenOptions::new().write(true))?)))
        } else {
            Err(io::Error::last_os_error())
        };
        match ends {
            Ok((pipe, writer)) => Some(Self { pipe, _writer: writer, path, project: note, partial: Vec::new() }),
            Err(_) => {
                let _ = fs::remove_file(path);
                let _ = fs::remove_file(note);
                None
            }
        }
    }

    /// Readable when a request has arrived.
    pub(super) fn fd(&self) -> RawFd {
        self.pipe.as_raw_fd()
    }

    /// The requests that have arrived whole since the last call, oldest first.
    pub(super) fn take(&mut self) -> Vec<Request> {
        let mut chunk = [0; 4096];
        loop {
            match self.pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => self.partial.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let mut requests = Vec::new();
        let mut rest = self.partial.as_slice();
        while let Some((file, at, after)) = split(rest) {
            rest = after;
            if !file.is_empty()
                && let Ok(at) = std::str::from_utf8(at)
            {
                let file = OsStr::from_bytes(file).into();
                match at.strip_prefix(SPOT) {
                    // A place that cannot be read is no reason to refuse the file.
                    Some(spot) => requests.push(Request { file, commit: None, spot: Spot::parse(spot) }),
                    None => {
                        let commit = (!at.is_empty()).then(|| at.to_owned());
                        requests.push(Request { file, commit, spot: None });
                    }
                }
            }
        }
        let used = self.partial.len() - rest.len();
        self.partial.drain(..used);
        if self.partial.len() > LIMIT {
            self.partial.clear();
        }
        requests
    }
}

/// The two fields of the first request in `bytes`, and what follows it.
fn split(bytes: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let file = bytes.iter().position(|&byte| byte == 0)?;
    let commit = bytes[file + 1..].iter().position(|&byte| byte == 0)?;
    let end = file + 1 + commit;
    Some((&bytes[..file], &bytes[file + 1..end], &bytes[end + 1..]))
}

impl Drop for Inbox {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.project);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    const BLOCK: &str = "block:11111111-1111-1111-1111-111111111111";

    #[test]
    fn a_request_reaches_only_the_reader_in_its_block_and_only_while_that_reader_lives() {
        let f = Fixture::folder();
        let state = f.root.join("state");
        let file = Path::new("/project/odd \n name.txt");
        assert!(!send(&state, BLOCK, file, At::Now), "no reader is listening yet");

        let project = Path::new("/project");
        assert_eq!(super::project(&state, BLOCK), None);
        let mut inbox = Inbox::open(&state, BLOCK, Some(project)).unwrap();
        assert_eq!(super::project(&state, BLOCK).as_deref(), Some(project));
        assert!(inbox.take().is_empty());
        assert!(!send(&state, "block:22222222-2222-2222-2222-222222222222", file, At::Now));
        assert!(send(&state, BLOCK, file, At::Now));
        assert!(send(&state, BLOCK, Path::new("src/main.rs"), At::Commit("a1b2c3d4")));
        let spot = Spot { line: 184, column: Some(17) };
        assert!(send(&state, BLOCK, Path::new("src/disk.rs"), At::Spot(spot)));
        assert_eq!(
            inbox.take(),
            [
                Request { file: file.into(), commit: None, spot: None },
                Request { file: "src/main.rs".into(), commit: Some("a1b2c3d4".into()), spot: None },
                Request { file: "src/disk.rs".into(), commit: None, spot: Some(spot) },
            ]
        );
        assert!(inbox.take().is_empty());

        // A reader that was killed leaves its pipe behind, and nothing is listening on it.
        let path = pipe_path(&state, BLOCK);
        inbox.path = PathBuf::new();
        inbox.project = PathBuf::new();
        drop(inbox);
        assert!(path.exists());
        // Another test's new process can hold a copy of the reader's end for a moment.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while send(&state, BLOCK, file, At::Now) {
            assert!(std::time::Instant::now() < until, "a pipe nobody reads still took a request");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!path.exists(), "the pipe of a reader that is gone is cleared away");
        assert_eq!(super::project(&state, BLOCK), None, "and so is the project it noted");

        // A reader that ends takes its note with it, and one that keeps to no project notes none.
        drop(Inbox::open(&state, BLOCK, Some(project)).unwrap());
        assert_eq!(super::project(&state, BLOCK), None);
        let _unbound = Inbox::open(&state, BLOCK, None).unwrap();
        assert_eq!(super::project(&state, BLOCK), None);
    }

    #[test]
    fn a_request_that_arrives_in_pieces_is_taken_whole_and_an_endless_one_is_dropped() {
        let f = Fixture::folder();
        let mut inbox = Inbox::open(&f.root, BLOCK, None).unwrap();
        let mut writer = open_end(&pipe_path(&f.root, BLOCK), OpenOptions::new().write(true)).unwrap();
        writer.write_all(b"a.txt\0").unwrap();
        assert!(inbox.take().is_empty());
        writer.write_all(b"\0\0\0b.txt\0abcd").unwrap();
        assert_eq!(inbox.take(), [Request { file: "a.txt".into(), commit: None, spot: None }]);
        writer.write_all(b"\0").unwrap();
        assert_eq!(inbox.take(), [Request { file: "b.txt".into(), commit: Some("abcd".into()), spot: None }]);

        // More than a request can be, in pieces that each fit the pipe.
        for _ in 0..3 {
            writer.write_all(&[b'x'; LIMIT / 2]).unwrap();
            assert!(inbox.take().is_empty());
        }
        // A place nobody can read still opens its file.
        writer.write_all(b"c.txt\0\0d.txt\0@12:x\0").unwrap();
        assert_eq!(
            inbox.take(),
            [
                Request { file: "c.txt".into(), commit: None, spot: None },
                Request { file: "d.txt".into(), commit: None, spot: None },
            ]
        );
    }

    #[test]
    fn a_file_left_where_the_pipe_should_be_is_not_written_to() {
        let f = Fixture::folder();
        fs::create_dir_all(f.root.join("readers")).unwrap();
        let path = pipe_path(&f.root, BLOCK);
        fs::write(&path, "").unwrap();
        assert!(!send(&f.root, BLOCK, Path::new("a.txt"), At::Now));
        assert_eq!(fs::read(&path).unwrap(), b"");
    }
}
