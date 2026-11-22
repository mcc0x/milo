//! Read declared raster sizes without invoking the platform image decoder.

use super::{Refusal, le32, png_size};

pub(super) fn size(bytes: &[u8], format: &str) -> Result<(u32, u32), Refusal> {
    let dimensions = match format {
        "png" => png_size(bytes),
        "jpeg" => jpeg_size(bytes),
        "webp" => return webp_size(bytes),
        "gif" => bytes.get(6..10).and_then(le16_pair),
        "bmp" => bmp_size(bytes),
        "tiff" => tiff_size(bytes),
        _ => None,
    };
    dimensions.ok_or(Refusal::Unreadable)
}

fn le16_pair(bytes: &[u8]) -> Option<(u32, u32)> {
    let [a, b, c, d, ..] = *bytes else { return None };
    Some((u32::from(u16::from_le_bytes([a, b])), u32::from(u16::from_le_bytes([c, d]))))
}

fn bmp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if le32(bytes, 14)? == 12 {
        le16_pair(bytes.get(18..)?)
    } else {
        let width = le32(bytes, 18)?;
        let height = i32::from_le_bytes(bytes.get(22..26)?.try_into().ok()?);
        Some((width, height.unsigned_abs()))
    }
}

/// Find the frame header without having to scan compressed JPEG data.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut rest = bytes.strip_prefix(b"\xff\xd8")?;
    loop {
        let mut marker = rest.strip_prefix(b"\xff")?;
        while let Some(tail) = marker.strip_prefix(b"\xff") {
            marker = tail;
        }
        let [kind, a, b, data @ ..] = marker else { return None };
        let kind = *kind;
        if matches!(kind, 0xd9 | 0xda) {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([*a, *b])).checked_sub(2)?;
        let field = data.get(..length)?;
        // Start of frame, not a Huffman or arithmetic table or JPG extension.
        if matches!(kind, 0xc0..=0xcf) && !matches!(kind, 0xc4 | 0xc8 | 0xcc) {
            let [_, h1, h2, w1, w2, components, ..] = *field else { return None };
            if components == 0 || length != 6 + 3 * usize::from(components) {
                return None;
            }
            return Some((u32::from(u16::from_be_bytes([w1, w2])), u32::from(u16::from_be_bytes([h1, h2]))));
        }
        rest = &data[length..];
    }
}

fn webp_size(bytes: &[u8]) -> Result<(u32, u32), Refusal> {
    use Refusal::{TooLarge, Unreadable};
    let riff_size = le32(bytes, 4).ok_or(Unreadable)? as usize;
    if riff_size != bytes.len().checked_sub(8).ok_or(Unreadable)? {
        return Err(Unreadable);
    }
    let mut chunks = bytes.get(12..).ok_or(Unreadable)?;
    let (kind, first) = riff_chunk(&mut chunks).ok_or(Unreadable)?;
    if kind != b"VP8X" {
        if !chunks.is_empty() {
            return Err(Unreadable);
        }
        return webp_coded_size(kind, first).ok_or(Unreadable);
    }
    let [flags, _, _, _, w0, w1, w2, h0, h1, h2] = *first else { return Err(Unreadable) };
    let size = (le24(w0, w1, w2) + 1, le24(h0, h1, h2) + 1);
    if !super::source_size(size) {
        return Err(TooLarge);
    }

    // Don't just trust the canvas: check all coded frame headers agree with
    // the dimensions and, for an animation, that every frame fits inside it.
    let animate = flags & 2 != 0;
    let mut has_image = false;
    while !chunks.is_empty() {
        let (kind, data) = riff_chunk(&mut chunks).ok_or(Unreadable)?;
        match kind {
            b"VP8 " | b"VP8L" if !animate && !has_image && webp_coded_size(kind, data) == Some(size) => {
                has_image = true;
            }
            b"ANMF" if animate => {
                webp_frame(data, size).ok_or(Unreadable)?;
                has_image = true;
            }
            b"VP8 " | b"VP8L" | b"VP8X" | b"ANMF" => return Err(Unreadable),
            _ => {}
        }
    }
    has_image.then_some(size).ok_or(Unreadable)
}

fn webp_frame(data: &[u8], canvas: (u32, u32)) -> Option<()> {
    let (header, mut chunks) = data.split_first_chunk::<16>()?;
    let [x0, x1, x2, y0, y1, y2, w0, w1, w2, h0, h1, h2, _, _, _, _] = *header;
    let size = (le24(w0, w1, w2) + 1, le24(h0, h1, h2) + 1);
    if le24(x0, x1, x2) * 2 + size.0 > canvas.0 || le24(y0, y1, y2) * 2 + size.1 > canvas.1 {
        return None;
    }
    let mut has_image = false;
    while !chunks.is_empty() {
        let (kind, data) = riff_chunk(&mut chunks)?;
        match kind {
            b"VP8 " | b"VP8L" if !has_image && webp_coded_size(kind, data) == Some(size) => has_image = true,
            b"VP8 " | b"VP8L" | b"VP8X" | b"ANMF" => return None,
            _ => {}
        }
    }
    has_image.then_some(())
}

