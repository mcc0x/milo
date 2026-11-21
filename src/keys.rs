//! Decodes keys, mouse events, focus and other terminal replies from input.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use crate::term::{milliseconds, wait_ready};

/// Allow split terminal sequences and multibyte keys one short wait to finish.
const SEQUENCE_WAIT: Duration = Duration::from_millis(25);
const READ_SIZE: usize = 4096;
const PASTE_END: &[u8] = b"\x1b[201~";
/// The longest paste kept for a pane that is taking text.
const PASTE_LIMIT: usize = 1 << 20;

/// A second click on the same item within this long opens it.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Keeps a double click that opened a new view from acting on the controls underneath it.
#[derive(Default)]
pub struct ConfirmClick(Option<(usize, usize, Instant)>);

impl ConfirmClick {
    pub fn opened(&mut self, x: usize, y: usize) {
        self.0 = Some((x, y, Instant::now()));
    }

    pub fn is_repeat(&self, x: usize, y: usize) -> bool {
        self.0.is_some_and(|(a, b, at)| (a, b) == (x, y) && at.elapsed() < DOUBLE_CLICK)
    }

    pub fn clear(&mut self) {
        self.0 = None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Ctrl(u8),
    Enter,
    Esc,
    Backspace,
    Delete,
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
    /// Pointer motion with no button held.
    Hover(usize, usize),
    WheelUp,
    WheelDown,
    /// Pasted text was kept; `Input::pasted` hands it over.
    Paste,
    /// The pane gained or lost keyboard focus.
    Focus(bool),
    /// The terminal reported its default background colour.
    Background([u8; 3]),
    /// The terminal said whether it can show pictures.
    Pictures(bool),
    /// The terminal reported the size of one cell in pixels: width, height.
    CellSize(u16, u16),
    /// The terminal has answered everything it was asked on the way in.
    Answered,
    /// stdin reached end of file: the terminal is gone.
    Eof,
}

/// Which way to move through a list of items.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Next,
    Previous,
}

impl Key {
    /// How far this key scrolls a list with `shown` visible rows, if it scrolls.
    pub fn rows(self, shown: usize) -> Option<isize> {
        let page = isize::try_from(shown.max(2) - 1).unwrap_or(isize::MAX);
        Some(match self {
            Key::Char('j') | Key::Down => 1,
            Key::Char('k') | Key::Up => -1,
            Key::WheelDown => 3,
            Key::WheelUp => -3,
            Key::PgDn | Key::Ctrl(b'f') => page,
            Key::PgUp | Key::Ctrl(b'b') => -page,
            Key::Ctrl(b'd') => page / 2,
            Key::Ctrl(b'u') => -page / 2,
            Key::Char('g') | Key::Home => isize::MIN,
            Key::Char('G') | Key::End => isize::MAX,
            _ => return None,
        })
    }
}

#[derive(Default)]
pub struct Input {
    pending: Vec<u8>,
    /// An overlong sequence is ignored through its terminator, not decoded as keys.
    discard: Option<Sequence>,
    /// The terminal has sent the last answer it was asked for. Until then an
    /// unfinished control string may be one of its answers, still arriving.
    answered: bool,
    paste: Paste,
}

/// Clipboard text between the terminal's paste marks. It is never read as
/// keys, and is kept only for a pane that is taking text.
#[derive(Default)]
struct Paste {
    /// While pasting, how many bytes of the end marker have been seen.
    end: Option<usize>,
    keep: bool,
    /// The text of the paste under way, if it is being kept.
    text: Option<Vec<u8>>,
    too_long: bool,
    /// Finished pastes, oldest first, or `None` for one that was too long to keep.
    done: VecDeque<Option<String>>,
}

impl Paste {
    fn start(&mut self) {
        self.end = Some(0);
        self.text = self.keep.then(Vec::new);
        self.too_long = false;
    }

    fn push(&mut self, bytes: &[u8]) {
        let Some(text) = &mut self.text else { return };
        if self.too_long || text.len() + bytes.len() > PASTE_LIMIT {
            self.too_long = true;
            *text = Vec::new();
        } else {
            text.extend_from_slice(bytes);
        }
    }

