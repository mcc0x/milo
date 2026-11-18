//! Raw terminal access: mode switching, input decoding and a diffed cell buffer.

use std::fs::File;
use std::io::{self, ErrorKind, IsTerminal, PipeReader, Read, Write};
use std::os::fd::{AsFd, AsRawFd, IntoRawFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::OnceLock;

/// A resize is pending. The signal handler needs access for the life of the program.
pub static RESIZED: AtomicBool = AtomicBool::new(true);
/// A stop is pending. The signal handler needs access for the life of the program.
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
/// How long the rest of a split escape sequence is waited for.
const SEQUENCE_MS: i32 = 25;

/// Wakes a waiting loop with a byte. A full non-blocking pipe already holds a wakeup.
pub fn nudge(fd: RawFd) {
    if fd >= 0 {
        // SAFETY: The byte lives for the call, and write is async-signal-safe.
        unsafe { libc::write(fd, [1u8].as_ptr().cast(), 1) };
    }
}

/// Waits up to `timeout_ms` for descriptors to be ready. Returns the number
/// ready, zero on timeout, or a negative result on failure or interruption.
pub fn poll(fds: &mut [libc::pollfd], timeout_ms: i32) -> i32 {
    let Ok(len) = libc::nfds_t::try_from(fds.len()) else { return -1 };
    // SAFETY: The slice is writable and holds exactly `len` descriptors.
    unsafe { libc::poll(fds.as_mut_ptr(), len, timeout_ms) }
}

// These handlers only load and store atomics and call write, both async-signal-safe.
extern "C" fn on_winch(_: libc::c_int) {
    RESIZED.store(true, Ordering::Relaxed);
    nudge(WAKE.load(Ordering::Relaxed));
}

extern "C" fn on_term(_: libc::c_int) {
    TERMINATED.store(true, Ordering::Relaxed);
    nudge(WAKE.load(Ordering::Relaxed));
}

fn install(sig: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    // sa_flags stays 0: without SA_RESTART a signal interrupts poll(), which
    // is what wakes the event loop.
    // SAFETY: The action starts zeroed, carries a handler with the C signal
    // ABI, and lives for both calls. No old action is requested.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(sig, &sa, std::ptr::null_mut());
    }
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
    // SAFETY: `fd` stays open through the calls, and both mode records are
    // live and passed by reference. A non-terminal descriptor is refused.
    let t = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = t;
        libc::cfmakeraw(&mut raw);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) != 0 {
            return Err(io::Error::last_os_error());
        }
        t
    };
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
            // SAFETY: Both pipe ends stay open across this call.
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
    // Give the keyboard back even if the terminal no longer accepts output.
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
    // SAFETY: TIOCGWINSZ writes into the live `winsize` record; on failure
    // the record is ignored.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ as _, &mut ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
    }
    (80, 24)
}

pub enum Wait {
    Input,
    /// The extra descriptor became readable.
    Woken,
    Timeout,
    Interrupted,
}

