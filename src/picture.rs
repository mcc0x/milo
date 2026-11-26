//! Prepares checked PNG data for Kitty and, on macOS, a small RGB raster for
//! terminals that can only show pictures as colored cells.
//!
//! User input is capped at 64 MiB, 40 million pixels and 16,384 pixels per
//! side. Kitty gets a static PNG no larger than 1600 pixels per side or
//! 16 MiB; the retained RGB raster is at most 512 pixels per side. On macOS
//! the decoder and its preparation share a five-second deadline. On other
//! platforms only already-sized PNGs can be shown, and only with Kitty graphics.
//! Declared dimensions are checked before launching the decoder. While it
//! runs, a sampled 384 MiB RSS watchdog can stop it; this is not a hard or
//! aggregate memory cap.
//!
//! SVG and PDF are not handed to a document renderer, and HEIC/HEIF and PSD
//! are excluded because their containers can refer to external media.
//! Legacy icon containers are also excluded rather than handing embedded
//! format variants (including linked BMP color profiles) to the decoder.
//! GIF and classic TIFF previews accept only one image, and animated WebP
//! files are excluded from the still-image preview.

use std::borrow::Cow;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

mod headers;

#[cfg(target_os = "macos")]
mod sips;

/// Picture files larger than this are not opened.
pub const FILE_LIMIT: u64 = 64 << 20;
/// The longest side kept, in pixels, for Kitty.
const LONGEST_SIDE: u32 = 1600;
/// The largest PNG passed on to Kitty.
const PNG_LIMIT: u64 = 16 << 20;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const SOURCE_SIDE: u32 = 16_384;
const SOURCE_PIXELS: u64 = 40_000_000;
/// Bytes of encoded picture sent to the terminal at a time, as it asks.
const CHUNK: usize = 4096;

/// File endings treated as possible pictures, not as permission to decode them.
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
    #[cfg(target_os = "macos")]
    raster: Option<Raster>,
}

#[cfg(target_os = "macos")]
struct Raster {
    rgb: Vec<u8>,
    width: usize,
    height: usize,
}

struct Prepared {
    png: Option<Vec<u8>>,
    width: u32,
    height: u32,
    #[cfg(target_os = "macos")]
    raster: Raster,
}

#[derive(Debug)]
pub struct DecodeError {
    pub note: &'static str,
}

#[derive(Debug)]
enum Refusal {
    TooLarge,
    Cancelled,
    #[cfg(not(target_os = "macos"))]
    UnsupportedPlatform,
    UnsupportedFormat,
    Unreadable,
}

impl Refusal {
    fn note(self) -> &'static str {
        match self {
            Self::TooLarge => "This image exceeds the preview size limit.",
            Self::Cancelled => "This image preview was cancelled.",
            #[cfg(not(target_os = "macos"))]
            Self::UnsupportedPlatform => "Decoding this image requires macOS.",
            Self::UnsupportedFormat => "This image format is not previewed.",
            Self::Unreadable => "This image could not be decoded.",
        }
    }
}

/// One budget for parsing, preparing private copies, and decoding.
struct DecodeControl<'a> {
    cancel: &'a AtomicBool,
    deadline: Instant,
}

impl<'a> DecodeControl<'a> {
    fn new(cancel: &'a AtomicBool) -> Self {
        Self { cancel, deadline: Instant::now() + Duration::from_secs(5) }
    }

    fn check(&self) -> Result<Duration, Refusal> {
        if self.cancel.load(Ordering::Relaxed) || crate::term::TERMINATED.load(Ordering::Relaxed) {
            return Err(Refusal::Cancelled);
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(Refusal::Unreadable)
    }
}

impl Picture {
    #[cfg(test)]
    fn decode(bytes: Vec<u8>) -> Result<Self, DecodeError> {
        Self::decode_cancellable(bytes, &AtomicBool::new(false))
    }

    /// Prepares a picture, returning a preview note if it cannot be shown.
    /// Any converter sees only a private copy, never a project path.
    pub fn decode_cancellable(bytes: Vec<u8>, cancel: &AtomicBool) -> Result<Self, DecodeError> {
        let control = DecodeControl::new(cancel);
        Self::prepare(&bytes, &control)
            .and_then(|prepared| {
                Ok(Self {
                    png: file_png(prepared.png.unwrap_or(bytes), &control)?,
                    width: prepared.width,
                    height: prepared.height,
                    id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                    #[cfg(target_os = "macos")]
                    raster: Some(prepared.raster),
                })
            })
            .map_err(|reason| DecodeError { note: reason.note() })
    }