    fn finish(&mut self, keys: &mut Vec<Key>) {
        self.end = None;
        if let Some(text) = self.text.take() {
            self.done.push_back((!self.too_long).then(|| String::from_utf8_lossy(&text).into_owned()));
            keys.push(Key::Paste);
        }
    }
}

impl Input {
    /// Whether to keep pasted text for the pane instead of skipping it.
    pub fn keep_pastes(&mut self, keep: bool) {
        self.paste.keep = keep;
    }

    /// The text of the oldest `Key::Paste` not yet handed over, or `None` if it was too long to keep.
    pub fn pasted(&mut self) -> Option<String> {
        self.paste.done.pop_front().flatten()
    }

    pub fn receive(&mut self, keyboard: &mut File, keys: &mut Vec<Key>) {
        let mut buf = [0u8; READ_SIZE];
        let mut read_input = false;
        let mut deadline = None;
        // Give resize signals and file checks a turn even during a long paste.
        for _ in 0..32 {
            let n = match keyboard.read(&mut buf) {
                Ok(0) if read_input => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Ok(0) | Err(_) => {
                    self.finish(keys);
                    keys.push(Key::Eof);
                    return;
                }
                Ok(n) => n,
            };
            read_input = true;
            self.feed(&buf[..n], keys);
            if self.pending.is_empty() || self.discard.is_some() {
                if n < buf.len() && self.paste.end.is_none() {
                    return;
                }
                deadline = None;
            } else {
                let until = *deadline.get_or_insert_with(|| Instant::now() + SEQUENCE_WAIT);
                if !wait_for_sequence(keyboard, until) {
                    break;
                }
            }
        }
        self.finish(keys);
    }

    fn feed(&mut self, mut bytes: &[u8], keys: &mut Vec<Key>) {
        if let Some(sequence) = self.discard {
            // Keep at most one ESC across reads so a split string terminator still works.
            if self.pending.pop().is_some() && bytes.starts_with(b"\\") {
                self.discard = None;
                bytes = &bytes[1..];
            } else if let Some((_, end)) = sequence.end(bytes) {
                self.discard = None;
                bytes = &bytes[end..];
            } else {
                if sequence != Sequence::Csi && bytes.ends_with(b"\x1b") {
                    self.pending.push(0x1b);
                }
                return;
            }
        }
        let decoded = keys.len();
        if self.pending.is_empty() {
            self.pending.extend_from_slice(decode(bytes, &mut self.paste, keys));
        } else {
            self.pending.extend_from_slice(bytes);
            let left = decode(&self.pending, &mut self.paste, keys).len();
            let used = self.pending.len() - left;
            self.pending.drain(..used);
        }
        self.answered |= keys[decoded..].contains(&Key::Answered);
        if self.pending.len() >= READ_SIZE
            && let Some(sequence) = Sequence::of(&self.pending[1..])
        {
            let split_end = sequence != Sequence::Csi && self.pending.ends_with(b"\x1b");
            self.discard = Some(sequence);
            self.pending.truncate(usize::from(split_end));
        }
    }

    fn finish(&mut self, keys: &mut Vec<Key>) {
        if self.discard.is_some() {
            return;
        }
        match Sequence::of(self.pending.get(1..).unwrap_or_default()) {
            None => {
                if self.pending == [0x1b] {
                    keys.push(Key::Esc);
                }
                self.pending.clear();
            }
            // After its last answer the terminal sends no control string, so one
            // that the wait did not finish is an Alt-modified key such as Alt-].
            // Nothing would end it, and it would take every later key with it.
            Some(sequence) if self.answered && sequence != Sequence::Csi => self.pending.clear(),
            Some(_) => {}
        }
    }
}

