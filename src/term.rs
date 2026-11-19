//! Sets terminal modes and waits for keyboard input, signals or file notifications.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, IsTerminal, PipeReader, Read, Write};
use std::os::fd::{AsFd, AsRawFd, IntoRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

/// Redraw at the current size after startup or a resize signal.
pub static RESIZED: AtomicBool = AtomicBool::new(true);
/// Wind down after a termination signal, including while waiting on git.
pub static TERMINATED: AtomicBool = AtomicBool::new(false);
/// The write end of the wake pipe, kept open for handlers until the program exits.
static WAKE: AtomicI32 = AtomicI32::new(-1);

// Use the alternate screen, hide the cursor, ask for mouse and focus reports,
// and ask the terminal to mark pasted text so it cannot act as key presses.
// Turn off wrapping so a row never spills onto the next. Then ask for the
// background colour, picture support, cell size, and terminal type. The
// terminal type reply tells us the terminal has finished answering.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[?2004h\
    \x1b]11;?\x1b\\\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c";
// End a partial control string or synchronized update before changing modes.
const LEAVE: &[u8] = b"\x18\x1b[?2026l\x1b[0m\x1b[?2004l\x1b[?1004l\x1b[?1006l\x1b[?1000l\x1b[?7h\x1b[?25h\x1b[?1049l";

/// Wakes a waiting loop with a byte. A full non-blocking pipe already holds a wakeup.
pub fn wake(fd: RawFd) {
    if fd >= 0 {
        // SAFETY: The byte lives for the call, and write is async-signal-safe.
        unsafe { libc::write(fd, [1u8].as_ptr().cast(), 1) };
    }
}

/// Waits up to `timeout_ms` for descriptors to be ready. Returns the number
/// ready, zero on timeout, or a negative result on failure or interruption.
pub fn wait_ready(fds: &mut [libc::pollfd], timeout_ms: i32) -> i32 {
    let Ok(len) = libc::nfds_t::try_from(fds.len()) else { return -1 };
    // SAFETY: The slice is writable and holds exactly `len` descriptors.
    unsafe { libc::poll(fds.as_mut_ptr(), len, timeout_ms) }
}

pub fn milliseconds(wait: Duration) -> i32 {
    i32::try_from(wait.as_millis() + 1).unwrap_or(i32::MAX)
}

// These handlers only load and store atomics and call write, both async-signal-safe.
extern "C" fn on_winch(_: libc::c_int) {
    RESIZED.store(true, Ordering::Relaxed);
    wake(WAKE.load(Ordering::Relaxed));
}

extern "C" fn on_term(_: libc::c_int) {
    TERMINATED.store(true, Ordering::Relaxed);
    wake(WAKE.load(Ordering::Relaxed));
}

