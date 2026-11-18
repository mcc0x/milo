//! Turns a picture file into something a terminal can show.
//!
//! Terminals that show pictures take PNG, so every picture becomes one,
//! no larger than a pane could use. The converting is left to `sips`, the
//! image tool that comes with macOS; elsewhere only PNG files can be shown.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::term;

/// Picture files larger than this are not opened.
const FILE_LIMIT: u64 = 64 << 20;
/// The longest side kept, in pixels: more than any pane has to show it with.
const LONGEST_SIDE: u32 = 1600;
/// How long a conversion may take before it is given up on.
const DEADLINE: Duration = Duration::from_secs(5);
/// Bytes of encoded picture sent to the terminal at a time, as it asks.
const CHUNK: usize = 4096;

/// File endings shown as pictures, in lower case.
const ENDINGS: [&str; 15] =
    ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "ico", "icns", "svg", "pdf", "psd"];

/// Each picture gets a number of its own, by which the terminal knows it.
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Whether a file with this name is shown as a picture, going by its ending.
pub fn is_picture(name: &str) -> bool {
    let ending = name.rsplit_once('.').map_or("", |(_, ending)| ending);
    ENDINGS.iter().any(|known| ending.eq_ignore_ascii_case(known))
}

/// A picture ready for the terminal: PNG data and its size in pixels.
pub struct Picture {
    png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub id: u32,
}

impl Picture {
    /// Reads the picture at `path`, or says in plain words why it cannot be
    /// shown. A link is not followed: what it points to may be anywhere.
    pub fn open(path: &Path) -> Result<Self, &'static str> {
        let meta = std::fs::symlink_metadata(path).map_err(|_| "This file is gone.")?;
        if !meta.is_file() {
            return Err("Not a file, so there is nothing to show.");
        }
        if meta.len() > FILE_LIMIT {
            return Err("Too large to show.");
        }
        let png = to_png(path).ok_or("This picture cannot be shown.")?;
        let (width, height) = png_size(&png).ok_or("This picture cannot be shown.")?;
        Ok(Self { png, width, height, id: NEXT_ID.fetch_add(1, Ordering::Relaxed) })
    }

    /// The columns and rows the picture takes in a space of `cols` by `rows`
    /// cells of `cell` pixels each: its own size where that fits, otherwise
    /// as large as fits with its proportions kept.
    pub fn fit(&self, cell: (u16, u16), cols: usize, rows: usize) -> (usize, usize) {
        let (cell_w, cell_h) = (u64::from(cell.0.max(1)), u64::from(cell.1.max(1)));
        let (w, h) = (u64::from(self.width), u64::from(self.height));
        let (room_w, room_h) = (cols as u64 * cell_w, rows as u64 * cell_h);
        // Compared as cross products, so nothing is lost to rounding.
        let (shown_w, shown_h) = if w <= room_w && h <= room_h {
            (w, h)
        } else if w * room_h > h * room_w {
            (room_w, h * room_w / w)
        } else {
            (w * room_h / h, room_h)
        };
        let cells = |pixels: u64, cell: u64, most: usize| {
            usize::try_from(pixels.div_ceil(cell)).map_or(most, |n| n.clamp(1, most))
        };
        (cells(shown_w, cell_w, cols.max(1)), cells(shown_h, cell_h, rows.max(1)))
    }

    /// Writes what hands the picture's data to a terminal that speaks the
    /// kitty graphics protocol: the PNG in base64, in pieces, under its number.
    pub fn transmit(&self, out: &mut Vec<u8>) {
        let encoded = base64(&self.png);
        let mut pieces = encoded.chunks(CHUNK).peekable();
        let mut first = true;
        while let Some(piece) = pieces.next() {
            let more = u8::from(pieces.peek().is_some());
            // Only the first piece says what the data is; `q=2` asks the
            // terminal not to answer.
            let _ = if first {
                write!(out, "\x1b_Ga=t,f=100,i={},q=2,m={more};", self.id)
            } else {
                write!(out, "\x1b_Gm={more};")
            };
            out.extend_from_slice(piece);
            out.extend_from_slice(b"\x1b\\");
            first = false;
        }
    }
}

/// The width and height a PNG declares in its header.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let header = png.strip_prefix(b"\x89PNG\r\n\x1a\n")?;
    let field = |at: usize| Some(u32::from_be_bytes(header.get(at..at + 4)?.try_into().ok()?));
    // The first chunk is the header: its length and name, then the two sizes.
    let (width, height) = (field(8)?, field(12)?);
    (header.get(4..8)? == b"IHDR" && width > 0 && height > 0).then_some((width, height))
}

/// The picture as PNG data no longer than `LONGEST_SIDE` on a side. A PNG
/// that is already small enough is used as it is.
fn to_png(path: &Path) -> Option<Vec<u8>> {
    let is_png = path.extension().is_some_and(|ending| ending.eq_ignore_ascii_case("png"));
    if is_png {
        let png = std::fs::read(path).ok()?;
        if png_size(&png).is_some_and(|(w, h)| w.max(h) <= LONGEST_SIDE) {
            return Some(png);
        }
    }
    convert(path)
}

/// A file of this process's own in the temporary directory, removed when
/// it goes out of use.
struct Scratch(PathBuf);

