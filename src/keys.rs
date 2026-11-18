//! Decodes keys, mouse events, focus and other terminal replies from input.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;

use crate::term::wait_ready;

/// Milliseconds to wait for the rest of a split escape sequence.
const SEQUENCE_MS: i32 = 25;

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

/// Drains everything currently readable on stdin into `keys`.
pub fn receive(keyboard: &mut File, keys: &mut Vec<Key>) {
    let mut buf = [0u8; 4096];
    let mut used = 0;
    let mut overflow = Vec::new();
    let mut retried = false;
    loop {
        match keyboard.read(&mut buf[used..]) {
            // Input was ready, so an empty first read is EOF.
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
        if retried || !unfinished(bytes) || wait_ready(&mut fds, SEQUENCE_MS) <= 0 {
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
        // An answer about pictures always ends with an ESC of its own.
        [b'_', ..] => true,
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
                // An answer about pictures: "Gi=31;OK", ended by ESC \.
                Some(b'_') => {
                    let start = i + 1;
                    let Some(len) = b[start..].iter().position(|&c| c == 0x1b) else { return };
                    let answer = &b[start..start + len];
                    if answer.starts_with(b"G") {
                        keys.push(Key::Pictures(answer.ends_with(b";OK")));
                    }
                    i = start + len + 2;
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
        // "6;height;width t": the size of one cell in pixels.
        b't' => {
            let mut parts = std::str::from_utf8(params.strip_prefix(b"6;")?).ok()?.split(';');
            let (height, width) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
            Key::CellSize(width, height)
        }
        // The terminal describing itself: asked for last, so it ends the answers.
        b'c' if params.starts_with(b"?") => Key::Answered,
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
                Key::Char('j'),
                Key::Background([17, 34, 51]),
                Key::Char('k'),
                Key::Background([0, 0, 255]),
                Key::Char('l'),
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
    fn what_the_terminal_says_of_itself_is_read_between_the_keys() {
        assert_eq!(
            keys(b"j\x1b_Gi=31;OK\x1b\\k\x1b[6;16;8t\x1b[?62;22cl"),
            [Key::Char('j'), Key::Pictures(true), Key::Char('k'), Key::CellSize(8, 16), Key::Answered, Key::Char('l')]
        );
        assert_eq!(keys(b"\x1b_Gi=31;ENOTSUPPORTED\x1b\\"), [Key::Pictures(false)]);
        // An answer still arriving is waited for, not read as an Esc and some letters.
        assert!(unfinished(b"\x1b_Gi=31;O") && !unfinished(b"\x1b_Gi=31;OK\x1b\\"));
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
}
