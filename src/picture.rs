//! Turns a picture file into something a terminal can show.
//!
//! Terminals that show pictures take PNG, so every picture becomes one,
//! no larger than a pane could use. The converting is left to `sips`, the
//! image tool that comes with macOS; elsewhere only PNG files can be shown.

use std::borrow::Cow;
#[cfg(target_os = "macos")]
use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

#[cfg(target_os = "macos")]
use crate::fs_ops::{self, Temp};
#[cfg(target_os = "macos")]
use crate::read::{self, Content};
#[cfg(target_os = "macos")]
use crate::term;

/// Picture files larger than this are not opened.
pub const FILE_LIMIT: u64 = 64 << 20;
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
    png: Cow<'static, [u8]>,
    pub width: u32,
    pub height: u32,
    pub id: u32,
}

impl Picture {
    /// Prepares a picture, returning the original bytes if it cannot be shown.
    pub fn decode(bytes: Vec<u8>, path: &Path) -> Result<Self, Vec<u8>> {
        Self::decode_with(bytes, path, DEADLINE)
    }

    fn decode_with(bytes: Vec<u8>, path: &Path, deadline: Duration) -> Result<Self, Vec<u8>> {
        let size = png_size(&bytes);
        if let Some((width, height)) = size
            && width.max(height) <= LONGEST_SIDE
        {
            return Ok(Self::with_size(bytes.into(), width, height));
        }
        convert(&bytes, path, size, deadline)
            .and_then(|png| Self::png(png.into()))
            .filter(|picture| picture.width.max(picture.height) <= LONGEST_SIDE)
            .ok_or(bytes)
    }

    /// Opens a PNG built into milo without copying its bytes.
    pub fn embedded(png: &'static [u8]) -> Option<Self> {
        Self::png(png.into())
    }

    /// Opens PNG bytes, which can be borrowed or already owned.
    pub fn png(png: Cow<'static, [u8]>) -> Option<Self> {
        let (width, height) = png_size(&png)?;
        Some(Self::with_size(png, width, height))
    }