fn wait_for_sequence(keyboard: &File, until: Instant) -> bool {
    let mut fds = [libc::pollfd { fd: keyboard.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
    loop {
        let wait = until.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            return false;
        }
        match wait_ready(&mut fds, milliseconds(wait)) {
            n if n < 0 && io::Error::last_os_error().kind() == ErrorKind::Interrupted => continue,
            n => return n > 0,
        }
    }
}

/// Passes over clipboard text without turning it into keys, keeping it if a pane wants it.
fn skip_paste(bytes: &[u8], paste: &mut Paste, keys: &mut Vec<Key>) -> usize {
    let Some(mut matched) = paste.end else { return 0 };
    let mut used = 0;
    while used < bytes.len() {
        if matched == 0 {
            let text = &bytes[used..];
            let Some(i) = text.iter().position(|&c| c == PASTE_END[0]) else {
                paste.push(text);
                break;
            };
            paste.push(&text[..i]);
            used += i + 1;
            matched = 1;
        } else {
            let byte = bytes[used];
            used += 1;
            if byte == PASTE_END[matched] {
                matched += 1;
                if matched == PASTE_END.len() {
                    paste.finish(keys);
                    return used;
                }
            } else {
                // The end marker did not complete, so what looked like it was text.
                paste.push(&PASTE_END[..matched]);
                matched = usize::from(byte == PASTE_END[0]);
                if matched == 0 {
                    paste.push(&[byte]);
                }
            }
        }
    }
    paste.end = Some(matched);
    bytes.len()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sequence {
    Csi,
    Osc,
    Apc,
    OtherString,
}

impl Sequence {
    fn of(after_escape: &[u8]) -> Option<Self> {
        Some(match after_escape.first()? {
            b'[' => Self::Csi,
            b']' => Self::Osc,
            b'_' => Self::Apc,
            b'P' | b'X' | b'^' => Self::OtherString,
            _ => return None,
        })
    }

    /// Returns the body length and the offset to resume reading keys.
    fn end(self, body: &[u8]) -> Option<(usize, usize)> {
        if self == Self::Csi {
            let i = body.iter().position(|b| !(0x20..=0x3f).contains(b))?;
            return Some((i, i + usize::from(body[i] != 0x1b)));
        }
        for (i, &byte) in body.iter().enumerate() {
            match byte {
                0x18 | 0x1a => return Some((i, i + 1)), // CAN or SUB cancel a control string.
                7 if self == Self::Osc => return Some((i, i + 1)),
                0x1b if body.get(i + 1) == Some(&b'\\') => return Some((i, i + 2)),
                _ => {}
            }
        }
        None
    }
}

/// Decodes complete keys and returns any partial sequence at the end.
fn decode<'a>(mut bytes: &'a [u8], paste: &mut Paste, keys: &mut Vec<Key>) -> &'a [u8] {
    loop {
        bytes = &bytes[skip_paste(bytes, paste, keys)..];
        let Some((&c, rest)) = bytes.split_first() else { return bytes };
        let (key, tail) = match c {
            0x1b => match rest {
                [] | [b'O'] => return bytes,
                // A new ESC can start a terminal report or bracketed paste.
                [0x1b, ..] => (None, rest),
                [b'O', 0x1b, ..] => (None, &rest[1..]),
                [b'O', final_byte, tail @ ..] => (csi(&[], *final_byte), tail),
                [b'[', b'M', report @ ..] => {
                    // Terminals without SGR mouse mode append three raw bytes to CSI M.
                    if let Some(i) = report.iter().take(3).position(|&byte| byte < b' ') {
                        bytes = &report[i..];
                        continue;
                    }
                    let Some(&[button, x, y]) = report.first_chunk::<3>() else { return bytes };
                    let key = x.checked_sub(33).zip(y.checked_sub(33)).and_then(|(x, y)| {
                        mouse_key(usize::from(button - 32), usize::from(x), usize::from(y))
                    });
                    (key, &report[3..])
                }
                [_, body @ ..] => {
                    let Some(sequence) = Sequence::of(rest) else {
                        // Alt-modified key: drop it rather than act on the bare key.
                        bytes = body;
                        continue;
                    };
                    let Some((end, resume)) = sequence.end(body) else { return bytes };
                    let (answer, tail) = (&body[..end], &body[resume..]);
                    let key = if end >= READ_SIZE {
                        None
                    } else {
                        match (sequence, body[end]) {
                            (Sequence::Csi, b'~') if answer == b"200" => {
                                paste.start();
                                None
                            }
                            (Sequence::Csi, fin @ 0x40..=0x7e) => csi(answer, fin),
                            (_, 0x18 | 0x1a) => None,
                            (Sequence::Osc, _) => background_reply(answer).map(Key::Background),
                            (Sequence::Apc, _) if answer.starts_with(b"Gi=31;") => {
                                Some(Key::Pictures(answer == b"Gi=31;OK"))
                            }
                            _ => None,
                        }
                    };
                    (key, tail)
                }
            },
            b'\r' | b'\n' => (Some(Key::Enter), rest),
            0x7f => (Some(Key::Backspace), rest),
            1..=26 => (Some(Key::Ctrl(c + b'a' - 1)), rest),
            b' '..=b'~' => (Some(Key::Char(char::from(c))), rest),
            _ => {
                let len = match c {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => 0,
                };
                if len > bytes.len() && rest.iter().all(|&byte| byte & 0xc0 == 0x80) {
                    return bytes;
                }
                if len > 0
                    && let Some(ch) = bytes.get(..len).and_then(|b| std::str::from_utf8(b).ok()?.chars().next())
                {
                    (Some(Key::Char(ch)), &bytes[len..])
                } else {
                    (None, rest)
                }
            }
        };
        if let Some(key) = key {
            if matches!(key, Key::Hover(..))
                && let Some(last @ Key::Hover(..)) = keys.last_mut()
            {
                *last = key; // Only the final position can be painted in this frame.
            } else {
                keys.push(key);
            }
        }
        bytes = tail;
    }
}