/// Waits for the keyboard, or for `extra` to become readable, or for
/// `timeout_ms` to pass. The keyboard is reported first when both are ready.
pub fn wait(keyboard: &File, signal: Option<&PipeReader>, timeout_ms: i32, extra: Option<RawFd>) -> Wait {
    let mut fds = [
        libc::pollfd { fd: keyboard.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        // poll() ignores a negative descriptor.
        libc::pollfd { fd: extra.unwrap_or(-1), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: signal.map_or(-1, AsRawFd::as_raw_fd), events: libc::POLLIN, revents: 0 },
    ];
    let ready = poll(&mut fds, timeout_ms);
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Ctrl(u8),
    Enter,
    Esc,
    Up,
    Down,
    Left,
    Right,
    PgUp,
    PgDn,
    Home,
    End,
    /// A left click, at this column and row.
    Click(usize, usize),
    WheelUp,
    WheelDown,
    /// The pane gained or lost keyboard focus.
    Focus(bool),
    /// The terminal reported its default background colour.
    Background([u8; 3]),
    /// stdin reached end of file: the terminal is gone.
    Eof,
}

/// Drains everything currently readable on stdin into `keys`.
pub fn read_keys(keyboard: &mut File, keys: &mut Vec<Key>) {
    let mut buf = [0u8; 4096];
    let mut used = 0;
    let mut overflow = Vec::new();
    let mut retried = false;
    loop {
        match keyboard.read(&mut buf[used..]) {
            // poll() said readable, so an immediate zero-length read is EOF.
            Ok(0) if !retried && used == 0 && overflow.is_empty() => keys.push(Key::Eof),
            Ok(n) => {
                used += n;
                if used == buf.len() {
                    overflow.extend_from_slice(&buf);
                    used = 0;
                    continue;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            // Any other failure will not mend: the terminal is gone.
            Err(_) => keys.push(Key::Eof),
        }
        let bytes = if overflow.is_empty() {
            &buf[..used]
        } else {
            overflow.extend_from_slice(&buf[..used]);
            used = 0;
            &overflow[..]
        };
        // Give a split escape sequence one short wait for its ending.
        let mut fds = [libc::pollfd { fd: keyboard.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if retried || !unfinished(bytes) || poll(&mut fds, SEQUENCE_MS) <= 0 {
            decode(bytes, keys);
            return;
        }
        retried = true;
    }
}

/// Whether `b` ends partway through an escape sequence.
fn unfinished(b: &[u8]) -> bool {
    let Some(esc) = b.iter().rposition(|&c| c == 0x1b) else { return false };
    match &b[esc + 1..] {
        [] | [b'O'] => true,
        [b'[', rest @ ..] => !rest.iter().any(|c| (0x40..=0x7e).contains(c)),
        // An answer from the terminal runs to a BEL, or to an ESC of its own.
        [b']', rest @ ..] => !rest.contains(&7),
        _ => false,
    }
}

fn decode(b: &[u8], keys: &mut Vec<Key>) {
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        match c {
            0x1b => match b.get(i) {
                None => keys.push(Key::Esc),
                Some(b'[') => {
                    let start = i + 1;
                    let mut end = start;
                    while end < b.len() && !(0x40..=0x7e).contains(&b[end]) {
                        end += 1;
                    }
                    if end == b.len() {
                        return;
                    }
                    if let Some(k) = csi(&b[start..end], b[end]) {
                        keys.push(k);
                    }
                    i = end + 1;
                }
                Some(b'O') => {
                    if let Some(k) = b.get(i + 1).and_then(|&f| csi(&[], f)) {
                        keys.push(k);
                    }
                    i += 2;
                }
                // An answer from the terminal, ended by BEL or by ESC \.
                Some(b']') => {
                    let start = i + 1;
                    let Some(len) = b[start..].iter().position(|&c| c == 7 || c == 0x1b) else { return };
                    if let Some(bg) = background_reply(&b[start..start + len]) {
                        keys.push(Key::Background(bg));
                    }
                    i = start + len + if b[start + len] == 7 { 1 } else { 2 };
                }
                // Alt-modified key: drop it rather than act on the bare key.
                Some(_) => i += 1,
            },
            b'\r' | b'\n' => keys.push(Key::Enter),
            1..=26 => keys.push(Key::Ctrl(c + b'a' - 1)),
            0..=31 | 127 => {}
            _ => {
                let len = match c {
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => 1,
                };
                let end = (i - 1 + len).min(b.len());
                if let Some(ch) = std::str::from_utf8(&b[i - 1..end]).ok().and_then(|s| s.chars().next()) {
                    keys.push(Key::Char(ch));
                }
                i = end;
            }
        }
    }
}

fn csi(params: &[u8], fin: u8) -> Option<Key> {
    if let Some(mouse) = params.strip_prefix(b"<") {
        let mut it = std::str::from_utf8(mouse).ok()?.split(';').map(|p| p.parse::<usize>().ok());
        let (btn, x, y) = (it.next()??, it.next()??.checked_sub(1)?, it.next()??.checked_sub(1)?);
        // Bit 6 marks the wheel; the low bits are the button. 'M' is a press.
        return match (fin, btn & 64 != 0, btn & 3) {
            (b'M', true, 0) => Some(Key::WheelUp),
            (b'M', true, 1) => Some(Key::WheelDown),
            (b'M', false, 0) => Some(Key::Click(x, y)),
            _ => None,
        };
    }
    Some(match fin {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'I' => Key::Focus(true),
        b'O' => Key::Focus(false),
        b'~' => match params.split(|&c| c == b';').next()? {
            b"1" | b"7" => Key::Home,
            b"4" | b"8" => Key::End,
            b"5" => Key::PgUp,
            b"6" => Key::PgDn,
            _ => return None,
        },
        _ => return None,
    })
}

// ANSI palette indices, so colours follow the terminal theme.
pub const RED: u8 = 1;
pub const GREEN: u8 = 2;
pub const YELLOW: u8 = 3;
pub const BLUE: u8 = 4;
pub const MAGENTA: u8 = 5;
pub const CYAN: u8 = 6;

/// A background laid behind text without changing the text.
#[derive(Clone, Copy)]
pub enum Wash {
    /// The bar under the cursor.
    Bar = 1,
    /// A line the file gained.
    Added = 2,
    /// A line the file lost.
    Removed = 3,
}

/// Reads the terminal's answer to being asked its background colour:
/// "11;rgb:RRRR/GGGG/BBBB", each part one to four hex digits.
fn background_reply(reply: &[u8]) -> Option<[u8; 3]> {
    let mut parts = reply.strip_prefix(b"11;rgb:")?.split(|&b| b == b'/');
    let mut component = || {
        let digits = parts.next()?;
        if !(1..=4).contains(&digits.len()) {
            return None;
        }
        let mut value = 0;
        for &digit in digits {
            value = value * 16 + char::from(digit).to_digit(16)?;
        }
        // Scale from the number of digits given to eight bits.
        Some((value * 255 / ((1 << (4 * digits.len())) - 1)) as u8)
    };
    let rgb = [component()?, component()?, component()?];
    parts.next().is_none().then_some(rgb)
}

/// Packed cell style: bits 0–8 hold the text colour (0 = default, n+1 =
/// palette colour n); bits 9–13 hold bold, italic, strike, faint and reverse
/// in that order; bits 14–15 hold the wash (0 = none).
/// Palette colours 0–15 follow the terminal theme; 16–255 are fixed.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Style(u16);

const FG: u16 = 0x1ff;
const BOLD: u16 = 1 << 9;
const ITALIC: u16 = 1 << 10;
const STRIKE: u16 = 1 << 11;
const FAINT: u16 = 1 << 12;
const REVERSE: u16 = 1 << 13;
const WASH_SHIFT: u16 = 14;
const WASH: u16 = 3 << WASH_SHIFT;

const _: () = assert!(
    Wash::Bar as u16 == 1 && Wash::Added as u16 == 2 && Wash::Removed as u16 == 3
        && Wash::Removed as u16 == WASH >> WASH_SHIFT
);

impl Style {
    pub const PLAIN: Style = Style(0);

    pub const fn fg(c: u8) -> Style {
        Style(c as u16 + 1)
    }

    pub const fn bold(self) -> Style {
        Style(self.0 | BOLD)
    }

    pub const fn italic(self) -> Style {
        Style(self.0 | ITALIC)
    }

    pub const fn strike(self) -> Style {
        Style(self.0 | STRIKE)
    }

    /// Receded, relative to whatever the theme's text colour is.
    pub const fn faint(self) -> Style {
        Style(self.0 | FAINT)
    }

    /// Text and background swapped.
    pub const fn reverse(self) -> Style {
        Style(self.0 | REVERSE)
    }

    fn with_wash(self, wash: Wash) -> Style {
        Style(self.0 & !WASH | (wash as u16) << WASH_SHIFT)
    }

    fn wash_id(self) -> usize {
        ((self.0 & WASH) >> WASH_SHIFT) as usize
    }

    fn sgr(self, out: &mut Vec<u8>, washes: &[u32; 3]) {
        out.extend_from_slice(b"\x1b[0");
        if self.0 & BOLD != 0 {
            out.extend_from_slice(b";1");
        }
        if self.0 & ITALIC != 0 {
            out.extend_from_slice(b";3");
        }
        if self.0 & FAINT != 0 {
            out.extend_from_slice(b";2");
        }
        if self.0 & REVERSE != 0 {
            out.extend_from_slice(b";7");
        }
        if self.0 & STRIKE != 0 {
            out.extend_from_slice(b";9");
        }
        let _ = match (self.0 & FG).checked_sub(1) {
            None => Ok(()),
            Some(c @ 0..8) => write!(out, ";{}", 30 + c),
            Some(c @ 8..16) => write!(out, ";{}", 82 + c),
            Some(c) => write!(out, ";38;5;{c}"),
        };
        // A wash is mixed from the terminal's own background. Where that is
        // not known the bar falls back on the theme's grey, and the other
        // washes on nothing: the lines they mark are marked in the margin too.
        let wash = self.wash_id();
        if wash != 0 {
            let rgb = washes[wash - 1];
            let _ = match (rgb, wash) {
                (0, wash) if wash == Wash::Bar as usize => write!(out, ";100"),
                (0, _) => Ok(()),
                _ => write!(out, ";48;2;{};{};{}", rgb >> 16 & 255, rgb >> 8 & 255, rgb & 255),
            };
        }
        out.push(b'm');
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    st: Style,
}

const BLANK: Cell = Cell { ch: ' ', st: Style::PLAIN };
/// Trailing half of a double-width character.
const CONT: char = '\0';
/// Never drawn, so a row of these differs from any real frame.
const STALE: Cell = Cell { ch: '\u{1}', st: Style::PLAIN };
const SYNC_ON: &[u8] = b"\x1b[?2026h";

pub struct Screen {
    pub w: usize,
    pub h: usize,
    cur: Vec<Cell>,
    prev: Vec<Cell>,
    out: Vec<u8>,
    /// Each wash is 0 while unknown, otherwise 0x01RRGGBB.
    washes: [u32; 3],
    /// The wash colours used for the cells on the terminal.
    prev_washes: [u32; 3],
}

impl Screen {
    pub fn new() -> Screen {
        Screen {
            w: 0,
            h: 0,
            cur: Vec::new(),
            prev: Vec::new(),
            out: SYNC_ON.to_vec(),
            washes: [0; 3],
            prev_washes: [0; 3],
        }
    }

    /// Mixes the washes with the terminal's background to match its theme:
    /// the bar towards the text, gained lines towards green, lost lines towards red.
    pub fn set_background(&mut self, bg: [u8; 3]) {
        let bg = bg.map(u32::from);
        let dark = bg[0] * 299 + bg[1] * 587 + bg[2] * 114 < 128_000;
        let towards = [if dark { [255; 3] } else { [0; 3] }, [0, 190, 70], [230, 50, 50]];
        for (wash, (to, part)) in self.washes.iter_mut().zip(towards.into_iter().zip([14, 16, 16])) {
            let mix = |i: usize| (bg[i] * (100 - part) + to[i] * part) / 100;
            *wash = 1 << 24 | mix(0) << 16 | mix(1) << 8 | mix(2);
        }
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        self.w = w;
        self.h = h;
        self.cur.clear();
        self.cur.resize(w * h, BLANK);
        self.prev.clear();
        self.prev.resize(w * h, STALE);
        self.out.extend_from_slice(b"\x1b[0m\x1b[2J");
    }

    pub fn clear(&mut self) {
        self.cur.fill(BLANK);
    }

    /// The cells of row `y` from `x`, at most `w` wide, clipped to the screen.
    fn span(&mut self, x: usize, y: usize, w: usize) -> &mut [Cell] {
        if y >= self.h || x >= self.w {
            return &mut [];
        }
        let start = y * self.w + x;
        &mut self.cur[start..start + w.min(self.w - x)]
    }

    /// Draws `s` at (x, y) in at most `max_w` columns. Returns the columns used.
    pub fn put(&mut self, x: usize, y: usize, max_w: usize, s: &str, st: Style) -> usize {
        let row = self.span(x, y, max_w);
        let mut used = 0;
        for ch in s.chars() {
            let cw = char_width(ch);
            if cw == 0 {
                continue;
            }
            if used + cw > row.len() {
                break;
            }
            row[used] = Cell { ch, st };
            if cw == 2 {
                row[used + 1] = Cell { ch: CONT, st };
            }
            used += cw;
        }
        used
    }

    /// Like `put`, but ends a string that does not fit with an ellipsis.
    pub fn put_fit(&mut self, x: usize, y: usize, max_w: usize, s: &str, st: Style) -> usize {
        if max_w == 0 || str_width(s) <= max_w {
            return self.put(x, y, max_w, s, st);
        }
        let used = self.put(x, y, max_w - 1, s, st);
        used + self.put(x + used, y, 1, "…", st)
    }

    /// Like `put`, but shortens a string that does not fit in the middle,
    /// so a file name keeps both its start and its extension.
    pub fn put_middle(&mut self, x: usize, y: usize, max_w: usize, s: &str, st: Style) -> usize {
        if max_w < 8 || str_width(s) <= max_w {
            return self.put_fit(x, y, max_w, s, st);
        }
        let tail_chars = (max_w / 3).min(8);
        let tail_at = s.char_indices().rev().nth(tail_chars - 1).map_or(0, |(i, _)| i);
        let tail_w = str_width(&s[tail_at..]);
        let mut used = self.put(x, y, max_w.saturating_sub(tail_w + 1), s, st);
        used += self.put(x + used, y, 1, "…", st);
        used + self.put(x + used, y, tail_w, &s[tail_at..], st)
    }

    /// Gives a span a style, keeping what is written there.
    pub fn paint(&mut self, x: usize, y: usize, w: usize, st: Style) {
        for cell in self.span(x, y, w) {
            cell.st = st;
        }
    }

    /// Inverts a span, to pick out the changed part of a line.
    pub fn emphasise(&mut self, x: usize, y: usize, w: usize) {
        for cell in self.span(x, y, w) {
            cell.st = cell.st.reverse();
        }
    }

    /// Lays a wash behind a span, keeping each cell's own text and colour.
    pub fn wash(&mut self, x: usize, y: usize, w: usize, wash: Wash) {
        for cell in self.span(x, y, w) {
            cell.st = cell.st.with_wash(wash);
        }
    }

    /// Writes only the cells that changed since the last flush.
    pub fn flush(&mut self) {
        self.flush_to(|frame| {
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(frame);
            let _ = stdout.flush();
        });
    }

    fn flush_to(&mut self, write_frame: impl FnOnce(&[u8])) {
        let washes = &self.washes;
        let prev_washes = &self.prev_washes;
        let palette_changed = washes != prev_washes;
        let mut active = None;
        let mut utf8 = [0u8; 4];
        for y in 0..self.h {
            let cur = &self.cur[y * self.w..(y + 1) * self.w];
            let prev = &mut self.prev[y * self.w..(y + 1) * self.w];
            let mut next_col = usize::MAX;
            for (x, (&cell, old)) in cur.iter().zip(prev).enumerate() {
                if cell != *old {
                    *old = cell;
                } else if !palette_changed
                    || !cell.st.wash_id().checked_sub(1).is_some_and(|i| washes[i] != prev_washes[i])
                {
                    continue;
                }
                // A wide glyph must be drawn from its first column, unless
                // the terminal has already drawn it while painting this row.
                let (first, glyph) = if cell.ch == CONT {
                    if next_col == x + 1 {
                        continue;
                    }
                    (x - 1, cur[x - 1])
                } else {
                    (x, cell)
                };
                if first != next_col {
                    let _ = write!(self.out, "\x1b[{};{}H", y + 1, first + 1);
                }
                if active != Some(glyph.st) {
                    glyph.st.sgr(&mut self.out, washes);
                    active = Some(glyph.st);
                }
                self.out.extend_from_slice(glyph.ch.encode_utf8(&mut utf8).as_bytes());
                let wide = cur.get(first + 1).is_some_and(|cell| cell.ch == CONT);
                next_col = first + 1 + usize::from(wide);
            }
        }
        self.prev_washes = self.washes;
        if self.out.len() > SYNC_ON.len() {
            self.out.extend_from_slice(b"\x1b[0m\x1b[?2026l");
            write_frame(&self.out);
            self.out.truncate(SYNC_ON.len());
        }
    }
}

#[cfg(test)]
impl Screen {
    /// A screen for drawing on without a terminal.
    pub fn sized(w: usize, h: usize) -> Self {
        let mut screen = Self::new();
        screen.resize(w, h);
        screen
    }

    /// The text of each row, without trailing blanks.
    pub fn text(&self) -> Vec<String> {
        (0..self.h)
            .map(|y| {
                let cells = &self.cur[y * self.w..(y + 1) * self.w];
                let mut line: String = cells.iter().filter(|c| c.ch != CONT).map(|c| c.ch).collect();
                line.truncate(line.trim_end_matches(' ').len());
                line
            })
            .collect()
    }

    /// Whether the row starts with the cursor's background shade.
    pub fn bar_at(&self, y: usize) -> bool {
        self.cur[y * self.w].st.wash_id() == Wash::Bar as usize
    }
}

/// Terminal columns a character occupies: none for control, combining and
/// invisible formatting characters, two for East Asian wide characters and
/// emoji, one for everything else. Text of unknown origin is drawn through
/// this, so what it calls zero-width is never sent to the terminal at all.
fn char_width(c: char) -> usize {
    let c = c as u32;
    match c {
        0..=0x1f | 0x7f..=0x9f => 0,
        0x20..=0x2ff => 1,
        // Combining marks.
        0x300..=0x36f
        | 0x483..=0x489
        | 0x591..=0x5bd
        | 0x610..=0x61a
        | 0x64b..=0x65f
        | 0x1ab0..=0x1aff
        | 0x1dc0..=0x1dff
        | 0x20d0..=0x20ff
        | 0xfe20..=0xfe2f => 0,
        // Zero-width and direction-changing characters, variation
        // selectors and tags: invisible, and able to reorder what is shown.
        0x61c
        | 0x200b..=0x200f
        | 0x2028..=0x202e
        | 0x2060..=0x206f
        | 0xfe00..=0xfe0f
        | 0xfeff
        | 0xfff9..=0xfffb
        | 0xe0000..=0xe0fff => 0,
        // Emoji outside the main blocks that terminals draw two columns wide.
        0x231a..=0x231b
        | 0x23e9..=0x23ec
        | 0x23f0
        | 0x23f3
        | 0x25fd..=0x25fe
        | 0x2614..=0x2615
        | 0x2648..=0x2653
        | 0x267f
        | 0x2693
        | 0x26a1
        | 0x26aa..=0x26ab
        | 0x26bd..=0x26be
        | 0x26c4..=0x26c5
        | 0x26ce
        | 0x26d4
        | 0x26ea
        | 0x26f2..=0x26f3
        | 0x26f5
        | 0x26fa
        | 0x26fd
        | 0x2705
        | 0x270a..=0x270b
        | 0x2728
        | 0x274c
        | 0x274e
        | 0x2753..=0x2755
        | 0x2757
        | 0x2795..=0x2797
        | 0x27b0
        | 0x27bf
        | 0x2b1b..=0x2b1c
        | 0x2b50
        | 0x2b55 => 2,
        0x1100..=0x115f
        | 0x2e80..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f680..=0x1f6ff
        | 0x1f900..=0x1faff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

/// The byte offset of the first character starting at or after column `col`,
/// or the length of `text` if no character starts there.
pub fn byte_at_col(text: &str, col: usize) -> usize {
    let mut start = 0;
    for (byte, ch) in text.char_indices() {
        if start >= col {
            return byte;
        }
        start += char_width(ch);
    }
    text.len()
}

pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_wider_than_the_screen_stops_at_the_end_of_its_row() {
        let mut screen = Screen::sized(4, 2);
        screen.put(1, 0, 8, "abcdef", Style::PLAIN);
        assert_eq!(screen.text(), [" abc", ""]);
    }

    #[test]
    fn a_wide_character_fits_only_when_both_columns_are_in_its_row() {
        let mut screen = Screen::sized(4, 2);
        screen.put(2, 0, 4, "界", Style::PLAIN);
        screen.put(3, 0, 4, "界 should not spill", Style::PLAIN);
        screen.put(3, 1, 4, "界", Style::PLAIN);
        assert_eq!(screen.text(), ["  界", ""]);
    }

    #[test]
    fn a_shortened_file_name_keeps_its_extension() {
        let mut screen = Screen::sized(12, 1);
        screen.put_middle(0, 0, 12, "very-long-name.txt", Style::PLAIN);
        assert_eq!(screen.text(), ["very-lo….txt"]);
    }

    fn keys(bytes: &[u8]) -> Vec<Key> {
        let mut keys = Vec::new();
        decode(bytes, &mut keys);
        keys
    }

    #[test]
    fn background_reply_requires_three_hex_components() {
        assert_eq!(background_reply(b"11;rgb:0d0d/1111/1717"), Some([13, 17, 23]));
        assert_eq!(background_reply(b"11;rgb:ff/80/0"), Some([255, 128, 0]));
        assert_eq!(background_reply(b"11;rgb:ABC/123/FFF"), Some([171, 18, 255]));
        assert_eq!(background_reply(b"11;rgb:zz/00/00"), None);
        assert_eq!(background_reply(b"11;rgb:00000/0/0"), None);
        assert_eq!(background_reply(b"11;rgb:ffffffff/0/0"), None);
        assert_eq!(background_reply(b"11;rgb:+f/0/0"), None);
        assert_eq!(background_reply(b"11;rgb:/0/0"), None);
        assert_eq!(background_reply(b"11;rgb:0/0/0/0"), None);
    }

    #[test]
    fn background_replies_leave_the_surrounding_keys_intact() {
        assert_eq!(
            keys(b"j\x1b]11;rgb:1/2/3\x1b\\k\x1b]11;rgb:0/0/f\x07l"),
            [
                Key::Char('j'), Key::Background([17, 34, 51]), Key::Char('k'),
                Key::Background([0, 0, 255]), Key::Char('l'),
            ]
        );
        assert_eq!(keys(b"j\x1b]10;rgb:1/2/3\x07k"), [Key::Char('j'), Key::Char('k')]);
        assert!(unfinished(b"\x1b]11;rgb:0d0d/11") && !unfinished(b"\x1b]11;rgb:0/0/0\x1b\\"));
    }

    #[test]
    fn keys_and_sequences_decode() {
        assert_eq!(keys(b"j\r\x1b[A\x1b[6~\x03"), [Key::Char('j'), Key::Enter, Key::Up, Key::PgDn, Key::Ctrl(b'c')]);
        assert_eq!(keys("é".as_bytes()), [Key::Char('é')]);
        assert_eq!(keys(b"\x1b"), [Key::Esc]);
        assert_eq!(keys(b"\x1b[I\x1b[O"), [Key::Focus(true), Key::Focus(false)]);
    }

    #[test]
    fn mouse_reports_decode() {
        assert_eq!(keys(b"\x1b[<0;8;3M"), [Key::Click(7, 2)]);
        assert_eq!(keys(b"\x1b[<0;8;3m"), []);
        assert_eq!(keys(b"\x1b[<64;1;1M\x1b[<65;1;1M"), [Key::WheelUp, Key::WheelDown]);
    }

    #[test]
    fn an_unfinished_sequence_is_recognised() {
        assert!(unfinished(b"\x1b"));
        assert!(unfinished(b"j\x1b["));
        assert!(unfinished(b"\x1b[<0;8"));
        assert!(unfinished(b"\x1bO"));
        assert!(!unfinished(b"\x1b[A"));
        assert!(!unfinished(b"\x1b[<0;8;3M"));
        assert!(!unfinished(b"abc"));
    }

    #[test]
    fn widths_cover_what_breaks_a_row() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("日本"), 4);
        assert_eq!(str_width("🚀✨"), 4);
        // A bidirectional override and a zero-width space take no room.
        assert_eq!(str_width("a\u{202e}b\u{200b}c\u{2066}"), 3);
        assert_eq!(str_width("\x1b[31m"), 4);
    }

    #[test]
    fn column_offsets_always_land_on_character_boundaries() {
        assert_eq!(byte_at_col("abcd", 2), 2);
        assert_eq!(byte_at_col("a日b", 1), 1);
        assert_eq!(byte_at_col("a日b", 2), "a日".len());
        assert_eq!(byte_at_col("a\u{0301}b", 1), 1);
        assert_eq!(byte_at_col("a\u{0301}b", 2), "a\u{0301}b".len());
        assert_eq!(byte_at_col("ab", 20), 2);
    }

    #[test]
    fn a_background_reply_repaints_only_the_washed_cells() {
        let mut screen = Screen::new();
        screen.resize(5, 1);
        screen.put(0, 0, 5, "ABC界", Style::PLAIN);
        screen.wash(0, 0, 1, Wash::Bar);
        screen.wash(3, 0, 2, Wash::Added);
        screen.flush_to(|frame| {
            assert!(frame.starts_with(b"\x1b[?2026h\x1b[0m\x1b[2J"));
            assert!(frame.ends_with(b"\x1b[0m\x1b[?2026l"));
        });

        screen.set_background([0; 3]);
        let mut wrote = false;
        screen.flush_to(|frame| {
            wrote = true;
            assert_eq!(
                frame,
                concat!(
                    "\x1b[?2026h\x1b[1;1H\x1b[0;48;2;35;35;35mA",
                    "\x1b[1;4H\x1b[0;48;2;0;30;11m界\x1b[0m\x1b[?2026l"
                ).as_bytes()
            );
        });
        assert!(wrote);
        screen.set_background([0; 3]);
        screen.flush_to(|_| panic!("the palette did not change"));
    }
}
