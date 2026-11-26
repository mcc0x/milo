//! Sets terminal modes and waits for keyboard input, signals or file notifications.

use std::ffi::{CStr, OsStr};
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, IsTerminal, PipeReader, Read, Write};
use std::os::fd::{AsFd, AsRawFd, IntoRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use crate::keys::{Input, Key};

// One decoder burst can exceed this threshold. On saturation, leave input on
// the tty until the frame drains, even if Ctrl-C is behind the queued keys.
const KEY_BACKLOG: usize = 4096;

/// Redraw at the current size after startup or a resize signal.
pub static RESIZED: AtomicBool = AtomicBool::new(true);
/// Wind down after a termination signal, including while waiting on git.
pub static TERMINATED: AtomicBool = AtomicBool::new(false);
/// The write end of the wake pipe, kept open for handlers until the program exits.
static WAKE: AtomicI32 = AtomicI32::new(-1);

// Use the alternate screen, hide the cursor, ask for mouse and focus reports,
// and ask the terminal to mark pasted text so it cannot act as key presses.
// Turn off wrapping so a row never spills onto the next.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[?2004h";
const ASK_BACKGROUND: &[u8] = b"\x1b]11;?\x1b\\";
const ASK_GRAPHICS: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
// Ask for cell size, then a device report to mark the end of the answers.
const ASK_END: &[u8] = b"\x1b[16t\x1b[c";
// Return to ground state before the next frame after a cancelled loading write.
const CANCEL_FRAME: &[u8] = b"\x18\x1b[?2026l";
// End a partial control string or synchronized update before changing modes.
const LEAVE: &[u8] =
    b"\x18\x1b[?2026l\x1b[0m\x1b[?2004l\x1b[?1004l\x1b[?1003l\x1b[?1006l\x1b[?1000l\x1b[?7h\x1b[?25h\x1b[?1049l";
/// How long after asking a terminal may take to answer and still be waited for on the way out.
const ANSWER_WAIT: Duration = Duration::from_millis(250);
/// How often a program the terminal is lent to is asked after.
const LEND_LOOK: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Host {
    Rex,
    Graphics,
    Multiplexer,
    Other,
    Unknown,
}

pub(crate) fn host() -> Host {
    host_with_term(&std::env::var_os("TERM").unwrap_or_default())
}

fn host_with_term(term: &OsStr) -> Host {
    let program = std::env::var_os("TERM_PROGRAM").unwrap_or_default();
    let in_mux = term != "xterm-rex"
        && ["TMUX", "STY"].into_iter().any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
    identify(term, &program, in_mux)
}

fn identify(term: &OsStr, program: &OsStr, in_mux: bool) -> Host {
    let name = term.as_bytes();
    if name == b"tmux"
        || name.starts_with(b"tmux-")
        || name == b"screen"
        || name.starts_with(b"screen-")
        || matches!(program.as_bytes(), b"tmux" | b"screen")
    {
        return Host::Multiplexer;
    }
    // A terminal assignment in tmux or screen can hide its usual TERM.
    // Only a native Rex TERM outweighs an inherited TMUX or STY value.
    if name == b"xterm-rex" {
        return Host::Rex;
    }
    if in_mux {
        return Host::Multiplexer;
    }
    if matches!(name, b"xterm-kitty" | b"xterm-ghostty" | b"ghostty" | b"wezterm" | b"konsole" | b"konsole-256color") {
        return Host::Graphics;
    }
    match program.as_bytes() {
        b"rex" => Host::Rex,
        b"kitty" | b"ghostty" | b"WezTerm" | b"iTerm.app" | b"konsole" | b"WarpTerminal" => Host::Graphics,
        b"" => Host::Unknown,
        _ => Host::Other,
    }
}

fn supports_panes(term: &OsStr) -> bool {
    !matches!(term.as_bytes(), b"dumb" | b"linux")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Colors {
    None,
    Ansi,
    Indexed,
    Rgb,
}

#[derive(Clone, Copy)]
pub(crate) struct Capabilities {
    pub colors: Colors,
    pub graphics: bool,
}

impl Capabilities {
    fn environment(term: &OsStr) -> Self {
        let env = |name| std::env::var_os(name).unwrap_or_default();
        let host = host_with_term(term);
        Self::for_host(term, &env("COLORTERM"), &env("NO_COLOR"), &env("MILO_GRAPHICS"), host)
    }

    fn for_host(term: &OsStr, colorterm: &OsStr, no_color: &OsStr, graphics: &OsStr, host: Host) -> Self {
        let term = term.as_bytes();
        let legacy_vt = matches!(term, [b'v', b't', b'0'..=b'9', ..]);
        let colors = if !no_color.is_empty() || legacy_vt {
            Colors::None
        } else if matches!(host, Host::Rex | Host::Graphics)
            || term.ends_with(b"-direct")
            || matches!(colorterm.as_bytes(), b"truecolor" | b"24bit")
        {
            Colors::Rgb
        } else if term.ends_with(b"-256color") {
            Colors::Indexed
        } else {
            Colors::Ansi
        };
        let graphics = host != Host::Multiplexer
            && graphics != "off"
            && (graphics == "kitty" || matches!(host, Host::Rex | Host::Graphics));
        Self { colors, graphics }
    }

    fn asks_background(self) -> bool {
        self.colors == Colors::Rgb || self.graphics && self.colors != Colors::None
    }

    #[cfg(test)]
    const TEST: Self = Self { colors: Colors::Rgb, graphics: true };
}

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
    keyboard: File,
    input: Input,
    keys: Vec<Key>,
    cancelled_input: bool,
    output: File,
    signal: PipeReader,
    pending_interrupt: bool,
    cancelled_output: bool,
    saved: Option<libc::termios>,
    capabilities: Capabilities,
    /// When to stop taking answers to the initial terminal queries.
    answers_due: Option<Instant>,
}

/// How many columns the terminal that standard output is has, or nothing
/// where it is a pipe or a file.
pub fn output_columns() -> Option<usize> {
    let stdout = io::stdout();
    if !stdout.is_terminal() {
        return None;
    }
    let mut ws = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCGWINSZ writes into the live `winsize` record.
    let result = unsafe { libc::ioctl(stdout.as_raw_fd(), libc::TIOCGWINSZ as _, &raw mut ws) };
    (result == 0 && ws.ws_col > 0).then(|| usize::from(ws.ws_col))
}

impl Session {
    pub fn open() -> io::Result<Self> {
        let stdout = io::stdout();
        if !stdout.is_terminal() {
            return Err(io::Error::other("standard output is not a terminal"));
        }
        if !io::stdin().is_terminal() {
            return Err(io::Error::other("standard input is not a terminal"));
        }
        let term = std::env::var_os("TERM").unwrap_or_default();
        if !supports_panes(&term) {
            return Err(io::Error::other(format!(
                "TERM={} does not support panes; use an xterm-compatible UTF-8 terminal, or try milo doctor or milo COMMAND --help",
                term.to_string_lossy()
            )));
        }
        let output = terminal_output(stdout.as_raw_fd())?;
        let keyboard = File::from(io::stdin().as_fd().try_clone_to_owned()?);
        Self::enter(keyboard, output, listen()?, Capabilities::environment(&term))
    }

    fn enter(keyboard: File, output: File, signal: PipeReader, capabilities: Capabilities) -> io::Result<Self> {
        let mut session = Self {
            keyboard,
            input: Input::default(),
            keys: Vec::new(),
            cancelled_input: false,
            output,
            signal,
            pending_interrupt: false,
            cancelled_output: false,
            saved: None,
            capabilities,
            answers_due: None,
        };
        session.engage()?;
        Ok(session)
    }

    /// Gives the terminal to `command` until it ends or `keep` says it may no
    /// longer have it, then takes it back as `open` first did. Returns whether
    /// the program finished on its own.
    pub fn lend(&mut self, command: &mut std::process::Command, mut keep: impl FnMut() -> bool) -> io::Result<bool> {
        self.restore();
        let finished = crate::fs_ops::spawn(command).is_ok_and(|mut child| {
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status.success(),
                    Ok(None) if keep() => std::thread::sleep(LEND_LOOK),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break false;
                    }
                }
            }
        });
        self.engage()?;
        Ok(finished)
    }

    /// Puts the keyboard in raw mode and the screen in Milo's modes, and asks
    /// the terminal what it can do.
    fn engage(&mut self) -> io::Result<()> {
        let fd = self.keyboard.as_raw_fd();
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
        self.saved = Some(saved);
        // What was read before the terminal was lent belongs to its last owner.
        self.input = Input::default();
        self.keys.clear();
        self.cancelled_input = false;
        self.cancelled_output = false;
        // There is no reliable distinction between a pasted control byte and a
        // key until the terminal receives the bracketed-paste mode request.
        self.write_until(ENTER, None, false)?;
        self.answers_due = Some(Instant::now() + ANSWER_WAIT);
        if self.capabilities.asks_background() {
            self.write_until(ASK_BACKGROUND, None, false)?;
        }
        if self.capabilities.graphics {
            self.write_until(ASK_GRAPHICS, None, false)?;
        }
        self.write_until(ASK_END, None, false)?;
        self.answers_due = Some(Instant::now() + ANSWER_WAIT);
        Ok(())
    }

    pub(crate) fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    /// Waits on the keyboard and on what the pane waits for: in `extra`, its
    /// watcher, an external source and a check it left running.
    pub fn wait(&mut self, timeout_ms: i32, extra: [Option<RawFd>; 3]) -> Wait {
        if std::mem::take(&mut self.pending_interrupt) {
            return Wait::Interrupted;
        }
        let wait = wait_for(
            self.keyboard.as_raw_fd(),
            &self.signal,
            self.replies_wait(timeout_ms),
            extra,
            false,
            !self.keys.is_empty(),
        );
        self.after_wait(wait)
    }

    pub fn wait_for_open(&mut self, timeout_ms: i32, done: RawFd) -> Wait {
        let extra = [Some(done), None, None];
        let wait = wait_for(
            self.keyboard.as_raw_fd(),
            &self.signal,
            self.replies_wait(timeout_ms),
            extra,
            true,
            !self.keys.is_empty(),
        );
        self.after_wait(wait)
    }

    fn replies_wait(&self, timeout_ms: i32) -> i32 {
        let Some(due) = self.answers_due else { return timeout_ms };
        let wait = milliseconds(due.saturating_duration_since(Instant::now()));
        if timeout_ms < 0 { wait } else { wait.min(timeout_ms) }
    }

    fn after_wait(&mut self, wait: Wait) -> Wait {
        let expired = self.expire_replies();
        if matches!(wait, Wait::Timeout) && expired { Wait::Input } else { wait }
    }

    fn expire_replies(&mut self) -> bool {
        if self.answers_due.is_some_and(|due| Instant::now() >= due) {
            self.answers_due = None;
            self.keys.push(Key::Answered);
            true
        } else {
            false
        }
    }

    fn read_keys(&mut self) {
        self.expire_replies();
        let at = self.keys.len();
        self.input.receive(&mut self.keyboard, &mut self.keys);
        // Only the terminal we probed may enable uploads. Ignore unsolicited
        // and late replies, but consume their escape strings in the decoder.
        let mut kept = at;
        for read in at..self.keys.len() {
            let key = self.keys[read];
            let waiting = self.answers_due.is_some();
            let keep = match key {
                Key::Background(_) => waiting && self.capabilities.asks_background(),
                Key::Pictures(_) => waiting && self.capabilities.graphics,
                Key::CellSize(..) => waiting,
                Key::Answered => self.answers_due.take().is_some(),
                _ => true,
            };
            if keep {
                self.keys[kept] = key;
                kept += 1;
            }
        }
        self.keys.truncate(kept);
    }

    pub fn receive(&mut self) -> std::vec::Drain<'_, Key> {
        if self.keys.is_empty() {
            self.read_keys();
        }
        self.keys.drain(..)
    }

    /// Whether pasted text is kept for the pane or skipped.
    pub fn keep_pastes(&mut self, keep: bool) {
        self.input.keep_pastes(keep);
    }

    /// The text behind the oldest `Key::Paste` received, unless it was too long to keep.
    pub fn pasted(&mut self) -> Option<String> {
        self.input.pasted()
    }

    pub fn stopping(&self) -> bool {
        self.cancelled_input || TERMINATED.load(Ordering::Relaxed)
    }

    /// Make progress while the terminal is reading, but let termination interrupt backpressure.
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if self.cancelled_output {
            self.write_until(CANCEL_FRAME, None, true)?;
            self.cancelled_output = false;
        }
        self.write_until(data, None, true)
    }

    /// The opening thread must be able to hand the terminal back even if it stopped reading.
    pub(crate) fn write_while_open(&mut self, data: &[u8], done: RawFd) -> io::Result<()> {
        self.write_until(data, Some(done), true)
    }

    fn write_until(&mut self, mut data: &[u8], done: Option<RawFd>, read_input: bool) -> io::Result<()> {
        while !data.is_empty() {
            if self.stopping() {
                return Err(ErrorKind::Interrupted.into());
            }
            match self.output.write(data) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => data = &data[n..],
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    let keyboard = if read_input && self.keys.len() < KEY_BACKLOG {
                        self.keyboard.as_raw_fd()
                    } else {
                        -1
                    };
                    let mut fds = [
                        libc::pollfd { fd: self.output.as_raw_fd(), events: libc::POLLOUT, revents: 0 },
                        libc::pollfd { fd: self.signal.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                        libc::pollfd { fd: done.unwrap_or(-1), events: libc::POLLIN, revents: 0 },
                        libc::pollfd { fd: keyboard, events: libc::POLLIN, revents: 0 },
                    ];
                    if wait_ready(&mut fds, -1) < 0 {
                        let e = io::Error::last_os_error();
                        if e.kind() != ErrorKind::Interrupted {
                            return Err(e);
                        }
                    } else if fds[2].revents != 0 {
                        self.cancelled_output = true;
                        return Err(ErrorKind::Interrupted.into());
                    } else if fds[1].revents != 0 {
                        drain_signal(&self.signal);
                        // Keep the wakeup for the pane without leaving the pipe readable
                        // while the terminal is still blocked.
                        self.pending_interrupt = true;
                    } else if fds[3].revents != 0 {
                        if fds[3].revents & libc::POLLIN == 0 {
                            self.cancelled_input = true;
                            continue;
                        }
                        // Use the same decoder as the pane, including an in-progress
                        // paste or terminal report. Keep other keys for the next wait.
                        let at = self.keys.len();
                        self.read_keys();
                        self.cancelled_input =
                            self.keys[at..].iter().any(|key| matches!(key, Key::Ctrl(b'c') | Key::Eof));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn track_motion(&mut self, enabled: bool) -> io::Result<()> {
        self.write(if enabled { b"\x1b[?1003h" } else { b"\x1b[?1003l\x1b[?1000h" })
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
        (Self::enter(keyboard, output, signals, Capabilities::TEST).unwrap(), master, writer)
    }

    pub fn restore(&mut self) {
        let Some(saved) = self.saved.take() else { return };
        // A pane that closes at once, as when it cannot open, can leave before
        // the terminal has answered. Answers that arrive once the shell has the
        // keyboard back are shown at its prompt and read as typing.
        if let Some(due) = self.answers_due.take() {
            self.input.drain_answers(&mut self.keyboard, due);
        }
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
    let term = std::env::var_os("TERM").unwrap_or_default();
    if stdout.is_terminal()
        && supports_panes(&term)
        && host_with_term(&term) == Host::Rex
        && crate::rex::inside()
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

/// A pipe that a signal handler writes to, so that a wait on it ends.
fn wake_pipe() -> io::Result<PipeReader> {
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
    Ok(reader)
}

/// For a helper with no terminal: a pipe that becomes readable when the
/// process is asked to end, which also sets `TERMINATED`.
pub fn termination() -> io::Result<PipeReader> {
    let reader = wake_pipe()?;
    for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        install(sig, on_term)?;
    }
    Ok(reader)
}

fn listen() -> io::Result<PipeReader> {
    let reader = wake_pipe()?;
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
    /// A check that was left running has its answer.
    Checked,
    Timeout,
    Interrupted,
    Closed,
}

fn drain_signal(mut reader: &PipeReader) {
    let mut buf = [0u8; 64];
    while reader.read(&mut buf).is_ok_and(|n| n > 0) {}
}

/// Waits for input, a signal, a watcher, an external source, a finished check or a timeout.
/// Signals always take priority; loading also prioritizes completion over keystrokes.
fn wait_for(
    keyboard: RawFd,
    signal: &PipeReader,
    timeout_ms: i32,
    extra: [Option<RawFd>; 3],
    opening: bool,
    buffered: bool,
) -> Wait {
    let mut fds = [
        libc::pollfd { fd: keyboard, events: libc::POLLIN, revents: 0 },
        // poll() ignores a negative descriptor.
        libc::pollfd { fd: extra[0].unwrap_or(-1), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: extra[1].unwrap_or(-1), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: extra[2].unwrap_or(-1), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: signal.as_raw_fd(), events: libc::POLLIN, revents: 0 },
    ];
    let ready = wait_ready(&mut fds, if buffered { 0 } else { timeout_ms });
    if ready < 0 {
        if io::Error::last_os_error().kind() == ErrorKind::Interrupted { Wait::Interrupted } else { Wait::Closed }
    } else if fds[4].revents != 0 {
        drain_signal(signal);
        Wait::Interrupted
    } else if opening && fds[1].revents != 0 {
        Wait::Woken
    } else if buffered || fds[0].revents & libc::POLLIN != 0 {
        Wait::Input
    } else if fds[0].revents != 0 {
        Wait::Closed
    } else if fds[2].revents != 0 {
        Wait::External
    } else if fds[3].revents != 0 {
        Wait::Checked
    } else if ready == 0 {
        Wait::Timeout
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
    fn explicit_terminal_identity_beats_stale_rex_branding() {
        let host = |term, program, in_mux| identify(OsStr::new(term), OsStr::new(program), in_mux);
        assert!(host("tmux-256color", "rex", false) == Host::Multiplexer);
        assert!(host("screen", "rex", false) == Host::Multiplexer);
        assert!(host("xterm-256color", "rex", true) == Host::Multiplexer);
        assert!(host("xterm-kitty", "rex", false) == Host::Graphics);
        assert!(host("xterm-256color", "Apple_Terminal", false) == Host::Other);
        assert!(host("xterm-rex", "", true) == Host::Rex);
        assert!(host("xterm-256color", "rex", false) == Host::Rex);
    }

    #[test]
    fn profiles_do_not_assume_truecolor_or_kitty_from_a_generic_term() {
        let caps = |term, program, colorterm, no_color, graphics| {
            let term = OsStr::new(term);
            Capabilities::for_host(
                term,
                OsStr::new(colorterm),
                OsStr::new(no_color),
                OsStr::new(graphics),
                identify(term, OsStr::new(program), false),
            )
        };
        let generic = caps("xterm-256color", "", "", "", "");
        assert_eq!(generic.colors, Colors::Indexed);
        assert!(!generic.asks_background());
        assert!(!generic.graphics);
        assert_eq!(caps("xterm", "", "", "", "").colors, Colors::Ansi);
        assert_eq!(caps("vt100", "", "truecolor", "", "").colors, Colors::None);
        assert_eq!(caps("vte-256color", "", "", "", "").colors, Colors::Indexed);
        assert_eq!(caps("xterm-256color", "rex", "truecolor", "0", "").colors, Colors::None);
        assert!(caps("xterm-ghostty", "", "", "", "").graphics);
        assert!(!caps("xterm-rex", "rex", "", "", "off").graphics);
        assert!(caps("xterm-256color", "", "", "", "kitty").graphics);
        assert!(!caps("tmux-256color", "rex", "", "", "kitty").graphics);
    }

    #[test]
    fn a_terminal_that_was_not_asked_cannot_enable_picture_uploads() {
        let (keyboard, mut master) = test_pair();
        let output = keyboard.try_clone().unwrap();
        let (signal, _wake) = io::pipe().unwrap();
        let capabilities = Capabilities { colors: Colors::Indexed, graphics: false };
        let mut terminal = Session::enter(keyboard, output, signal, capabilities).unwrap();
        let mut startup = [0; ENTER.len() + ASK_END.len()];
        master.read_exact(&mut startup).unwrap();
        assert_eq!(&startup[ENTER.len()..], ASK_END);
        terminal.answers_due = Some(Instant::now() + Duration::from_secs(10));
        master.write_all(b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?1;2cq").unwrap();
        assert!(matches!(terminal.wait(1000, [None; 3]), Wait::Input));
        assert_eq!(terminal.receive().collect::<Vec<_>>(), [Key::CellSize(10, 20), Key::Answered, Key::Char('q')]);
    }

    #[test]
    fn silence_ends_negotiation_and_late_replies_are_not_keys() {
        let (mut terminal, mut master, _wake) = Session::test_pty();
        terminal.answers_due = Some(Instant::now());
        assert!(matches!(terminal.wait(0, [None; 3]), Wait::Input));
        assert_eq!(terminal.receive().collect::<Vec<_>>(), [Key::Answered]);
        master.write_all(b"\x1b_Gi=31;OK\x1b\\\x1b[?1;2cq").unwrap();
        assert!(matches!(terminal.wait(1000, [None; 3]), Wait::Input));
        assert_eq!(terminal.receive().collect::<Vec<_>>(), [Key::Char('q')]);
    }

    #[test]
    fn keyboard_readiness_takes_priority_over_an_extra_wakeup() {
        let (keyboard, input) = io::pipe().unwrap();
        let (extra, writer) = io::pipe().unwrap();
        let extra = Some(extra.as_raw_fd());
        let (signal, _signal_writer) = io::pipe().unwrap();
        let wait =
            |opening, buffered| wait_for(keyboard.as_raw_fd(), &signal, 0, [extra, None, None], opening, buffered);
        assert!(matches!(wait(false, false), Wait::Timeout));
        assert!(matches!(wait(false, true), Wait::Input));

        wake(writer.as_raw_fd());
        assert!(matches!(wait(false, false), Wait::Woken));
        assert!(matches!(wait(false, true), Wait::Input));
        assert!(matches!(wait(true, true), Wait::Woken));

        wake(input.as_raw_fd());
        assert!(matches!(wait(false, false), Wait::Input));
        assert!(matches!(wait(true, false), Wait::Woken));
    }

    #[test]
    fn observer_updates_and_end_of_stream_wake_the_pane() {
        let (keyboard, input) = io::pipe().unwrap();
        let (watch, changed) = io::pipe().unwrap();
        let (observer, update) = io::pipe().unwrap();
        let (signal, _signal_writer) = io::pipe().unwrap();
        let wait = || {
            wait_for(
                keyboard.as_raw_fd(),
                &signal,
                0,
                [Some(watch.as_raw_fd()), Some(observer.as_raw_fd()), None],
                false,
                false,
            )
        };
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

    #[test]
    fn a_finished_check_wakes_the_pane_once_its_keys_are_read() {
        let (keyboard, input) = io::pipe().unwrap();
        let (check, done) = io::pipe().unwrap();
        let (signal, _signal_writer) = io::pipe().unwrap();
        let wait = || wait_for(keyboard.as_raw_fd(), &signal, 0, [None, None, Some(check.as_raw_fd())], false, false);
        assert!(matches!(wait(), Wait::Timeout));
        // The check's thread closes its end when the answer is ready.
        drop(done);
        assert!(matches!(wait(), Wait::Checked));
        wake(input.as_raw_fd());
        assert!(matches!(wait(), Wait::Input));
    }

    #[test]
    fn a_signal_drained_during_a_blocked_write_still_interrupts_the_next_wait() {
        use std::os::fd::OwnedFd;
        use std::time::Instant;

        let (keyboard, _input) = io::pipe().unwrap();
        let (mut read, output) = io::pipe().unwrap();
        let (signal, wakeup) = io::pipe().unwrap();
        for fd in [output.as_raw_fd(), signal.as_raw_fd()] {
            // SAFETY: Both descriptors are open; F_SETFL sets nonblocking mode.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) }, 0);
        }
        let mut output = File::from(OwnedFd::from(output));
        let frame = [b'x'; 4096];
        let mut buffered = 0;
        loop {
            match output.write(&frame) {
                Ok(n) => buffered += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => panic!("cannot fill output pipe: {e}"),
            }
        }
        let signal_fd = signal.as_raw_fd();
        let mut terminal = Session {
            keyboard: File::from(OwnedFd::from(keyboard)),
            input: Input::default(),
            keys: Vec::new(),
            cancelled_input: false,
            output,
            signal,
            pending_interrupt: false,
            cancelled_output: false,
            saved: None,
            capabilities: Capabilities::TEST,
            answers_due: None,
        };
        wake(wakeup.as_raw_fd());
        std::thread::scope(|scope| {
            let writing = scope.spawn(|| terminal.write(&frame));
            let until = Instant::now() + Duration::from_secs(5);
            let mut fds = [libc::pollfd { fd: signal_fd, events: libc::POLLIN, revents: 0 }];
            // Don't free the output pipe until the writer has consumed the wakeup.
            let drained = loop {
                if wait_ready(&mut fds, 0) == 0 {
                    break true;
                }
                if Instant::now() >= until {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            read.read_exact(&mut vec![0; buffered]).unwrap();
            writing.join().unwrap().unwrap();
            assert!(drained, "the writer did not consume the signal byte while blocked");
        });
        assert!(matches!(terminal.wait(0, [None; 3]), Wait::Interrupted));
        assert!(matches!(terminal.wait(0, [None; 3]), Wait::Timeout));
    }

    #[test]
    fn completing_open_cancels_a_partial_loading_frame_and_resets_terminal_state() {
        use std::os::fd::OwnedFd;

        let (keyboard, _input) = io::pipe().unwrap();
        let (mut read, output) = io::pipe().unwrap();
        let (signal, _signal_writer) = io::pipe().unwrap();
        let (done, completion) = io::pipe().unwrap();
        for fd in [output.as_raw_fd(), read.as_raw_fd()] {
            // SAFETY: Both descriptors are open; F_SETFL sets nonblocking mode.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) }, 0);
        }
        let mut terminal = Session {
            keyboard: File::from(OwnedFd::from(keyboard)),
            input: Input::default(),
            keys: Vec::new(),
            cancelled_input: false,
            output: File::from(OwnedFd::from(output)),
            signal,
            pending_interrupt: false,
            cancelled_output: false,
            saved: None,
            capabilities: Capabilities::TEST,
            answers_due: None,
        };
        let frame = vec![b'x'; 1 << 20];
        drop(completion);
        assert_eq!(terminal.write_while_open(&frame, done.as_raw_fd()).unwrap_err().kind(), ErrorKind::Interrupted);
        let mut partial = Vec::new();
        assert_eq!(read.read_to_end(&mut partial).unwrap_err().kind(), ErrorKind::WouldBlock);
        assert!(!partial.is_empty() && partial.len() < frame.len());

        terminal.write(b"Ready").unwrap();
        let mut resumed = [0; CANCEL_FRAME.len() + 5];
        read.read_exact(&mut resumed).unwrap();
        assert!(resumed.starts_with(CANCEL_FRAME) && resumed.ends_with(b"Ready"));
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
            let terminal = Session::enter(
                keyboard.try_clone().unwrap(),
                keyboard.try_clone().unwrap(),
                io::pipe().unwrap().0,
                Capabilities::TEST,
            )
            .unwrap();
            assert_ne!(modes(&terminal.keyboard), before);
            std::panic::resume_unwind(Box::new("panic while drawing"));
        });
        assert!(result.is_err());
        assert_eq!(modes(&keyboard), before);
        // The PTY may deliver the enter and leave sequences in separate reads.
        let mut output = [0; ENTER.len() + ASK_BACKGROUND.len() + ASK_GRAPHICS.len() + ASK_END.len() + LEAVE.len()];
        let mut at = 0;
        while at < output.len() {
            let mut ready = [libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            assert!(wait_ready(&mut ready, 1000) > 0, "missing terminal output after {at} bytes");
            let n = master.read(&mut output[at..]).unwrap();
            assert_ne!(n, 0);
            at += n;
        }
        assert!(output.starts_with(ENTER) && output.ends_with(LEAVE));
    }

    #[test]
    fn a_lent_terminal_has_the_shell_s_modes_and_gets_milo_s_back_after() {
        let (keyboard, mut master) = test_pair();
        let before = modes(&keyboard);
        let mut terminal = Session::enter(
            keyboard.try_clone().unwrap(),
            keyboard.try_clone().unwrap(),
            io::pipe().unwrap().0,
            Capabilities::TEST,
        )
        .unwrap();
        let raw = modes(&terminal.keyboard);
        assert_ne!(raw, before);
        // The program sees the modes the shell left, and reports them.
        let seen = std::env::temp_dir().join(format!("milo-lent-{}", std::process::id()));
        let mut lent = std::process::Command::new("/bin/sh");
        lent.args(["-c", "stty -g > \"$0\"", seen.to_str().unwrap()]).stdin(keyboard.try_clone().unwrap());
        assert!(terminal.lend(&mut lent, || true).unwrap());
        assert_eq!(modes(&terminal.keyboard), raw);
        assert!(!terminal.lend(&mut std::process::Command::new("false"), || true).unwrap());
        let started = Instant::now();
        let mut sleeper = std::process::Command::new("/bin/sleep");
        sleeper.arg("30");
        assert!(!terminal.lend(&mut sleeper, || false).unwrap(), "taken back from a program that may not keep it");
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(terminal);
        assert_eq!(modes(&keyboard), before);
        let shown = std::fs::read_to_string(&seen).unwrap();
        std::fs::remove_file(&seen).unwrap();
        assert!(!shown.trim().is_empty());
        let asked = ENTER.len() + ASK_BACKGROUND.len() + ASK_GRAPHICS.len() + ASK_END.len();
        let mut output = vec![0; 3 * (asked + LEAVE.len())];
        let mut at = 0;
        while at < output.len() {
            let mut ready = [libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            assert!(wait_ready(&mut ready, 1000) > 0, "missing terminal output after {at} bytes");
            let n = master.read(&mut output[at..]).unwrap();
            assert_ne!(n, 0);
            at += n;
        }
        for turn in output.chunks(asked + LEAVE.len()) {
            assert!(turn.starts_with(ENTER) && turn.ends_with(LEAVE));
        }
    }

    #[test]
    fn a_failed_open_write_still_restores_keyboard_modes() {
        let (keyboard, _master) = test_pair();
        let before = modes(&keyboard);
        let read_only = File::open("/dev/null").unwrap();
        let failed =
            Session::enter(keyboard.try_clone().unwrap(), read_only, io::pipe().unwrap().0, Capabilities::TEST);
        assert!(failed.is_err());
        assert_eq!(modes(&keyboard), before);
    }

    #[test]
    fn answers_that_arrive_as_a_pane_closes_are_not_left_for_the_shell() {
        let (keyboard, mut master) = test_pair();
        let mut terminal = Session::enter(
            keyboard.try_clone().unwrap(),
            keyboard.try_clone().unwrap(),
            io::pipe().unwrap().0,
            Capabilities::TEST,
        )
        .unwrap();
        // However busy the machine is, the answers come before the wait that `enter` set for them ends.
        terminal.answers_due = terminal.answers_due.map(|due| due + Duration::from_secs(60));
        let answering = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            master.write_all(b"\x1b]11;rgb:1/2/3\x1b\\\x1b[6;16;8t\x1b[?62;22c").unwrap();
            master
        });
        terminal.restore();
        let mut master = answering.join().unwrap();

        // The shell's terminal modes would echo a late answer after the leave sequence.
        let mut output = Vec::new();
        let mut ready = [libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        while wait_ready(&mut ready, 200) > 0 {
            let mut chunk = [0; 256];
            let n = master.read(&mut chunk).unwrap();
            assert_ne!(n, 0);
            output.extend_from_slice(&chunk[..n]);
        }
        assert!(output.starts_with(ENTER) && output.ends_with(LEAVE), "{:?}", String::from_utf8_lossy(&output));
    }
}