fn mouse_key(button: usize, x: usize, y: usize) -> Option<Key> {
    // Ignore modifier bits, but never treat motion or an extended button as a press.
    match button & !(4 | 8 | 16) {
        64 => Some(Key::WheelUp),
        65 => Some(Key::WheelDown),
        0 => Some(Key::Click(x, y)),
        35 => Some(Key::Hover(x, y)),
        _ => None,
    }
}

fn csi(params: &[u8], fin: u8) -> Option<Key> {
    if let Some(mouse) = params.strip_prefix(b"<") {
        if fin != b'M' {
            return None;
        }
        let mut it = std::str::from_utf8(mouse).ok()?.split(';').map(|p| p.parse::<usize>().ok());
        let (btn, x, y) = (it.next()??, it.next()??.checked_sub(1)?, it.next()??.checked_sub(1)?);
        return if it.next().is_none() { mouse_key(btn, x, y) } else { None };
    }
    Some(match fin {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'I' if params.is_empty() => Key::Focus(true),
        b'O' if params.is_empty() => Key::Focus(false),
        // "6;height;width t": the size of one cell in pixels.
        b't' => {
            let mut parts = std::str::from_utf8(params.strip_prefix(b"6;")?).ok()?.split(';');
            let (height, width) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
            if parts.next().is_some() {
                return None;
            }
            Key::CellSize(width, height)
        }
        // The terminal describing itself: asked for last, so it ends the answers.
        b'c' if params.starts_with(b"?") => Key::Answered,
        b'~' => match params.split(|&c| c == b';').next()? {
            b"1" | b"7" => Key::Home,
            b"3" => Key::Delete,
            b"4" | b"8" => Key::End,
            b"5" => Key::PgUp,
            b"6" => Key::PgDn,
            _ => return None,
        },
        _ => return None,
    })
}

