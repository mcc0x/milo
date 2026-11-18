//! Raw terminal access: mode switching, input decoding and a diffed cell buffer.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

pub static RESIZED: AtomicBool = AtomicBool::new(true);
pub static TERMINATED: AtomicBool = AtomicBool::new(false);
static ORIG: OnceLock<libc::termios> = OnceLock::new();

// Alternate screen, hidden cursor, mouse buttons in SGR form, focus reporting.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[?1004h";
const LEAVE: &[u8] = b"\x1b[0m\x1b[?1004l\x1b[?1006l\x1b[?1000l\x1b[?25h\x1b[?1049l";

extern "C" fn on_winch(_: libc::c_int) {
    RESIZED.store(true, Ordering::Relaxed);
}

extern "C" fn on_term(_: libc::c_int) {
    TERMINATED.store(true, Ordering::Relaxed);
}

fn install(sig: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    // sa_flags stays 0: without SA_RESTART a signal interrupts poll(), which
    // is what wakes the event loop.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(sig, &sa, std::ptr::null_mut());
    }
}

pub fn enter() -> io::Result<()> {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) != 0 {
            return Err(io::Error::last_os_error());
        }
        let _ = ORIG.set(t);
        let mut raw = t;
        libc::cfmakeraw(&mut raw);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(0, libc::TCSAFLUSH, &raw) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    install(libc::SIGWINCH, on_winch);
    install(libc::SIGTERM, on_term);
    install(libc::SIGHUP, on_term);
    let mut out = io::stdout().lock();
    out.write_all(ENTER)?;
    out.flush()
}

pub fn leave() {
    let mut out = io::stdout().lock();
    let _ = out.write_all(LEAVE);
    let _ = out.flush();
    if let Some(t) = ORIG.get() {
        unsafe {
            libc::tcsetattr(0, libc::TCSAFLUSH, t);
        }
    }
}

pub fn size() -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ as _, &mut ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
    }
    (80, 24)
}

pub enum Wait {
    Input,
    Timeout,
    Interrupted,
}

pub fn wait(timeout_ms: i32) -> Wait {
    let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    match unsafe { libc::poll(&mut fd, 1, timeout_ms) } {
        0 => Wait::Timeout,
        n if n > 0 => Wait::Input,
        _ => Wait::Interrupted,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Ctrl(u8),
    Enter,
    Up,
    Down,
    Left,
    Right,
    PgUp,
    PgDn,
    Home,
    End,
    /// A left click, on this screen row.
    Click(usize),
    WheelUp,
    WheelDown,
    /// The pane gained or lost keyboard focus.
    Focus(bool),
    /// stdin reached end of file: the terminal is gone.
    Eof,
}

/// Drains everything currently readable on stdin into `keys`.
pub fn read_keys(keys: &mut Vec<Key>) {
    let mut buf = [0u8; 4096];
    let mut bytes = Vec::new();
    let mut first = true;
    loop {
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            // poll() said readable, so an immediate zero-length read is EOF.
            if n == 0 && first {
                keys.push(Key::Eof);
            }
            break;
        }
        first = false;
        bytes.extend_from_slice(&buf[..n as usize]);
        if (n as usize) < buf.len() {
            break;
        }
    }
    decode(&bytes, keys);
}

