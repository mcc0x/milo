//! Runs `milo menubar attention watch`: one frame a line for the menu app, for as long as it listens.
//!
//! The menu app starts this only while the section is on, and it ends itself
//! when the saved choice turns off or cannot be read. On its input a NUL asks
//! it to read the choice again, and `seen BLOCK` marks that pane's row seen.
//! However it ends, the Rex script it started ends with it.

use crate::sys::AsRawFd as _;
use std::io::{self, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use super::board::Board;
use super::{Attention, Show};
use crate::{rex, term, when};

const FIRST_RETRY: Duration = Duration::from_secs(1);
/// The longest wait before asking again for a Rex that was not there.
const SLOWEST_RETRY: Duration = Duration::from_secs(30);
const COMMAND_LIMIT: usize = 128;

fn chosen(file: &Path) -> Option<Attention> {
    Attention::load(file).ok().filter(|choice| choice.show != Show::Off)
}

struct Watch<'a> {
    file: &'a Path,
    choice: Attention,
    board: Board,
    source: Option<rex::AttentionWatch>,
    /// When to ask for Rex again, and how long to wait after that try.
    retry: Instant,
    wait: Duration,
    /// The line of input read so far.
    command: Vec<u8>,
}

impl Watch<'_> {
    /// Starts the Rex script when there is none and the wait since the last try is over.
    fn connect(&mut self) {
        if self.source.is_some() || Instant::now() < self.retry {
            return;
        }
        match rex::AttentionWatch::start() {
            Ok(watch) => self.source = Some(watch),
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.board.missing(),
            Err(_) => self.board.lost(),
        }
        self.retry = Instant::now() + self.wait;
        self.wait = (self.wait * 2).min(SLOWEST_RETRY);
    }

    /// Takes in what the script printed. A script that has ended is a lost Rex.
    fn hear(&mut self) {
        let Some(source) = &mut self.source else { return };
        if !source.read(|line| self.board.line(line, when::now())) {
            self.source = None;
            self.board.lost();
            self.retry = Instant::now() + self.wait;
        }
        if self.board.settled() {
            self.wait = FIRST_RETRY;
        }
    }

    /// Takes in what the menu app wrote. False once the saved choice no longer asks for a watch.
    fn obey(&mut self, bytes: &[u8]) -> bool {
        for &byte in bytes {
            match byte {
                0 => match chosen(self.file) {
                    Some(choice) => self.choice = choice,
                    None => return false,
                },
                b'\n' => {
                    if let Some(block) = self.command.strip_prefix(b"seen ").and_then(|id| str::from_utf8(id).ok()) {
                        self.board.mark_seen(block, self.choice.states);
                    }
                    self.command.clear();
                }
                _ if self.command.len() < COMMAND_LIMIT => self.command.push(byte),
                _ => {}
            }
        }
        true
    }
}

pub(super) fn run(file: &Path) -> io::Result<()> {
    let Some(choice) = chosen(file) else { return Ok(()) };
    // Asked to end, the watch returns, so that the Rex script it holds is ended too.
    let stop = term::termination()?;
    let (input, mut output) = (io::stdin(), io::stdout().lock());
    let board = Board::default();
    let mut watch =
        Watch { file, choice, board, source: None, retry: Instant::now(), wait: FIRST_RETRY, command: Vec::new() };
    let (mut sent, mut frame) = (String::new(), String::new());
    loop {
        watch.connect();
        frame.clear();
        if watch.board.frame(&mut frame, watch.choice) && frame != sent {
            match output.write_all(frame.as_bytes()).and_then(|()| output.flush()) {
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
                result => result?,
            }
            std::mem::swap(&mut sent, &mut frame);
        }
        let heard = watch.source.as_ref().map_or(-1, rex::AttentionWatch::fd);
        let mut fds = [
            crate::sys::pollfd { fd: input.as_raw_fd(), events: crate::sys::POLLIN, revents: 0 },
            // Darwin requires POLLHUP in the mask to report a closed pipe reader.
            crate::sys::pollfd { fd: output.as_raw_fd(), events: crate::sys::POLLHUP, revents: 0 },
            crate::sys::pollfd { fd: stop.as_raw_fd(), events: crate::sys::POLLIN, revents: 0 },
            crate::sys::pollfd { fd: heard, events: crate::sys::POLLIN, revents: 0 },
        ];
        // A script that is attached says when anything changes, so nothing is asked on a timer.
        let left = watch.retry.saturating_duration_since(Instant::now());
        let timeout = if watch.source.is_some() { -1 } else { term::milliseconds(left) };
        if term::wait_ready(&mut fds, timeout) < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
        let [told, listener, stopped, script] = fds.map(|fd| fd.revents != 0);
        if listener || stopped || term::TERMINATED.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        if told {
            let mut bytes = [0u8; 256];
            // SAFETY: Standard input stays open for the whole watch, poll reported it readable,
            // and the buffer is writable for its length.
            let read = unsafe { libc::read(input.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
            match usize::try_from(read) {
                Ok(0) => return Ok(()),
                Ok(read) if !watch.obey(&bytes[..read]) => return Ok(()),
                Ok(_) => {}
                Err(_) if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(io::Error::last_os_error()),
            }
        }
        if script {
            watch.hear();
        }
    }
}
