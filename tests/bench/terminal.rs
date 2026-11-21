//! A pane running in a terminal of its own, for the timing tests: it answers
//! the questions Milo asks as it starts and finds where each frame ends.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
/// Milo ends every frame it sends with this.
const FRAME_END: &[u8] = b"\x1b[?2026l";
/// The last question Milo asks a terminal as it starts.
const ASKED: &[u8] = b"\x1b[c";
/// A dark background, a cell size and a terminal type, as a terminal answers.
const ANSWERS: &[u8] = b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[6;16;8t\x1b[?62;22c";

pub struct Terminal {
    child: Child,
    terminal: File,
    /// Everything the pane has sent.
    pub output: Vec<u8>,
    chunk: Vec<u8>,
    answered: bool,
}

impl Terminal {
    pub fn start(command: &mut Command, columns: u16, rows: u16) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize { ws_row: rows, ws_col: columns, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: Both descriptor outputs and the size are live, and optional outputs are null.
        let opened =
            unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
        assert_eq!(opened, 0, "{}", io::Error::last_os_error());
        // SAFETY: Successful openpty gives the caller ownership of this descriptor.
        let terminal = unsafe { File::from_raw_fd(master) };
        // SAFETY: The other end is a different descriptor, owned the same way.
        let slave = unsafe { File::from_raw_fd(slave) };
        for fd in [terminal.as_raw_fd(), slave.as_raw_fd()] {
            // SAFETY: The descriptor is open; the pane gets only its end, on its standard streams.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }, 0);
        }
        let child =
            command.stdin(slave.try_clone().unwrap()).stdout(slave.try_clone().unwrap()).stderr(slave).spawn().unwrap();
        Self { child, terminal, output: Vec::new(), chunk: vec![0; 65536], answered: false }
    }

    /// Reads what the pane has sent, waiting until `until` for more.
    fn read(&mut self, until: Instant) -> bool {
        let mut ready = libc::pollfd { fd: self.terminal.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let wait = i32::try_from(left.as_millis()).unwrap();
            // SAFETY: The single live pollfd points to an open descriptor.
            match unsafe { libc::poll(&mut ready, 1, wait) } {
                -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
                1 => break,
                _ => return false,
            }
        }
        let count = loop {
            match self.terminal.read(&mut self.chunk) {
                Ok(count) => break count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                // A terminal whose program has gone has nothing more to say.
                Err(_) => return false,
            }
        };
        self.output.extend_from_slice(&self.chunk[..count]);
        if !self.answered && find(&self.output, ASKED).is_some() {
            self.terminal.write_all(ANSWERS).unwrap();
            self.answered = true;
        }
        count > 0
    }

    /// Waits for a frame to end at or after `from` and returns where it ends.
    fn frame_end(&mut self, from: usize) -> Option<usize> {
        let until = Instant::now() + WAIT;
        loop {
            if let Some(end) = find(&self.output[from..], FRAME_END) {
                return Some(from + end + FRAME_END.len());
            }
            if !self.read(until) {
                return None;
            }
        }
    }

    /// Waits for the end of the first frame at or after `from` that shows `text`.
    pub fn frame_showing(&mut self, from: usize, text: &str) -> usize {
        let until = Instant::now() + WAIT;
        loop {
            let shown = find(&self.output[from..], text.as_bytes()).map(|at| from + at);
            if let Some(end) = shown.and_then(|at| self.frame_end(at)) {
                return end;
            }
            if shown.is_some() || !self.read(until) {
                panic!("the pane never showed {text:?}: {:?}", String::from_utf8_lossy(&self.output[from..]));
            }
        }
    }

    /// Lets a repaint that follows the terminal's answers finish before timing keys.
    pub fn settle(&mut self) {
        while self.read(Instant::now() + Duration::from_millis(40)) {}
    }

    /// Sends keys and returns how long the next frame took to arrive.
    pub fn press(&mut self, keys: &[u8]) -> Duration {
        let from = self.output.len();
        let began = Instant::now();
        self.terminal.write_all(keys).unwrap();
        let drawn = self.frame_end(from);
        let took = began.elapsed();
        assert!(drawn.is_some(), "no frame after the keys {:?}", String::from_utf8_lossy(keys));
        took
    }

    /// Sends keys and returns how long the first frame that shows `text` took to arrive.
    pub fn press_until(&mut self, keys: &[u8], text: &str) -> Duration {
        let from = self.output.len();
        let began = Instant::now();
        self.terminal.write_all(keys).unwrap();
        self.frame_showing(from, text);
        began.elapsed()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.terminal.write_all(b"q");
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && self.child.try_wait().is_ok_and(|status| status.is_none()) {
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find(bytes: &[u8], wanted: &[u8]) -> Option<usize> {
    bytes.windows(wanted.len()).position(|window| window == wanted)
}