    fn with_size(png: Cow<'static, [u8]>, width: u32, height: u32) -> Self {
        Self { png, width, height, id: NEXT_ID.fetch_add(1, Ordering::Relaxed) }
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
        let encoded_len = self.png.len().div_ceil(3) * 4;
        out.reserve(encoded_len + encoded_len.div_ceil(CHUNK) * 12 + 32);
        let mut pieces = self.png.chunks(CHUNK / 4 * 3).peekable();
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
            append_base64(piece, out);
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

/// Find the JPEG frame header without decoding or scanning the image data.
#[cfg(target_os = "macos")]
fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    let be16 = |a, b| u16::from_be_bytes([a, b]);
    let mut rest = jpeg.strip_prefix(b"\xff\xd8")?;
    loop {
        let mut marker = rest.strip_prefix(b"\xff")?;
        while let Some(tail) = marker.strip_prefix(b"\xff") {
            marker = tail;
        }
        let (&kind, [a, b, data @ ..]) = marker.split_first()? else { return None };
        if matches!(kind, 0xd9 | 0xda) {
            return None;
        }
        let length = usize::from(be16(*a, *b)).checked_sub(2)?;
        let field = data.get(..length)?;
        // Start of frame, excluding the Huffman and arithmetic tables and JPG extension.
        if matches!(kind, 0xc0..=0xcf) && !matches!(kind, 0xc4 | 0xc8 | 0xcc) {
            let [_, h1, h2, w1, w2, ..] = *field else { return None };
            let (w, h) = (u32::from(be16(w1, w2)), u32::from(be16(h1, h2)));
            return (w != 0 && h != 0).then_some((w, h));
        }
        rest = &data[length..];
    }
}

/// Convert a private copy: the original path may no longer name the file we opened.
#[cfg(target_os = "macos")]
fn convert(bytes: &[u8], path: &Path, size: Option<(u32, u32)>, deadline: Duration) -> Option<Vec<u8>> {
    let size = size.or_else(|| jpeg_size(bytes));
    let dir = std::env::temp_dir();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let prefix = format!("milo-{}-{id}", std::process::id());
    let source = dir.join(format!("{prefix}-in")).with_extension(path.extension().unwrap_or_default());
    let (input, mut file) = Temp::create(source).ok()?;
    file.write_all(bytes).ok()?;
    drop(file);
    let (output, _) = Temp::create(dir.join(format!("{prefix}-out.png"))).ok()?;

    let sips = |source: &Path, resize: bool| {
        let mut command = Command::new("/usr/bin/sips");
        if resize {
            command.args(["-Z", &LONGEST_SIDE.to_string()]);
        }
        command.args(["-s", "format", "png"]).arg(source).arg("--out").arg(output.path());
        finishes(command, deadline)
    };
    // `-Z` can enlarge a small picture. For unknown formats only resize
    // after the converted PNG tells us it is too large.
    if !sips(input.path(), size.is_some_and(|(w, h)| w.max(h) > LONGEST_SIDE)) {
        return None;
    }
    if size.is_none() {
        let mut header = [0; 24];
        fs_ops::open_read(output.path()).ok()?.read_exact(&mut header).ok()?;
        let (w, h) = png_size(&header)?;
        if w.max(h) > LONGEST_SIDE && !sips(output.path(), true) {
            return None;
        }
    }
    match read::file(output.path(), FILE_LIMIT) {
        Content::Bytes(png) => Some(png),
        _ => None,
    }
}

/// Without a converter at hand, only what is already a PNG can be shown.
#[cfg(not(target_os = "macos"))]
fn convert(_: &[u8], _: &Path, _: Option<(u32, u32)>, _: Duration) -> Option<Vec<u8>> {
    None
}

/// Runs a command to its end and says whether it succeeded. One that takes
/// too long, or outlives a request to quit, is stopped: a file made to
/// stall a converter must not stall milo.
#[cfg(target_os = "macos")]
fn finishes(mut command: Command, deadline: Duration) -> bool {
    let started = Instant::now();
    let Ok(mut child) = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < deadline && !term::TERMINATED.load(Ordering::Relaxed) => {
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
fn append_base64(data: &[u8], out: &mut Vec<u8>) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for group in data.chunks(3) {
        let bits = group.iter().fold(0u32, |bits, &byte| bits << 8 | u32::from(byte)) << (8 * (3 - group.len()));
        for place in 0..4 {
            // A group of one or two bytes fills only its first two or three places.
            let letter = if place <= group.len() { ALPHABET[(bits >> (18 - 6 * place) & 63) as usize] } else { b'=' };
            out.push(letter);
        }
    }
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
        Self { png: Self::sample_png(width, height).into(), width, height, id: 1 }
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
        let text = |data: &[u8]| {
            let mut out = Vec::new();
            append_base64(data, &mut out);
            String::from_utf8(out).unwrap()
        };
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

    #[cfg(target_os = "macos")]
    #[test]
    fn converting_small_pictures_keeps_their_size() {
        // A single grey pixel with one Huffman code in each table.
        let mut jpeg = b"\xff\xd8\xff\xdb\0\x43\0".to_vec();
        jpeg.extend([1; 64]);
        jpeg.extend_from_slice(b"\xff\xc0\0\x0b\x08\0\x01\0\x01\x01\x01\x11\0");
        for table in [0, 0x10] {
            jpeg.extend([0xff, 0xc4, 0, 0x14, table, 1]);
            jpeg.extend([0; 16]);
        }
        jpeg.extend_from_slice(b"\xff\xda\0\x08\x01\x01\0\0\x3f\0\x3f\xff\xd9");
        let gif = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;";
        for (data, name) in [(jpeg, "pixel.jpg"), (gif.to_vec(), "pixel.gif")] {
            let p = Picture::decode_with(data, Path::new(name), Duration::from_secs(120)).unwrap();
            assert_eq!((p.width, p.height), (1, 1), "{name}");
        }
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
        let picture = Picture { png: vec![7; CHUNK].into(), width: 1, height: 1, id: 9 };
        let mut out = Vec::new();
        picture.transmit(&mut out);
        let text = String::from_utf8(out).unwrap();
        let pieces: Vec<&str> = text.split_terminator("\x1b\\").collect();
        assert_eq!(pieces.len(), 2);
        assert!(pieces[0].starts_with("\x1b_Ga=t,f=100,i=9,q=2,m=1;"));
        assert!(pieces[1].starts_with("\x1b_Gm=0;"));
        let (first, second) = (pieces[0].split_once(';').unwrap().1, pieces[1].split_once(';').unwrap().1);
        assert_eq!(first.len(), CHUNK);
        let mut encoded = Vec::new();
        append_base64(&picture.png, &mut encoded);
        assert_eq!([first, second].concat().as_bytes(), encoded);
    }
}
