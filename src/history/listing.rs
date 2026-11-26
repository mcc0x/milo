//! Reads the files of one commit off the pane's loop, so that a commit that
//! touched thousands of files does not hold the keys back while Git counts.

use crate::sys::{AsRawFd, RawFd};
use std::io;
use std::net::Shutdown;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::git::{CommitFiles, CommitFilesError, DiffFiles};
use crate::term;

type Listed = Result<DiffFiles, CommitFilesError>;

/// One read on its own thread. Its descriptor becomes readable when the result is ready.
pub(super) struct Listing {
    /// The commit being read. Its place in the timeline may change meanwhile.
    pub(super) hash: String,
    ready: UnixStream,
    result: Receiver<Listed>,
    thread: Option<JoinHandle<()>>,
}

impl Listing {
    pub(super) fn start(files: CommitFiles, hash: &str) -> io::Result<Self> {
        let (ready, done) = UnixStream::pair()?;
        let (send, result) = mpsc::sync_channel(1);
        let commit = hash.to_owned();
        let thread = thread::Builder::new().spawn(move || {
            let _ = send.send(files.at(&commit, done.as_fd()));
            // Closing this end wakes the terminal after the result is ready.
            drop(done);
        })?;
        Ok(Self { hash: hash.to_owned(), ready, result, thread: Some(thread) })
    }

    pub(super) fn fd(&self) -> RawFd {
        self.ready.as_raw_fd()
    }

    /// The result, waiting up to `patience` for it. None while Git is still reading.
    pub(super) fn finished(&self, patience: Duration) -> Option<Listed> {
        if !patience.is_zero() {
            let mut ready = [crate::sys::pollfd { fd: self.fd(), events: crate::sys::POLLIN, revents: 0 }];
            term::wait_ready(&mut ready, term::milliseconds(patience));
        }
        match self.result.try_recv() {
            Ok(listed) => Some(listed),
            Err(TryRecvError::Disconnected) => Some(Err(CommitFilesError::Git)),
            Err(TryRecvError::Empty) => None,
        }
    }
}

impl Drop for Listing {
    fn drop(&mut self) {
        // Wake Git's poll before joining the sole worker. Closing the write side
        // also works if the worker has not yet started reading.
        let _ = self.ready.shutdown(Shutdown::Write);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