    fn prepare(bytes: &[u8], control: &DecodeControl<'_>) -> Result<Prepared, Refusal> {
        use Refusal::{TooLarge, Unreadable};
        control.check()?;
        let file_size = bytes.len() as u64;
        if file_size > FILE_LIMIT {
            return Err(TooLarge);
        }
        let format = raster_format(bytes)?;
        let size = headers::size(bytes, format, control)?;
        if size.0 == 0 || size.1 == 0 {
            return Err(Unreadable);
        }
        if !source_size(size) {
            return Err(TooLarge);
        }
        let original = if format == "png" {
            if !check_png_image_data(bytes, || control.check().map(|_| ()))? {
                return Err(Unreadable);
            }
            (size.0.max(size.1) <= LONGEST_SIDE && file_size <= PNG_LIMIT).then_some(size)
        } else {
            None
        };
        #[cfg(target_os = "macos")]
        {
            sips::decode(bytes, format, original, control)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let (width, height) = original.ok_or(Refusal::UnsupportedPlatform)?;
            Ok(Prepared { png: None, width, height })
        }
    }

    /// Top-down, tightly packed RGB24 and its own width and height. Generated
    /// and embedded Kitty pictures, and platforms without a decoder, have none.
    pub fn raster(&self) -> Option<(&[u8], usize, usize)> {
        #[cfg(target_os = "macos")]
        {
            self.raster.as_ref().map(|raster| (raster.rgb.as_slice(), raster.width, raster.height))
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// Opens a PNG built into milo without copying its bytes.
    pub fn embedded(png: &'static [u8]) -> Option<Self> {
        Self::png(png.into())
    }

    /// Opens PNG bytes, which can be borrowed or already owned.
    pub fn png(png: Cow<'static, [u8]>) -> Option<Self> {
        let (width, height) = png_size(&png)?;
        if !png_has_image_data(&png) {
            return None;
        }
        Some(Self::with_size(png, width, height))
    }

    fn with_size(png: Cow<'static, [u8]>, width: u32, height: u32) -> Self {
        Self {
            png,
            width,
            height,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            #[cfg(target_os = "macos")]
            raster: None,
        }
    }

    /// The columns and rows the picture takes in a space of `cols` by `rows`
    /// cells of `cell` pixels each: its own size where that fits, otherwise
    /// as large as fits with its proportions kept.
    pub fn fit(&self, cell: (u16, u16), cols: usize, rows: usize) -> (usize, usize) {
        let (cell_w, cell_h) = (u64::from(cell.0.max(1)), u64::from(cell.1.max(1)));
        let (w, h) = (u64::from(self.width), u64::from(self.height));
        let (room_w, room_h) = (cols as u64 * cell_w, rows as u64 * cell_h);
        // Compare as cross products, and round up so a fractional pixel
        // cannot drop the cell that contains it.
        let (shown_cols, shown_rows) = if w <= room_w && h <= room_h {
            (w.div_ceil(cell_w), h.div_ceil(cell_h))
        } else if w * room_h > h * room_w {
            (cols as u64, (h * room_w).div_ceil(w * cell_h))
        } else {
            ((w * room_h).div_ceil(h * cell_w), rows as u64)
        };
        let cells = |n: u64, most: usize| usize::try_from(n.max(1)).map_or(most, |n| n.min(most));
        (cells(shown_cols, cols.max(1)), cells(shown_rows, rows.max(1)))
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

fn source_size((width, height): (u32, u32)) -> bool {
    width != 0
        && height != 0
        && width.max(height) <= SOURCE_SIDE
        && u64::from(width) * u64::from(height) <= SOURCE_PIXELS
}

/// Identify raster containers by their magic; the string is a fixed sips
/// extension, never borrowed from the project. BMP V5 can link a color profile.
fn raster_format(bytes: &[u8]) -> Result<&'static str, Refusal> {
    let format = match bytes {
        _ if bytes.starts_with(PNG_SIGNATURE) => "png",
        [0xff, 0xd8, 0xff, _, ..] => "jpeg",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', _, _, _, _, ..] => "webp",
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', _, _, _, _, ..] => "gif",
        [b'B', b'M', ..] => {
            let header = le32(bytes, 14).ok_or(Refusal::Unreadable)?;
            match header {
                12 | 40 | 52 | 56 | 64 | 108 => "bmp",
                124 if le32(bytes, 14 + 56).ok_or(Refusal::Unreadable)? != 0x4c49_4e4b => "bmp", // PROFILE_LINKED
                _ => return Err(Refusal::UnsupportedFormat),
            }
        }
        [b'I', b'I', 42, 0, _, _, _, _, ..] | [b'M', b'M', 0, 42, _, _, _, _, ..] => "tiff",
        [b'%', b'P', b'D', b'F', b'-', ..]
        | [b'8', b'B', b'P', b'S', ..]
        | [_, _, _, _, b'f', b't', b'y', b'p', ..]
        | [b'i', b'c', b'n', b's', ..]
        | [0, 0, 1, 0, ..] => return Err(Refusal::UnsupportedFormat),
        _ => return Err(Refusal::Unreadable),
    };
    Ok(format)
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The width and height a PNG declares in its header.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let header = png.strip_prefix(PNG_SIGNATURE)?;
    let field = |at: usize| Some(u32::from_be_bytes(header.get(at..at + 4)?.try_into().ok()?));
    // The first chunk is the header: its length and name, then the two sizes.
    let (width, height) = (field(8)?, field(12)?);
    (header.get(..8)? == b"\0\0\0\rIHDR" && width > 0 && height > 0).then_some((width, height))
}

/// Checks that a file reaches IEND with image data, without decoding its pixels.
fn png_has_image_data(png: &[u8]) -> bool {
    check_png_image_data(png, || Ok(())).unwrap_or(false)
}

fn check_png_image_data(png: &[u8], check: impl Fn() -> Result<(), Refusal>) -> Result<bool, Refusal> {
    let Some(mut chunks) = png.get(33..) else { return Ok(false) }; // signature + IHDR
    let mut has_data = false;
    while let Some(([a, b, c, d, kind @ ..], body)) = chunks.split_first_chunk::<8>() {
        check()?;
        let len = u32::from_be_bytes([*a, *b, *c, *d]) as usize;
        let Some(rest) = body.get(len..).and_then(|tail| tail.get(4..)) else { return Ok(false) }; // CRC
        match kind {
            b"IDAT" => has_data |= len != 0,
            b"IEND" => return Ok(has_data && len == 0),
            _ => {}
        }
        chunks = rest;
    }
    Ok(false)
}

/// Keep only the static rendering chunks for Kitty. The caller has checked
/// each chunk's length through IEND.
fn file_png(mut png: Vec<u8>, control: &DecodeControl<'_>) -> Result<Cow<'static, [u8]>, Refusal> {
    const COPY_CHUNK: usize = 64 << 10;
    let (mut read, mut write) = (33, 33); // signature + IHDR
    loop {
        control.check()?;
        let length = u32::from_be_bytes(png[read..read + 4].try_into().expect("checked PNG chunk")) as usize;
        let kind = &png[read + 4..read + 8];
        let end = kind == b"IEND";
        let next = read + 12 + length;
        if rendering_chunk(kind) {
            if read != write {
                // Copy toward the front, so this cannot overwrite unread bytes.
                for offset in (0..next - read).step_by(COPY_CHUNK) {
                    control.check()?;
                    let start = read + offset;
                    png.copy_within(start..(start + COPY_CHUNK).min(next), write + offset);
                }
            }
            write += next - read;
        }
        if end {
            png.truncate(write);
            return Ok(png.into());
        }
        read = next;
    }
}

fn rendering_chunk(kind: &[u8]) -> bool {
    matches!(kind, b"PLTE" | b"IDAT" | b"IEND" | b"tRNS" | b"gAMA" | b"cHRM" | b"sRGB")
}

/// Make a private PNG copy without metadata or extra frames.
#[cfg(target_os = "macos")]
fn write_png_pixels(
    png: &[u8],
    control: &DecodeControl<'_>,
    mut write: impl FnMut(&[u8]) -> Result<(), Refusal>,
) -> Result<(), Refusal> {
    let (mut chunk, mut start) = (33, 0);
    loop {
        control.check()?;
        let length = u32::from_be_bytes(png[chunk..chunk + 4].try_into().expect("checked PNG chunk")) as usize;
        let kind = &png[chunk + 4..chunk + 8];
        let next = chunk + 12 + length;
        if kind == b"IEND" {
            return write(&png[start..next]);
        }
        if !rendering_chunk(kind) {
            write(&png[start..chunk])?;
            start = next;
        }
        chunk = next;
    }
}

/// Standard base64, with padding.
pub(crate) fn append_base64(data: &[u8], out: &mut Vec<u8>) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let encode = |a: u8, b: u8, c: u8| {
        [
            ALPHABET[usize::from(a >> 2)],
            ALPHABET[usize::from(((a & 3) << 4) | (b >> 4))],
            ALPHABET[usize::from(((b & 15) << 2) | (c >> 6))],
            ALPHABET[usize::from(c & 63)],
        ]
    };
    let (triples, tail) = data.as_chunks::<3>();
    for &[a, b, c] in triples {
        out.extend_from_slice(&encode(a, b, c));
    }
    if let Some((&a, rest)) = tail.split_first() {
        let mut encoded = encode(a, rest.first().copied().unwrap_or(0), 0);
        encoded[tail.len() + 1..].fill(b'=');
        out.extend_from_slice(&encoded);
    }
}

#[cfg(test)]
impl Picture {
    /// A black, one-bit grayscale PNG, with uncompressed DEFLATE blocks.
    pub fn sample_png(width: u32, height: u32) -> Vec<u8> {
        fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            png.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
            let start = png.len();
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            let mut crc = !0u32;
            for &byte in &png[start..] {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ if crc & 1 == 0 { 0 } else { 0xedb8_8320 };
                }
            }
            png.extend_from_slice(&(!crc).to_be_bytes());
        }

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = [0; 13];
        header[..4].copy_from_slice(&width.to_be_bytes());
        header[4..8].copy_from_slice(&height.to_be_bytes());
        header[8] = 1;
        chunk(&mut png, b"IHDR", &header);

        // Each scanline starts with a zero filter byte, then zero pixels.
        let mut remaining = height as usize * (1 + width.div_ceil(8) as usize);
        let adler = u32::try_from(remaining % 65521).unwrap() << 16 | 1;
        let mut data = vec![0x78, 0x01];
        while remaining > 0 {
            let len = u16::try_from(remaining.min(usize::from(u16::MAX))).unwrap();
            remaining -= usize::from(len);
            data.push(u8::from(remaining == 0));
            data.extend_from_slice(&len.to_le_bytes());
            data.extend_from_slice(&(!len).to_le_bytes());
            data.resize(data.len() + usize::from(len), 0);
        }
        data.extend_from_slice(&adler.to_be_bytes());
        chunk(&mut png, b"IDAT", &data);
        chunk(&mut png, b"IEND", &[]);
        png
    }

    pub fn sample(width: u32, height: u32) -> Self {
        Self { id: 1, ..Self::with_size(Self::sample_png(width, height).into(), width, height) }
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
        assert_eq!([text(b"foob"), text(b"fooba"), text(b"foobar")], ["Zm9vYg==", "Zm9vYmE=", "Zm9vYmFy"]);
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
    fn a_truncated_png_is_not_decoded() {
        let png = Picture::sample_png(8, 8);
        for end in [24, png.len() - 13, png.len() - 1] {
            let bytes = png[..end].to_vec();
            assert!(Picture::png(bytes.clone().into()).is_none());
            assert_eq!(Picture::decode(bytes).err().unwrap().note, "This image could not be decoded.");
        }
        assert!(Picture::decode(png).is_ok());
    }

    #[test]
    fn a_cancelled_picture_reports_cancellation() {
        let png = Picture::sample_png(8, 8);
        let cancel = AtomicBool::new(true);
        let error = Picture::decode_cancellable(png, &cancel).err().unwrap();
        assert_eq!(error.note, "This image preview was cancelled.");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_raster_decodes_png_jpeg_webp_and_gif_without_enlarging_them() {
        // Single-pixel JPEG, with one Huffman code in each table.
        let mut jpeg = b"\xff\xd8\xff\xdb\0\x43\0".to_vec();
        jpeg.extend([1; 64]);
        jpeg.extend_from_slice(b"\xff\xc0\0\x0b\x08\0\x01\0\x01\x01\x01\x11\0");
        for table in [0, 0x10] {
            jpeg.extend([0xff, 0xc4, 0, 0x14, table, 1]);
            jpeg.extend([0; 16]);
        }
        jpeg.extend_from_slice(b"\xff\xda\0\x08\x01\x01\0\0\x3f\0\x3f\xff\xd9");
        let gif = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;";
        let webp =
            b"RIFF\x22\0\0\0WEBPVP8 \x16\0\0\0\x30\x01\0\x9d\x01\x2a\x01\0\x01\0\x0e\xc0\xfe\x25\xa4\0\x03\x70\0\0\0\0";
        let png = Picture::sample_png(2, 3);
        for (data, name, size, shade) in [
            (png, "pixel.png", (2, 3), 0),
            (jpeg, "pixel.jpg", (1, 1), 128),
            (webp.to_vec(), "pixel.webp", (1, 1), 130),
            (gif.to_vec(), "pixel.gif", (1, 1), 0),
        ] {
            let picture = Picture::decode(data).unwrap();
            assert_eq!((picture.width, picture.height), size, "{name}");
            let (rgb, w, h) = picture.raster().unwrap();
            assert_eq!((w, h), (size.0 as usize, size.1 as usize), "{name}");
            assert_eq!(rgb.len(), w * h * 3, "{name}");
            assert!(rgb.iter().all(|value| value.abs_diff(shade) <= 2), "{name}: {rgb:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn converting_a_large_bmp_keeps_its_proportions_at_the_size_limit() {
        let (width, height) = (LONGEST_SIDE * 2, 40_u32);
        // BITMAPFILEHEADER and BITMAPINFOHEADER followed by black 24-bit pixels.
        // At this width every row is already aligned to four bytes.
        let pixels = width * height * 3;
        let mut bmp = vec![0; usize::try_from(54 + pixels).unwrap()];
        bmp[..2].copy_from_slice(b"BM");
        for (at, value) in [(2, 54 + pixels), (10, 54), (14, 40), (18, width), (22, height), (34, pixels)] {
            bmp[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        bmp[26..30].copy_from_slice(&[1, 0, 24, 0]); // one plane, 24 bits per pixel
        let picture = Picture::decode(bmp).unwrap();
        assert_eq!((picture.width, picture.height), (LONGEST_SIDE, height / 2));
        let (rgb, w, h) = picture.raster().unwrap();
        assert_eq!(w, 512);
        assert!(h.abs_diff(w * height as usize / width as usize) <= 1);
        assert_eq!(rgb.len(), w * h * 3);
    }

    #[test]
    fn the_kitty_payload_skips_unneeded_metadata_and_still_has_its_image_data() {
        let mut png = Picture::sample_png(1024, 1024);
        let clean = png.clone();
        // An ancillary chunk and trailing bytes are not part of the preview.
        png.splice(33..33, *b"\0\0\0\x01zTXtX\0\0\0\0");
        png.extend_from_slice(b"trailing bytes");
        assert!(png_has_image_data(&png));
        let cancel = AtomicBool::new(false);
        assert_eq!(&*file_png(png, &DecodeControl::new(&cancel)).unwrap(), clean);
    }

    #[test]
    fn vector_documents_are_not_decoded_as_rasters() {
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><image href='file:///etc/passwd'/></svg>";
        let pdf = b"%PDF-1.0\n";
        for content in [svg.as_slice(), pdf.as_slice()] {
            assert!(Picture::decode(content.to_vec()).is_err());
        }
    }

    #[test]
    fn clear_refusals_distinguish_oversize_unsupported_and_unreadable() {
        let mut oversize = Picture::sample_png(1, 1);
        oversize[16..20].copy_from_slice(&(SOURCE_SIDE + 1).to_be_bytes());
        let mut linked_bmp = vec![0_u8; 138];
        linked_bmp[..2].copy_from_slice(b"BM");
        linked_bmp[14..18].copy_from_slice(&124_u32.to_le_bytes());
        linked_bmp[70..74].copy_from_slice(&0x4c49_4e4b_u32.to_le_bytes());
        for (bytes, note) in [
            (oversize, "This image exceeds the preview size limit."),
            (
                b"\xff\xd8\xff\xc0\0\x0b\x08\x40\x01\0\x01\x01\x01\x11\0".to_vec(),
                "This image exceeds the preview size limit.",
            ),
            (
                b"RIFF\x16\0\0\0WEBPVP8 \x0a\0\0\0\0\0\0\x9d\x01\x2a\0\x20\0\x20".to_vec(),
                "This image exceeds the preview size limit.",
            ),
            (linked_bmp, "This image format is not previewed."),
            (b"%PDF-1.0\n".to_vec(), "This image format is not previewed."),
            (b"\x89PNG\r\n\x1a\n".to_vec(), "This image could not be decoded."),
        ] {
            let error = Picture::decode(bytes).err().unwrap();
            assert_eq!(error.note, note);
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn without_a_system_decoder_static_png_is_still_available_for_kitty() {
        let picture = Picture::decode(Picture::sample_png(2, 3)).unwrap();
        assert_eq!((picture.width, picture.height), (2, 3));
        assert!(picture.raster().is_none());
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
        // A fractional pixel just past a cell boundary still needs the next cell.
        assert_eq!(sized(100, 51).fit(cell, 8, 20), (8, 3));
        assert_eq!(sized(51, 100).fit(cell, 80, 4), (5, 4));
        // Never less than one cell, nor more than there is.
        assert_eq!(sized(1, 1).fit(cell, 80, 20), (1, 1));
        assert_eq!(sized(4000, 10).fit(cell, 80, 20), (80, 1));
    }

    #[test]
    fn a_picture_is_sent_in_pieces_and_only_the_last_says_so() {
        let picture = Picture { id: 9, ..Picture::with_size(vec![7; CHUNK].into(), 1, 1) };
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