/// Decodes an OSC 11 background-colour reply with one to four hex digits per component.
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
        u8::try_from(value * 255 / ((1 << (4 * digits.len())) - 1)).ok()
    };
    let rgb = [component()?, component()?, component()?];
    parts.next().is_none().then_some(rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Key> {
        let mut keys = Vec::new();
        let mut input = Input::default();
        input.feed(bytes, &mut keys);
        input.finish(&mut keys);
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
                Key::Char('j'),
                Key::Background([17, 34, 51]),
                Key::Char('k'),
                Key::Background([0, 0, 255]),
                Key::Char('l'),
            ]
        );
        assert_eq!(keys(b"j\x1b]10;rgb:1/2/3\x07k"), [Key::Char('j'), Key::Char('k')]);
    }

    #[test]
    fn keys_and_sequences_decode() {
        assert_eq!(keys(b"j\r\x1b[A\x1b[6~\x03"), [Key::Char('j'), Key::Enter, Key::Up, Key::PgDn, Key::Ctrl(b'c')]);
        assert_eq!(keys("é".as_bytes()), [Key::Char('é')]);
        assert_eq!(keys(b"\x1b"), [Key::Esc]);
        assert_eq!(keys(b"\x1b[I\x1b[O"), [Key::Focus(true), Key::Focus(false)]);
        assert_eq!(keys(b"\x1b[1I\x1b[1O"), []);
    }

    #[test]
    fn what_the_terminal_says_of_itself_is_read_between_the_keys() {
        assert_eq!(
            keys(b"j\x1b_Gi=31;OK\x1b\\k\x1b[6;16;8t\x1b[?62;22cl"),
            [Key::Char('j'), Key::Pictures(true), Key::Char('k'), Key::CellSize(8, 16), Key::Answered, Key::Char('l')]
        );
        assert_eq!(keys(b"\x1b_Gi=31;ENOTSUPPORTED\x1b\\"), [Key::Pictures(false)]);
        assert_eq!(keys(b"\x1b_Gi=31;Error: expected ;OK\x1b\\"), [Key::Pictures(false)]);
        assert_eq!(keys(b"\x1b[6;16;8;9t"), []);
    }

    #[test]
    fn mouse_reports_decode() {
        assert_eq!(keys(b"\x1b[<0;8;3M"), [Key::Click(7, 2)]);
        assert_eq!(keys(b"\x1b[<0;8;3m"), []);
        assert_eq!(keys(b"\x1b[<35;8;3M\x1b[<39;2;1M"), [Key::Hover(1, 0)]);
        assert_eq!(keys(b"\x1b[<35;8;3Mj\x1b[<35;2;1M"), [Key::Hover(7, 2), Key::Char('j'), Key::Hover(1, 0)]);
        assert_eq!(keys(b"\x1b[<32;8;3M\x1b[<128;8;3M"), []);
        assert_eq!(keys(b"\x1b[<64;1;1M\x1b[<65;1;1M"), [Key::WheelUp, Key::WheelDown]);
        assert_eq!(keys(b"\x1b[<0;8;3;9M"), []);
    }

    #[test]
    fn a_split_legacy_mouse_report_preserves_its_coordinates() {
        let mut input = Input::default();
        let mut keys = Vec::new();
        for part in [b"\x1b[M ".as_slice(), b"q", b"kk"] {
            input.feed(part, &mut keys);
            input.finish(&mut keys);
        }
        assert_eq!(keys, [Key::Click(80, 74), Key::Char('k')]);
    }

    #[test]
    fn terminal_replies_and_multibyte_keys_can_span_reads() {
        let mut input = Input::default();
        let mut keys = Vec::new();
        for byte in "j\x1b[<0;8;3M界\x1b]11;rgb:0/0/f\x1b\\\x1b_Gi=31;OK\x1b\\k".bytes() {
            input.feed(&[byte], &mut keys);
        }
        assert_eq!(
            keys,
            [
                Key::Char('j'),
                Key::Click(7, 2),
                Key::Char('界'),
                Key::Background([0, 0, 255]),
                Key::Pictures(true),
                Key::Char('k'),
            ]
        );
    }

    #[test]
    fn a_paste_cannot_quit_the_pane_or_act_on_it() {
        let mut input = Input::default();
        let mut keys = Vec::new();
        for part in [b"j\x1b[".as_slice(), b"200~s q\r\x03\x1b[201", b"x\x1b", b"[201", b"~k"] {
            input.feed(part, &mut keys);
        }
        assert_eq!(keys, [Key::Char('j'), Key::Char('k')]);
    }

    #[test]
    fn a_paste_after_an_escape_prefix_cannot_act_on_the_pane() {
        assert_eq!(keys(b"\x1b\x1b[200~q\x03\x1b[201~k"), [Key::Char('k')]);
        assert_eq!(keys(b"\x1bO\x1b[200~q\x03\x1b[201~k"), [Key::Char('k')]);
    }

    #[test]
    fn a_failed_paste_end_match_does_not_carry_over_to_the_next_read() {
        let mut input = Input::default();
        let mut keys = Vec::new();
        for part in [b"\x1b[200~\x1b[20".as_slice(), b"x more text", b"1~q\x03\x1b[201~k"] {
            input.feed(part, &mut keys);
        }
        assert_eq!(keys, [Key::Char('k')]);
    }

    #[test]
    fn a_pause_inside_a_paste_start_does_not_turn_pasted_text_into_keys() {
        let mut input = Input::default();
        let mut keys = Vec::new();
        input.feed(b"\x1b[20", &mut keys);
        input.finish(&mut keys);
        input.feed(b"0~q\x03\x1b[201~k", &mut keys);
        assert_eq!(keys, [Key::Char('k')]);
    }

    #[test]
    fn unknown_and_overlong_terminal_strings_do_not_type_commands() {
        assert_eq!(keys(b"\x1bPq\x03\x1b\\k"), [Key::Char('k')]);
        let mut input = Input::default();
        let mut keys = Vec::new();
        input.feed(b"\x1b]", &mut keys);
        input.feed(&[b'q'; READ_SIZE], &mut keys);
        input.finish(&mut keys);
        input.feed(b"q\x03\x1b", &mut keys);
        input.finish(&mut keys);
        input.feed(b"\\k", &mut keys);
        assert_eq!(keys, [Key::Char('k')]);
    }

    #[test]
    fn an_alt_key_that_opens_a_control_string_does_not_take_the_keys_after_it() {
        for opener in *b"]P_^X" {
            let mut input = Input::default();
            let mut keys = Vec::new();
            input.feed(b"\x1b[?62;22c", &mut keys);
            input.feed(&[0x1b, opener], &mut keys);
            input.finish(&mut keys);
            input.feed(b"q\x03", &mut keys);
            assert_eq!(keys, [Key::Answered, Key::Char('q'), Key::Ctrl(b'c')], "Alt-{}", char::from(opener));
        }

        // Before the last answer, a pause inside an answer still does not type its text.
        let mut input = Input::default();
        let mut keys = Vec::new();
        input.feed(b"\x1b]", &mut keys);
        input.finish(&mut keys);
        input.feed(b"11;rgb:0/0/f\x1b\\\x1b[?62;22ck", &mut keys);
        assert_eq!(keys, [Key::Background([0, 0, 255]), Key::Answered, Key::Char('k')]);
    }

    #[test]
    fn a_malformed_utf8_key_does_not_swallow_the_next_key() {
        assert_eq!(keys(b"\xf0\x80q"), [Key::Char('q')]);
    }

    #[test]
    fn the_keys_that_erase_text_decode() {
        assert_eq!(keys(b"\x7f\x1b[3~\x08\t"), [Key::Backspace, Key::Delete, Key::Ctrl(b'h'), Key::Ctrl(b'i')]);
    }

    #[test]
    fn a_kept_paste_is_handed_over_whole_and_never_read_as_keys() {
        let mut input = Input::default();
        let mut keys = Vec::new();
        input.keep_pastes(true);
        // The end marker can arrive in pieces, and a piece that only looks like its start is text.
        for part in [b"j\x1b[".as_slice(), b"200~s q\r\x03\x1b[201", b"x\x1b", b"[201", b"~k"] {
            input.feed(part, &mut keys);
        }
        assert_eq!(keys, [Key::Char('j'), Key::Paste, Key::Char('k')]);
        assert_eq!(input.pasted().as_deref(), Some("s q\r\x03\x1b[201x"));
        assert_eq!(input.pasted(), None);

        // A pane that is not taking text gets neither the text nor a key for it.
        keys.clear();
        input.keep_pastes(false);
        input.feed(b"\x1b[200~skipped\x1b[201~k", &mut keys);
        assert_eq!(keys, [Key::Char('k')]);
        assert_eq!(input.pasted(), None);
    }

    #[test]
    fn a_paste_too_long_to_keep_is_reported_without_its_text() {
        let mut input = Input::default();
        let mut keys = Vec::new();
        input.keep_pastes(true);
        input.feed(b"\x1b[200~", &mut keys);
        for _ in 0..=PASTE_LIMIT / READ_SIZE {
            input.feed(&[b'x'; READ_SIZE], &mut keys);
        }
        input.feed(b"\x1b[201~\x1b[200~short\x1b[201~", &mut keys);
        assert_eq!(keys, [Key::Paste, Key::Paste]);
        assert_eq!(input.pasted(), None);
        assert_eq!(input.pasted().as_deref(), Some("short"));
    }
}