fn riff_chunk<'a>(chunks: &mut &'a [u8]) -> Option<(&'a [u8; 4], &'a [u8])> {
    let (kind, rest) = (*chunks).split_first_chunk::<4>()?;
    let size = usize::try_from(le32(rest, 0)?).ok()?;
    let body = rest.get(4..)?;
    let data = body.get(..size)?;
    *chunks = body.get(size..)?.get(size % 2..)?;
    Some((kind, data))
}

fn webp_coded_size(kind: &[u8; 4], data: &[u8]) -> Option<(u32, u32)> {
    match (kind, data) {
        (b"VP8 ", [tag, _, _, 0x9d, 1, 0x2a, rest @ ..]) if *tag & 1 == 0 => {
            let (w, h) = le16_pair(rest)?;
            Some((w & 0x3fff, h & 0x3fff))
        }
        (b"VP8L", [0x2f, rest @ ..]) => {
            let bits = le32(rest, 0)?;
            (bits >> 29 == 0).then_some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        _ => None,
    }
}

fn le24(a: u8, b: u8, c: u8) -> u32 {
    u32::from_le_bytes([a, b, c, 0])
}

/// A classic TIFF's first IFD carries the primary image's integer dimensions.
fn tiff_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let little = bytes.starts_with(b"II");
    let short = |bytes: &[u8]| {
        let raw = bytes.first_chunk::<2>().copied()?;
        Some(if little { u16::from_le_bytes(raw) } else { u16::from_be_bytes(raw) })
    };
    let long = |bytes: &[u8]| {
        let raw = bytes.first_chunk::<4>().copied()?;
        Some(if little { u32::from_le_bytes(raw) } else { u32::from_be_bytes(raw) })
    };
    let offset = usize::try_from(long(bytes.get(4..)?)?).ok()?;
    let ifd = bytes.get(offset..)?;
    let count = usize::from(short(ifd)?);
    let mut width = None;
    let mut height = None;
    for entry in ifd.get(2..)?.get(..12 * count)?.as_chunks::<12>().0 {
        let destination = match short(entry)? {
            256 => &mut width,
            257 => &mut height,
            _ => continue,
        };
        if destination.is_some() || long(&entry[4..])? != 1 {
            return None;
        }
        *destination = Some(match short(&entry[2..])? {
            3 => u32::from(short(&entry[8..])?),
            4 => long(&entry[8..])?,
            _ => return None,
        });
    }
    Some((width?, height?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_jpeg_dimensions_come_from_the_frame_not_comment_data() {
        let jpeg = b"\xff\xd8\xff\xfe\0\x06\xff\xc0\xff\xc2\xff\xc0\0\x0b\x08\0\x0b\0\x16\x01\x01\x11\0";
        assert_eq!(jpeg_size(jpeg), Some((22, 11)));
        assert_eq!(jpeg_size(&jpeg[..jpeg.len() - 1]), None);
    }

    #[test]
    fn webp_checks_lossless_dimensions_and_the_extended_canvas() {
        let lossless = b"RIFF\x12\0\0\0WEBPVP8L\x05\0\0\0\x2f\0\x40\0\0\0";
        assert_eq!(webp_size(lossless).ok(), Some((1, 2)));
        let mut extended = *b"RIFF\x24\0\0\0WEBPVP8X\x0a\0\0\0\x10\0\0\0\0\0\0\x01\0\0VP8L\x05\0\0\0\x2f\0\x40\0\0\0";
        assert_eq!(webp_size(&extended).ok(), Some((1, 2)));
        extended[39] = 1; // coded width 2, not the declared canvas width 1
        assert!(matches!(webp_size(&extended), Err(Refusal::Unreadable)));
    }

    #[test]
    fn animated_webp_rejects_outlying_or_mismatched_frames() {
        let mut frame = *b"\0\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0VP8L\x05\0\0\0\x2f\0\x40\0\0\0";
        assert_eq!(webp_frame(&frame, (8, 8)), Some(()));
        frame[0] = 4; // x = 8, leaving no room for the frame
        assert_eq!(webp_frame(&frame, (8, 8)), None);
        frame[0] = 0;
        frame[9] = 2; // frame height = 3, coded image is only 2
        assert_eq!(webp_frame(&frame, (8, 8)), None);
    }
}