fn install(sig: libc::c_int, handler: extern "C" fn(libc::c_int)) -> io::Result<()> {
    // With no SA_RESTART, signals can also interrupt a wait on descriptors.
    // SAFETY: A zeroed sigaction has no flags and no handler; fill both the
    // handler and the signal mask before passing it to sigaction.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = handler as usize;
    // SAFETY: The mask is writable storage for an empty signal set.
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    // SAFETY: `sa` holds the C signal handler and its mask; no old action is requested.
    let result = unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
    if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Owns the terminal modes until the pane returns or unwinds.
pub struct Session {
    pub keyboard: File,
    output: File,
    signal: PipeReader,
    saved: Option<libc::termios>,
}

impl Session {
    pub fn open() -> io::Result<Self> {
        let stdout = io::stdout();
        if !stdout.is_terminal() {
            return Err(io::Error::other("standard output is not a terminal"));
        }
        let output = terminal_output(stdout.as_raw_fd())?;
        let keyboard = File::from(io::stdin().as_fd().try_clone_to_owned()?);
        Self::enter(keyboard, output, listen()?)
    }

    fn enter(keyboard: File, output: File, signal: PipeReader) -> io::Result<Self> {
        let fd = keyboard.as_raw_fd();
        // SAFETY: A zeroed termios holds valid zero values in its integer fields.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: The keyboard descriptor is open and `saved` is writable.
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: The keyboard descriptor is open; discard input queued before startup.
        unsafe { libc::tcflush(fd, libc::TCIFLUSH) };
        let mut raw = saved;
        // SAFETY: `raw` is a writable copy of the keyboard's terminal modes.
        unsafe { libc::cfmakeraw(&mut raw) };
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: The keyboard descriptor is open and `raw` holds valid terminal modes.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut session = Self { keyboard, output, signal, saved: Some(saved) };
        session.write(ENTER)?;
        Ok(session)
    }

    pub fn wait(&self, timeout_ms: i32, watch: Option<RawFd>, external: Option<RawFd>) -> Wait {
        wait_for(self.keyboard.as_raw_fd(), &self.signal, timeout_ms, [watch, external], false)
    }

    pub fn wait_for_open(&self, timeout_ms: i32, done: RawFd) -> Wait {
        wait_for(self.keyboard.as_raw_fd(), &self.signal, timeout_ms, [Some(done), None], true)
    }

    /// Make progress while the terminal is reading, but let a signal interrupt backpressure.
    pub fn write(&mut self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            if TERMINATED.load(Ordering::Relaxed) {
                return Err(ErrorKind::Interrupted.into());
            }
            match self.output.write(data) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => data = &data[n..],
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    let mut fds = [
                        libc::pollfd { fd: self.output.as_raw_fd(), events: libc::POLLOUT, revents: 0 },
                        libc::pollfd { fd: self.signal.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                    ];
                    if wait_ready(&mut fds, -1) < 0 {
                        let e = io::Error::last_os_error();
                        if e.kind() != ErrorKind::Interrupted {
                            return Err(e);
                        }
                    } else if fds[1].revents != 0 {
                        drain_signal(&self.signal);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn finish(&mut self, data: &[u8]) {
        let _ = self.output.write_all(data);
    }

    pub fn size(&self) -> (usize, usize) {
        let mut ws = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCGWINSZ writes into the live `winsize` record.
        let result = unsafe { libc::ioctl(self.output.as_raw_fd(), libc::TIOCGWINSZ as _, &raw mut ws) };
        if result == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            (usize::from(ws.ws_col), usize::from(ws.ws_row))
        } else {
            (80, 24)
        }
    }

    #[cfg(test)]
    pub fn test_pty() -> (Self, File, io::PipeWriter) {
        let (keyboard, master) = test_pair();
        let output = keyboard.try_clone().unwrap();
        let (signals, writer) = io::pipe().unwrap();
        (Self::enter(keyboard, output, signals).unwrap(), master, writer)
    }

    pub fn restore(&mut self) {
        let Some(saved) = self.saved.take() else { return };
        let fd = self.keyboard.as_raw_fd();
        loop {
            // SAFETY: `saved` holds the keyboard's original modes and lives through the call.
            let result = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
            if result == 0 || io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                break;
            }
        }
        // SAFETY: The keyboard descriptor is open; unread keys do not belong to the shell.
        unsafe { libc::tcflush(fd, libc::TCIFLUSH) };
        if self.output.write_all(LEAVE).is_err_and(|e| e.kind() == ErrorKind::WouldBlock) {
            // Drop pending drawing bytes to make room for the cleanup sequence.
            // SAFETY: The output descriptor is open and this flush does not wait for a reader.
            unsafe { libc::tcflush(self.output.as_raw_fd(), libc::TCOFLUSH) };
            let _ = self.output.write_all(LEAVE);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Open a separate description so non-blocking writes don't change the shell's stdout flags.
fn terminal_output(fd: RawFd) -> io::Result<File> {
    let mut path = [0; 4096];
    #[cfg(target_os = "macos")]
    {
        // ttyname_r walks /dev and can miss the tty while other PTYs open or close.
        // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes to this larger buffer.
        if unsafe { libc::fcntl(fd, libc::F_GETPATH, path.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // SAFETY: `path` provides writable storage for the tty name, including its trailing NUL.
        let result = unsafe { libc::ttyname_r(fd, path.as_mut_ptr().cast(), path.len()) };
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
    }
    let name = CStr::from_bytes_until_nul(&path).map_err(io::Error::other)?;
    let name = std::ffi::OsStr::from_bytes(name.to_bytes());
    OpenOptions::new().write(true).custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK).open(name)
}

/// Report a setup error without claiming the shell's root activity record or
/// waiting on a stalled terminal. The setup screen carries the actual error.
pub(crate) fn setup_status(failed: bool) {
    let stdout = io::stdout();
    if crate::rex::inside()
        && stdout.is_terminal()
        && let Ok(mut out) = terminal_output(stdout.as_raw_fd())
    {
        let record: &[u8] = if failed {
            b"\x1b]7501;id=milo/setup:state=error:app=milo:title=TWlsbyBzZXR1cA==\x1b\\"
        } else {
            b"\x1b]7501;id=milo/setup:state=clear\x1b\\"
        };
        let _ = out.write(record);
    }
}

fn listen() -> io::Result<PipeReader> {
    let (reader, writer) = io::pipe()?;
    // A signal just before a wait must still wake it, without ever
    // blocking the handler or leaving the event loop stuck reading.
    for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
        // SAFETY: `fd` is an open pipe end and F_SETFL takes an integer flag.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    WAKE.store(writer.into_raw_fd(), Ordering::Relaxed);
    install(libc::SIGWINCH, on_winch)?;
    for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT] {
        install(sig, on_term)?;
    }
    Ok(reader)
}

pub enum Wait {
    Input,
    /// The extra descriptor became readable.
    Woken,
    External,
    Timeout,
    Interrupted,
    Closed,
}

fn drain_signal(mut reader: &PipeReader) {
    let mut buf = [0u8; 64];
    while reader.read(&mut buf).is_ok_and(|n| n > 0) {}
}

/// Waits for input, a signal, a watcher, an external source or a timeout.
/// Signals always take priority; loading also prioritizes completion over keystrokes.
fn wait_for(keyboard: RawFd, signal: &PipeReader, timeout_ms: i32, extra: [Option<RawFd>; 2], opening: bool) -> Wait {
    let mut fds = [
        libc::pollfd { fd: keyboard, events: libc::POLLIN, revents: 0 },
        // poll() ignores a negative descriptor.
        libc::pollfd { fd: extra[0].unwrap_or(-1), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: extra[1].unwrap_or(-1), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: signal.as_raw_fd(), events: libc::POLLIN, revents: 0 },
    ];
    let ready = wait_ready(&mut fds, timeout_ms);
    if ready == 0 {
        Wait::Timeout
    } else if ready < 0 {
        if io::Error::last_os_error().kind() == ErrorKind::Interrupted { Wait::Interrupted } else { Wait::Closed }
    } else if fds[3].revents != 0 {
        drain_signal(signal);
        Wait::Interrupted
    } else if opening && fds[1].revents != 0 {
        Wait::Woken
    } else if fds[0].revents & libc::POLLIN != 0 {
        Wait::Input
    } else if fds[0].revents != 0 {
        Wait::Closed
    } else if fds[2].revents != 0 {
        Wait::External
    } else {
        Wait::Woken
    }
}

#[cfg(test)]
fn test_pair() -> (File, File) {
    use std::os::fd::FromRawFd;

    let (mut master, mut slave) = (-1, -1);
    // SAFETY: The descriptor outputs are writable, and the other parameters are optional.
    let result = unsafe {
        libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
    };
    assert_eq!(result, 0, "{}", io::Error::last_os_error());
    // SAFETY: openpty returned ownership of the slave descriptor.
    let keyboard = unsafe { File::from_raw_fd(slave) };
    // SAFETY: openpty returned ownership of the separate master descriptor.
    let master = unsafe { File::from_raw_fd(master) };
    (keyboard, master)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_readiness_takes_priority_over_an_extra_wakeup() {
        let (keyboard, input) = io::pipe().unwrap();
        let (extra, writer) = io::pipe().unwrap();
        let extra = Some(extra.as_raw_fd());
        let (signal, _signal_writer) = io::pipe().unwrap();
        let wait = |opening| wait_for(keyboard.as_raw_fd(), &signal, 0, [extra, None], opening);
        assert!(matches!(wait(false), Wait::Timeout));

        wake(writer.as_raw_fd());
        assert!(matches!(wait(false), Wait::Woken));

        wake(input.as_raw_fd());
        assert!(matches!(wait(false), Wait::Input));
        assert!(matches!(wait(true), Wait::Woken));
    }

    #[test]
    fn observer_updates_and_end_of_stream_wake_the_pane() {
        let (keyboard, input) = io::pipe().unwrap();
        let (watch, changed) = io::pipe().unwrap();
        let (observer, update) = io::pipe().unwrap();
        let (signal, _signal_writer) = io::pipe().unwrap();
        let wait =
            || wait_for(keyboard.as_raw_fd(), &signal, 0, [Some(watch.as_raw_fd()), Some(observer.as_raw_fd())], false);
        wake(changed.as_raw_fd());
        assert!(matches!(wait(), Wait::Woken));
        wake(update.as_raw_fd());
        assert!(matches!(wait(), Wait::External));
        (&observer).read_exact(&mut [0]).unwrap();
        drop(update);
        assert!(matches!(wait(), Wait::External));
        wake(input.as_raw_fd());
        assert!(matches!(wait(), Wait::Input));
    }

    fn modes(terminal: &File) -> (libc::tcflag_t, libc::tcflag_t, libc::tcflag_t, libc::tcflag_t) {
        // SAFETY: A zeroed termios is valid and `tcgetattr` can fill it.
        let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: The descriptor is open and attrs is writable storage.
        assert_eq!(unsafe { libc::tcgetattr(terminal.as_raw_fd(), &mut attrs) }, 0);
        (attrs.c_iflag, attrs.c_oflag, attrs.c_cflag, attrs.c_lflag)
    }

    #[test]
    fn unwinding_restores_keyboard_modes_and_leaves_the_alternate_screen() {
        let (keyboard, mut master) = test_pair();
        let before = modes(&keyboard);
        let result = std::panic::catch_unwind(|| {
            let terminal =
                Session::enter(keyboard.try_clone().unwrap(), keyboard.try_clone().unwrap(), io::pipe().unwrap().0)
                    .unwrap();
            assert_ne!(modes(&terminal.keyboard), before);
            std::panic::resume_unwind(Box::new("panic while drawing"));
        });
        assert!(result.is_err());
        assert_eq!(modes(&keyboard), before);
        let mut output = [0; 512];
        let n = master.read(&mut output).unwrap();
        assert!(output[..n].starts_with(ENTER) && output[..n].ends_with(LEAVE));
    }

    #[test]
    fn a_failed_open_write_still_restores_keyboard_modes() {
        let (keyboard, _master) = test_pair();
        let before = modes(&keyboard);
        let read_only = File::open("/dev/null").unwrap();
        let failed = Session::enter(keyboard.try_clone().unwrap(), read_only, io::pipe().unwrap().0);
        assert!(failed.is_err());
        assert_eq!(modes(&keyboard), before);
    }
}