impl Scratch {
    fn new(id: u32) -> Self {
        Self(std::env::temp_dir().join(format!("milo-{}-{id}.png", std::process::id())))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Converts with `sips`, then shrinks the result if it is larger than a pane
/// could use. Shrinking is a second step because `sips` would enlarge a
/// small picture to the size asked for.
#[cfg(target_os = "macos")]
fn convert(path: &Path) -> Option<Vec<u8>> {
    let scratch = Scratch::new(NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let sips = |args: &[&std::ffi::OsStr]| {
        let mut command = Command::new("/usr/bin/sips");
        command.args(args).arg("--out").arg(&scratch.0);
        finishes(command)
    };
    if !sips(&["-s".as_ref(), "format".as_ref(), "png".as_ref(), path.as_os_str()]) {
        return None;
    }
    let png = std::fs::read(&scratch.0).ok()?;
    if png_size(&png)?.0.max(png_size(&png)?.1) <= LONGEST_SIDE {
        return Some(png);
    }
    let side = LONGEST_SIDE.to_string();
    sips(&["-Z".as_ref(), side.as_ref(), scratch.0.as_os_str()]).then(|| std::fs::read(&scratch.0).ok())?
}

/// Without a converter at hand, only what is already a PNG can be shown.
#[cfg(not(target_os = "macos"))]
fn convert(_: &Path) -> Option<Vec<u8>> {
    None
}

/// Runs a command to its end and says whether it succeeded. One that takes
/// too long, or outlives a request to quit, is stopped: a file made to
/// stall a converter must not stall milo.
#[cfg_attr(not(target_os = "macos"), expect(dead_code, reason = "only macOS has a converter to run"))]
fn finishes(mut command: Command) -> bool {
    let started = Instant::now();
    let Ok(mut child) = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < DEADLINE && !term::TERMINATED.load(Ordering::Relaxed) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Standard base64, with padding.
fn base64(data: &[u8]) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for group in data.chunks(3) {
        let bits = group.iter().fold(0u32, |bits, &byte| bits << 8 | u32::from(byte)) << (8 * (3 - group.len()));
        for place in 0..4 {
            // A group of one or two bytes fills only its first two or three places.
            let letter = if place <= group.len() { ALPHABET[(bits >> (18 - 6 * place) & 63) as usize] } else { b'=' };
            out.push(letter);
        }
    }
    out
}

#[cfg(test)]
impl Picture {
    /// The start of a PNG that declares this size: enough to be taken for
    /// a picture, which is as far as milo itself ever reads one.
    pub fn sample_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(width.to_be_bytes());
        png.extend(height.to_be_bytes());
        png
    }

    pub fn sample(width: u32, height: u32) -> Self {
        Self { png: Self::sample_png(width, height), width, height, id: 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized(width: u32, height: u32) -> Picture {
        Picture::sample(width, height)
    }

    #[test]
    fn pictures_are_known_by_their_ending_whatever_its_case() {
        assert!(is_picture("logo.png") && is_picture("docs/Photo.JPG") && is_picture("a.b.svg"));
        assert!(!is_picture("main.rs") && !is_picture("png") && !is_picture("README"));
    }

    #[test]
    fn base64_pads_what_does_not_fill_a_group() {
        let text = |data: &[u8]| String::from_utf8(base64(data)).unwrap();
        assert_eq!([text(b""), text(b"f"), text(b"fo"), text(b"foo")], ["", "Zg==", "Zm8=", "Zm9v"]);
        assert_eq!(text(b"\xff\xfe\xfd\xfc"), "//79/A==");
    }

    #[test]
    fn a_png_says_its_own_size_and_anything_else_says_nothing() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend([0, 0, 1, 44, 0, 0, 0, 200]);
        assert_eq!(png_size(&png), Some((300, 200)));
        assert_eq!(png_size(&png[..20]), None);
        assert_eq!(png_size(b"GIF89a, not a PNG at all"), None);
    }

    #[test]
    fn a_small_picture_keeps_its_size_and_a_large_one_keeps_its_shape() {
        // Cells 8 pixels wide and 16 tall, in a space of 80 by 20.
        let cell = (8, 16);
        assert_eq!(sized(64, 64).fit(cell, 80, 20), (8, 4));
        // Twice as wide as tall, limited by the width: 640 by 320 pixels.
        assert_eq!(sized(2000, 1000).fit(cell, 80, 20), (80, 20));
        // Tall and thin, limited by the height: 160 by 320 pixels.
        assert_eq!(sized(500, 1000).fit(cell, 80, 20), (20, 20));
        // Never less than one cell, nor more than there is.
        assert_eq!(sized(1, 1).fit(cell, 80, 20), (1, 1));
        assert_eq!(sized(4000, 10).fit(cell, 80, 20), (80, 1));
    }

    #[test]
    fn a_picture_is_sent_in_pieces_and_only_the_last_says_so() {
        let picture = Picture { png: vec![7; CHUNK], width: 1, height: 1, id: 9 };
        let mut out = Vec::new();
        picture.transmit(&mut out);
        let text = String::from_utf8(out).unwrap();
        let pieces: Vec<&str> = text.split_terminator("\x1b\\").collect();
        assert_eq!(pieces.len(), 2);
        assert!(pieces[0].starts_with("\x1b_Ga=t,f=100,i=9,q=2,m=1;"));
        assert!(pieces[1].starts_with("\x1b_Gm=0;"));
    }
}
