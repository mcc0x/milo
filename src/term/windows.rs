//! The Windows console: raw keyboard input as terminal sequences, mode changes and their restoration.
//!
//! The console reports keys as input records. With virtual terminal input on, it turns keys into
//! the same escape sequences a Unix terminal sends, so the shared decoder reads them unchanged.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::sync::atomic::Ordering;

use super::{RESIZED, TERMINATED, WAKE};

const ENABLE_PROCESSED_INPUT: u32 = 0x1;
const ENABLE_LINE_INPUT: u32 = 0x2;
const ENABLE_ECHO_INPUT: u32 = 0x4;
const ENABLE_WINDOW_INPUT: u32 = 0x8;
const ENABLE_MOUSE_INPUT: u32 = 0x10;
const ENABLE_QUICK_EDIT_MODE: u32 = 0x40;
const ENABLE_EXTENDED_FLAGS: u32 = 0x80;
const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x200;
const ENABLE_PROCESSED_OUTPUT: u32 = 0x1;
const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x4;
const DISABLE_NEWLINE_AUTO_RETURN: u32 = 0x8;
const UTF8: u32 = 65001;
const KEY_EVENT: u16 = 0x1;
const WINDOW_BUFFER_SIZE_EVENT: u16 = 0x4;

#[repr(C, align(4))]
#[derive(Clone, Copy, Default)]
struct InputRecord {
    kind: u16,
    padding: u16,
    /// The event, in the layout of a key record: down (two words), repeat count, virtual key,
    /// scan code, UTF-16 unit and control key state (two words).
    event: [u16; 8],
}