fn decode(b: &[u8], keys: &mut Vec<Key>) {
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        match c {
            0x1b => match b.get(i) {
                None => {}
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
        let (btn, _x, y) = (it.next()??, it.next()??, it.next()??.checked_sub(1)?);
        // Bit 6 marks the wheel; the low bits are the button. 'M' is a press.
        return match (fin, btn & 64 != 0, btn & 3) {
            (b'M', true, 0) => Some(Key::WheelUp),
            (b'M', true, 1) => Some(Key::WheelDown),
            (b'M', false, 0) => Some(Key::Click(y)),
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
pub const MAGENTA: u8 = 5;
pub const CYAN: u8 = 6;
pub const GRAY: u8 = 8;

/// Packed cell style: fg in bits 0-8, bg in bits 9-17 (0 = default, n+1 =
/// colour n of the 256-colour palette), then the face flags. Colours 0-15
/// follow the terminal theme; 16-255 are fixed.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Style(u32);

const FG: u32 = 0x1ff;
const BG_SHIFT: u32 = 9;
const BOLD: u32 = 1 << 18;
const ITALIC: u32 = 1 << 19;
const STRIKE: u32 = 1 << 20;
const FAINT: u32 = 1 << 21;
/// The flags that are part of the text itself and survive a highlight.
const FACE: u32 = BOLD | ITALIC | STRIKE | FAINT;

impl Style {
    pub const PLAIN: Style = Style(0);

    pub const fn fg(c: u8) -> Style {
        Style(c as u32 + 1)
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

    /// A grey background. Grey text would vanish on it, so that turns plain.
    const fn tinted(self) -> Style {
        let fg = if self.0 & FG == GRAY as u32 + 1 { 0 } else { self.0 & FG };
        Style(self.0 & FACE | fg | (GRAY as u32 + 1) << BG_SHIFT)
    }

    fn sgr(self, out: &mut Vec<u8>) {
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
        if self.0 & STRIKE != 0 {
            out.extend_from_slice(b";9");
        }
        for (color, base) in [(self.0 & FG, 30), (self.0 >> BG_SHIFT & FG, 40)] {
            let _ = match color.checked_sub(1) {
                None => Ok(()),
                Some(c @ 0..8) => write!(out, ";{}", base + c),
                Some(c @ 8..16) => write!(out, ";{}", base + 52 + c),
                Some(c) => write!(out, ";{};5;{c}", base + 8),
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

pub struct Screen {
    pub w: usize,
    pub h: usize,
    cur: Vec<Cell>,
    prev: Vec<Cell>,
    out: Vec<u8>,
}

impl Screen {
    pub fn new() -> Screen {
        Screen { w: 0, h: 0, cur: Vec::new(), prev: Vec::new(), out: Vec::new() }
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

    /// Gives a span a grey background that keeps each cell's own colour.
    pub fn tint(&mut self, x: usize, y: usize, w: usize) {
        for cell in self.span(x, y, w) {
            cell.st = cell.st.tinted();
        }
    }

    /// Writes only the cells that changed since the last flush.
    pub fn flush(&mut self) {
        let mut active: Option<Style> = None;
        for y in 0..self.h {
            let cur = &self.cur[y * self.w..(y + 1) * self.w];
            let prev = &self.prev[y * self.w..(y + 1) * self.w];
            let Some(mut first) = (0..self.w).find(|&x| cur[x] != prev[x]) else { continue };
            let last = (first..self.w).rev().find(|&x| cur[x] != prev[x]).unwrap_or(first);
            while first > 0 && cur[first].ch == CONT {
                first -= 1;
            }
            let _ = write!(self.out, "\x1b[{};{}H", y + 1, first + 1);
            let mut utf8 = [0u8; 4];
            for cell in &cur[first..=last] {
                if cell.ch == CONT {
                    continue;
                }
                if active != Some(cell.st) {
                    cell.st.sgr(&mut self.out);
                    active = Some(cell.st);
                }
                self.out.extend_from_slice(cell.ch.encode_utf8(&mut utf8).as_bytes());
            }
        }
        if self.out.is_empty() {
            return;
        }
        self.out.extend_from_slice(b"\x1b[0m");
        self.prev.copy_from_slice(&self.cur);
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(&self.out);
        let _ = stdout.flush();
        self.out.clear();
    }
}

/// Terminal columns a character occupies. Covers control, combining and the
/// East Asian wide / emoji blocks; everything else is one column.
pub fn char_width(c: char) -> usize {
    let c = c as u32;
    match c {
        0..=0x1f | 0x7f..=0x9f => 0,
        0x20..=0x2ff => 1,
        0x300..=0x36f | 0x200b..=0x200f | 0x2028..=0x202e | 0x2060..=0x2064 | 0xfe00..=0xfe0f | 0xfeff => 0,
        0x1100..=0x115f
        | 0x2e80..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1faff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}
