//! Sets terminal modes and waits for keyboard input, signals or file notifications.

use std::fs::File;
use std::io::{self, ErrorKind, IsTerminal, PipeReader, Read, Write};
use std::os::fd::{AsFd, AsRawFd, IntoRawFd, RawFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Redraw at the current size after startup or a resize signal.
pub static RESIZED: AtomicBool = AtomicBool::new(true);
/// Wind down after a termination signal, including while waiting on git.
pub static TERMINATED: AtomicBool = AtomicBool::new(false);
/// Modes saved for restoration on exit, including from the panic hook.
static ORIG: OnceLock<libc::termios> = OnceLock::new();
/// The write end of the wake pipe, kept open for handlers until the program exits.
static WAKE: AtomicI32 = AtomicI32::new(-1);

// Alternate screen, hidden cursor, mouse buttons in SGR form, focus
// reporting, and no wrapping: a row that turns out wider than reckoned is
// cut at the edge instead of spilling onto the next. Last, the terminal is
// asked what its background colour is; its answer comes in with the keys.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b]11;?\x1b\\";
const LEAVE: &[u8] = b"\x1b[0m\x1b[?1004l\x1b[?1006l\x1b[?1000l\x1b[?7h\x1b[?25h\x1b[?1049l";

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

// These handlers only load and store atomics and call write, both async-signal-safe.
extern "C" fn on_winch(_: libc::c_int) {
    RESIZED.store(true, Ordering::Relaxed);
    wake(WAKE.load(Ordering::Relaxed));
}

extern "C" fn on_term(_: libc::c_int) {
    TERMINATED.store(true, Ordering::Relaxed);
    wake(WAKE.load(Ordering::Relaxed));
}

fn install(sig: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    // With no SA_RESTART, signals can also interrupt a wait on descriptors.
    // SAFETY: A zeroed sigaction has no flags and no handler; fill both the
    // handler and the signal mask before passing it to sigaction.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = handler as usize;
    // SAFETY: The mask is writable storage for an empty signal set.
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    // SAFETY: `sa` holds the C signal handler and its mask; no old action is requested.
    unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
}

pub fn enter() -> io::Result<File> {
    let stdout = io::stdout();
    if !stdout.is_terminal() {
        return Err(io::Error::other("standard output is not a terminal"));
    }
    // Stdout treats a bad descriptor as a successful write. The opening
    // sequence must report that failure so the keyboard can be restored.
    let mut out = File::from(stdout.as_fd().try_clone_to_owned()?);
    let keyboard = File::from(io::stdin().as_fd().try_clone_to_owned()?);
    let fd = keyboard.as_raw_fd();
    // SAFETY: A zeroed termios holds valid zero values in its integer fields.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: The keyboard descriptor is open and `t` is writable.
    if unsafe { libc::tcgetattr(fd, &mut t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut raw = t;
    // SAFETY: `raw` is a writable copy of the keyboard's terminal modes.
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: The keyboard descriptor is open and `raw` holds valid terminal modes.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let _ = ORIG.set(t);
    if let Err(e) = out.write_all(ENTER) {
        leave();
        return Err(e);
    }
    Ok(keyboard)
}

/// Starts heeding signals. Asked to stop, the program winds down by itself
/// from then on: it puts the terminal back and takes any git command it is
/// waiting for down with it. Returns the listening end of the wake pipe.
pub fn listen() -> Option<PipeReader> {
    let reader = io::pipe().ok().and_then(|(reader, writer)| {
        // A signal just before a wait must still wake it, without ever
        // blocking the handler or leaving the event loop stuck reading.
        for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
            // SAFETY: `fd` is an open pipe end and F_SETFL takes an integer flag.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } != 0 {
                return None;
            }
        }
        WAKE.store(writer.into_raw_fd(), Ordering::Relaxed);
        Some(reader)
    });
    install(libc::SIGWINCH, on_winch);
    for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT] {
        install(sig, on_term);
    }
    reader
}

pub fn leave() {
    // Restore keyboard modes even if writes to the terminal fail.
    if let Some(t) = ORIG.get() {
        loop {
            // SAFETY: `t` contains saved terminal modes and is live for the call.
            let result = unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, t) };
            if result == 0 || io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                break;
            }
        }
    }
    let mut out = io::stdout().lock();
    let _ = out.write_all(LEAVE);
    let _ = out.flush();
}

pub fn size() -> (usize, usize) {
    let mut ws = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCGWINSZ writes into the live `winsize` record.
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ as _, &raw mut ws) };
    if result == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        (usize::from(ws.ws_col), usize::from(ws.ws_row))
    } else {
        (80, 24)
    }
}

pub enum Wait {
    Input,
    /// The extra descriptor became readable.
    Woken,
    Timeout,
    Interrupted,
}

/// Waits for input, a signal, the extra descriptor or a timeout.
/// Signals take priority, then the keyboard, then the extra descriptor.
pub fn wait(keyboard: &File, signal: Option<&PipeReader>, timeout_ms: i32, extra: Option<RawFd>) -> Wait {
    let mut fds = [
        libc::pollfd { fd: keyboard.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        // poll() ignores a negative descriptor.
        libc::pollfd { fd: extra.unwrap_or(-1), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: signal.map_or(-1, AsRawFd::as_raw_fd), events: libc::POLLIN, revents: 0 },
    ];
    let ready = wait_ready(&mut fds, timeout_ms);
    if ready == 0 {
        Wait::Timeout
    } else if ready < 0 {
        Wait::Interrupted
    } else if fds[2].revents != 0 {
        if let Some(mut reader) = signal {
            let mut buf = [0u8; 64];
            while reader.read(&mut buf).is_ok_and(|n| n > 0) {}
        }
        Wait::Interrupted
    } else if fds[0].revents != 0 {
        Wait::Input
    } else {
        Wait::Woken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;

    #[test]
    fn keyboard_readiness_takes_priority_over_an_extra_wakeup() {
        let (keyboard, input) = io::pipe().unwrap();
        let keyboard = File::from(OwnedFd::from(keyboard));
        let (extra, writer) = io::pipe().unwrap();
        let extra = Some(extra.as_raw_fd());
        assert!(matches!(wait(&keyboard, None, 0, extra), Wait::Timeout));

        wake(writer.as_raw_fd());
        assert!(matches!(wait(&keyboard, None, 0, extra), Wait::Woken));

        wake(input.as_raw_fd());
        assert!(matches!(wait(&keyboard, None, 0, extra), Wait::Input));
    }
}