#[repr(C)]
#[derive(Default)]
struct ScreenBufferInfo {
    size: [i16; 2],
    cursor: [i16; 2],
    attributes: u16,
    window: [i16; 4],
    maximum: [i16; 2],
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleMode(console: RawHandle, mode: *mut u32) -> i32;
    fn SetConsoleMode(console: RawHandle, mode: u32) -> i32;
    safe fn GetConsoleOutputCP() -> u32;
    safe fn SetConsoleOutputCP(page: u32) -> i32;
    fn FlushConsoleInputBuffer(console: RawHandle) -> i32;
    fn GetNumberOfConsoleInputEvents(console: RawHandle, count: *mut u32) -> i32;
    fn ReadConsoleInputW(console: RawHandle, records: *mut InputRecord, length: u32, read: *mut u32) -> i32;
    fn GetConsoleScreenBufferInfo(console: RawHandle, info: *mut ScreenBufferInfo) -> i32;
    fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

fn mode(console: &impl AsRawHandle) -> io::Result<u32> {
    let mut mode = 0;
    // SAFETY: The handle is open and the mode output is writable.
    if unsafe { GetConsoleMode(console.as_raw_handle(), &raw mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mode)
}

fn set_mode(console: &impl AsRawHandle, mode: u32) -> io::Result<()> {
    // SAFETY: The handle is open; an unsupported mode only fails the call.
    if unsafe { SetConsoleMode(console.as_raw_handle(), mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn flush_input(console: &impl AsRawHandle) {
    // SAFETY: The handle is open; flushing only discards unread input records.
    unsafe { FlushConsoleInputBuffer(console.as_raw_handle()) };
}

/// The console's own input, whatever standard input was redirected from.
pub(crate) fn keyboard() -> io::Result<Keyboard> {
    let file = OpenOptions::new().read(true).write(true).open("CONIN$")?;
    Ok(Keyboard { file, pending: Vec::new(), surrogate: None })
}

/// The console's active screen buffer. Reading is needed to ask for its size.
pub(crate) fn output() -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open("CONOUT$")
}

/// The console modes and output code page in force before the pane, to put back on the way out.
pub(crate) struct Saved {
    input: u32,
    output: u32,
    page: u32,
}

/// Turns off line editing, echo and Ctrl-C handling, asks for keys as terminal sequences, and
/// lets output carry terminal sequences in UTF-8.
pub(crate) fn enter(keyboard: &Keyboard, output: &File) -> io::Result<Saved> {
    let saved = Saved { input: mode(&keyboard.file)?, output: mode(output)?, page: GetConsoleOutputCP() };
    flush_input(&keyboard.file);
    let input = (saved.input
        & !(ENABLE_PROCESSED_INPUT
            | ENABLE_LINE_INPUT
            | ENABLE_ECHO_INPUT
            | ENABLE_MOUSE_INPUT
            | ENABLE_QUICK_EDIT_MODE))
        | ENABLE_WINDOW_INPUT
        | ENABLE_EXTENDED_FLAGS
        | ENABLE_VIRTUAL_TERMINAL_INPUT;
    set_mode(&keyboard.file, input)?;
    let changed = set_mode(
        output,
        saved.output | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN,
    )
    .and_then(|()| if SetConsoleOutputCP(UTF8) != 0 { Ok(()) } else { Err(io::Error::last_os_error()) });
    if let Err(error) = changed {
        restore(keyboard, output, &saved);
        return Err(error);
    }
    Ok(saved)
}

/// Puts the modes back and discards keys the pane did not read: they are not the shell's.
pub(crate) fn restore(keyboard: &Keyboard, output: &File, saved: &Saved) {
    let _ = set_mode(&keyboard.file, saved.input);
    let _ = set_mode(output, saved.output);
    SetConsoleOutputCP(saved.page);
    flush_input(&keyboard.file);
}

/// The visible window's columns and rows.
pub(crate) fn size(output: &impl AsRawHandle) -> Option<(usize, usize)> {
    let mut info = ScreenBufferInfo::default();
    // SAFETY: The record is writable and sized for this call.
    if unsafe { GetConsoleScreenBufferInfo(output.as_raw_handle(), &raw mut info) } == 0 {
        return None;
    }
    let [left, top, right, bottom] = info.window;
    let columns = usize::try_from(i32::from(right) - i32::from(left) + 1).ok()?;
    let rows = usize::try_from(i32::from(bottom) - i32::from(top) + 1).ok()?;
    (columns > 0 && rows > 0).then_some((columns, rows))
}

unsafe extern "system" fn on_control(_event: u32) -> i32 {
    TERMINATED.store(true, Ordering::Relaxed);
    crate::sys::wake(WAKE.load(Ordering::Relaxed));
    1
}

/// Ctrl-Break, closing the console window, logging off and shutting down end the pane the way
/// termination signals do on Unix. Ctrl-C arrives as a key while the pane has the console.
pub(crate) fn listen() -> io::Result<()> {
    // SAFETY: The handler only stores atomics and writes a byte to a pipe, and lives for the program.
    if unsafe { SetConsoleCtrlHandler(Some(on_control), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Console keyboard input as the bytes a terminal would send. A read never waits: with nothing
/// to give it fails with `WouldBlock`, as a non-blocking Unix terminal does.
pub(crate) struct Keyboard {
    file: File,
    /// Bytes decoded but not yet handed out.
    pending: Vec<u8>,
    /// The first half of a character outside the Basic Multilingual Plane.
    surrogate: Option<u16>,
}

impl AsRawHandle for Keyboard {
    fn as_raw_handle(&self) -> RawHandle {
        self.file.as_raw_handle()
    }
}

impl Keyboard {
    fn push_unit(&mut self, unit: u16) {
        let units: &[u16] = match (self.surrogate.take(), unit) {
            (None, 0xd800..=0xdbff) => {
                self.surrogate = Some(unit);
                return;
            }
            (Some(high), 0xdc00..=0xdfff) => &[high, unit],
            (_, unit) => &[unit],
        };
        for decoded in char::decode_utf16(units.iter().copied()) {
            let c = decoded.unwrap_or(char::REPLACEMENT_CHARACTER);
            self.pending.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }

    /// A key the console did not turn into characters, as the sequence a terminal sends for it.
    fn push_key(&mut self, virtual_key: u16) {
        let sequence: &[u8] = match virtual_key {
            0x21 => b"\x1b[5~",
            0x22 => b"\x1b[6~",
            0x23 => b"\x1b[F",
            0x24 => b"\x1b[H",
            0x25 => b"\x1b[D",
            0x26 => b"\x1b[A",
            0x27 => b"\x1b[C",
            0x28 => b"\x1b[B",
            0x2d => b"\x1b[2~",
            0x2e => b"\x1b[3~",
            _ => return,
        };
        self.pending.extend_from_slice(sequence);
    }

    fn take_records(&mut self) -> io::Result<()> {
        let handle = self.file.as_raw_handle();
        let mut count = 0;
        // SAFETY: The handle is open and the count output is writable.
        if unsafe { GetNumberOfConsoleInputEvents(handle, &raw mut count) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            return Ok(());
        }
        let mut records = [InputRecord::default(); 64];
        let mut read = 0;
        // SAFETY: The buffer holds 64 writable records and the console holds at least one, so this does not wait.
        if unsafe { ReadConsoleInputW(handle, records.as_mut_ptr(), 64, &raw mut read) } == 0 {
            return Err(io::Error::last_os_error());
        }
        for record in &records[..usize::try_from(read).unwrap_or(0).min(records.len())] {
            match record.kind {
                KEY_EVENT if record.event[0] != 0 || record.event[1] != 0 => {
                    let [_, _, repeat, virtual_key, _, unit, ..] = record.event;
                    for _ in 0..repeat.max(1) {
                        if unit != 0 {
                            self.push_unit(unit);
                        } else {
                            self.push_key(virtual_key);
                        }
                    }
                }
                WINDOW_BUFFER_SIZE_EVENT => RESIZED.store(true, Ordering::Relaxed),
                _ => {}
            }
        }
        Ok(())
    }
}

impl Read for Keyboard {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            self.take_records()?;
        }
        if self.pending.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}
