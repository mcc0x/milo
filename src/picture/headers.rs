//! Read declared raster sizes without invoking the platform image decoder.

use super::{DecodeControl, Refusal, le32, png_size, source_size};

pub(super) fn size(bytes: &[u8], format: &str, control: &DecodeControl<'_>) -> Result<(u32, u32), Refusal> {
    let dimensions = match format {
        "png" => png_size(bytes),
        "jpeg" => return jpeg_size(bytes, control),
        "webp" => return webp_size(bytes, control),
        "gif" => return gif_size(bytes, control),
        "bmp" => bmp_size(bytes),
        "tiff" => return tiff_size(bytes, control),
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

/// A single image descriptor must fit inside the logical screen. Verify
/// the complete block sequence, not just the first frame or canvas header.
fn gif_size(bytes: &[u8], control: &DecodeControl<'_>) -> Result<(u32, u32), Refusal> {
    use Refusal::{TooLarge, Unreadable, UnsupportedFormat};
    let size = le16_pair(bytes.get(6..10).ok_or(Unreadable)?).ok_or(Unreadable)?;
    if size.0 == 0 || size.1 == 0 {
        return Err(Unreadable);
    }
    if !source_size(size) {
        return Err(TooLarge);
    }
    let [flags, _, _, rest @ ..] = bytes.get(10..).ok_or(Unreadable)? else { return Err(Unreadable) };
    let table_len = |flags: u8| if flags & 0x80 == 0 { 0 } else { 3 << (1 + (flags & 7)) };
    let global = table_len(*flags);
    let mut rest = rest.get(global..).ok_or(Unreadable)?;
    let mut has_image = false;
    loop {
        control.check()?;
        match rest {
            [b';'] if has_image => return Ok(size),
            [b',', ..] if has_image => return Err(UnsupportedFormat),
            [b',', rectangle @ ..] => {
                let (x, y) = le16_pair(rectangle).ok_or(Unreadable)?;
                let (width, height) = le16_pair(rectangle.get(4..).ok_or(Unreadable)?).ok_or(Unreadable)?;
                if width == 0 || height == 0 || x + width > size.0 || y + height > size.1 {
                    return Err(Unreadable);
                }
                let [flags, data @ ..] = rectangle.get(8..).ok_or(Unreadable)? else { return Err(Unreadable) };
                let local = table_len(*flags);
                if global == 0 && local == 0 {
                    return Err(Unreadable);
                }
                let [2..=8, blocks @ ..] = data.get(local..).ok_or(Unreadable)? else { return Err(Unreadable) };
                if matches!(blocks.first(), None | Some(0)) {
                    return Err(Unreadable);
                }
                rest = gif_sub_blocks(blocks, control)?;
                has_image = true;
            }
            [b'!', 0xf9, 4, _, _, _, _, 0, tail @ ..] => rest = tail, // graphic control
            [b'!', 0xfe, blocks @ ..] => rest = gif_sub_blocks(blocks, control)?, // comment
            [b'!', 0xff, 11, application @ ..] => {
                rest = gif_sub_blocks(application.get(11..).ok_or(Unreadable)?, control)?;
            }
            // Plain-text extensions are rendering blocks, not just metadata.
            [b'!', 0x01, ..] => return Err(UnsupportedFormat),
            _ => return Err(Unreadable),
        }
    }
}

fn gif_sub_blocks<'a>(mut blocks: &'a [u8], control: &DecodeControl<'_>) -> Result<&'a [u8], Refusal> {
    loop {
        control.check()?;
        let (&length, tail) = blocks.split_first().ok_or(Refusal::Unreadable)?;
        if length == 0 {
            return Ok(tail);
        }
        blocks = tail.get(usize::from(length)..).ok_or(Refusal::Unreadable)?;
    }
}

/// Find the frame header without having to scan compressed JPEG data.
fn jpeg_size(bytes: &[u8], control: &DecodeControl<'_>) -> Result<(u32, u32), Refusal> {
    use Refusal::Unreadable;
    let mut rest = bytes.strip_prefix(b"\xff\xd8").ok_or(Unreadable)?;
    loop {
        control.check()?;
        let mut marker = rest.strip_prefix(b"\xff").ok_or(Unreadable)?;
        while marker.first() == Some(&0xff) {
            control.check()?;
            let prefix = &marker[..marker.len().min(64 * 1024)];
            marker = &marker[prefix.iter().take_while(|&&b| b == 0xff).count()..];
        }
        let [kind, a, b, data @ ..] = marker else { return Err(Unreadable) };
        let kind = *kind;
        if matches!(kind, 0xd9 | 0xda) {
            return Err(Unreadable);
        }
        let length = usize::from(u16::from_be_bytes([*a, *b])).checked_sub(2).ok_or(Unreadable)?;
        let field = data.get(..length).ok_or(Unreadable)?;
        // Start of frame, not a Huffman or arithmetic table or JPG extension.
        if matches!(kind, 0xc0..=0xcf) && !matches!(kind, 0xc4 | 0xc8 | 0xcc) {
            let [_, h1, h2, w1, w2, components, ..] = *field else { return Err(Unreadable) };
            if components == 0 || length != 6 + 3 * usize::from(components) {
                return Err(Unreadable);
            }
            return Ok((u32::from(u16::from_be_bytes([w1, w2])), u32::from(u16::from_be_bytes([h1, h2]))));
        }
        rest = &data[length..];
    }
}

fn webp_size(bytes: &[u8], control: &DecodeControl<'_>) -> Result<(u32, u32), Refusal> {
    use Refusal::{TooLarge, Unreadable, UnsupportedFormat};
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
    if !source_size(size) {
        return Err(TooLarge);
    }

    if flags & 2 != 0 {
        return Err(UnsupportedFormat);
    }

    // A static extended file must contain exactly one coded image whose
    // dimensions match the canvas. Undeclared animation chunks also fail.
    let mut has_image = false;
    while !chunks.is_empty() {
        control.check()?;
        let (kind, data) = riff_chunk(&mut chunks).ok_or(Unreadable)?;
        match kind {
            b"VP8 " | b"VP8L" if !has_image && webp_coded_size(kind, data) == Some(size) => {
                has_image = true;
            }
            b"VP8 " | b"VP8L" | b"VP8X" | b"ANIM" | b"ANMF" => return Err(Unreadable),
            _ => {}
        }
    }
    has_image.then_some(size).ok_or(Unreadable)
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

/// Accept only a classic TIFF with one IFD and no nested directory pointers.
fn tiff_size(bytes: &[u8], control: &DecodeControl<'_>) -> Result<(u32, u32), Refusal> {
    use Refusal::{Unreadable, UnsupportedFormat};
    let little = bytes.starts_with(b"II");
    let short = |bytes: &[u8]| -> Result<u16, Refusal> {
        let raw = bytes.first_chunk::<2>().copied().ok_or(Unreadable)?;
        Ok(if little { u16::from_le_bytes(raw) } else { u16::from_be_bytes(raw) })
    };
    let long = |bytes: &[u8]| -> Result<u32, Refusal> {
        let raw = bytes.first_chunk::<4>().copied().ok_or(Unreadable)?;
        Ok(if little { u32::from_le_bytes(raw) } else { u32::from_be_bytes(raw) })
    };
    let offset = long(bytes.get(4..).ok_or(Unreadable)?)? as usize;
    if offset < 8 || !offset.is_multiple_of(2) {
        return Err(Unreadable);
    }
    let ifd = bytes.get(offset..).ok_or(Unreadable)?;
    let count = usize::from(short(ifd)?);
    let (entries, next) = ifd[2..].split_at_checked(12 * count).ok_or(Unreadable)?;
    if long(next)? != 0 {
        return Err(UnsupportedFormat);
    }
    let mut width = None;
    let mut height = None;
    for entry in entries.as_chunks::<12>().0 {
        control.check()?;
        let tag = short(entry)?;
        let kind = short(&entry[2..])?;
        // SubIFDs, global parameters, Exif, GPS, and interoperability IFDs
        // may be stored as plain LONG offsets; 13 and 18 are IFD field types.
        if matches!(tag, 330 | 400 | 34665 | 34853 | 40965) || matches!(kind, 13 | 18) {
            return Err(UnsupportedFormat);
        }
        let destination = match tag {
            256 => &mut width,
            257 => &mut height,
            _ => continue,
        };
        if destination.is_some() || long(&entry[4..])? != 1 {
            return Err(Unreadable);
        }
        *destination = Some(match kind {
            3 => u32::from(short(&entry[8..])?),
            4 => long(&entry[8..])?,
            _ => return Err(Unreadable),
        });
    }
    width.zip(height).ok_or(Unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn control() -> DecodeControl<'static> {
        static RUNNING: AtomicBool = AtomicBool::new(false);
        DecodeControl::new(&RUNNING)
    }

    #[test]
    fn delivered_jpeg_dimensions_come_from_the_frame_not_comment_data() {
        let jpeg = b"\xff\xd8\xff\xfe\0\x06\xff\xc0\xff\xc2\xff\xc0\0\x0b\x08\0\x0b\0\x16\x01\x01\x11\0";
        assert_eq!(jpeg_size(jpeg, &control()).unwrap(), (22, 11));
        assert!(matches!(jpeg_size(&jpeg[..jpeg.len() - 1], &control()), Err(Refusal::Unreadable)));
    }

    #[test]
    fn webp_checks_lossless_dimensions_and_the_extended_canvas() {
        let lossless = b"RIFF\x12\0\0\0WEBPVP8L\x05\0\0\0\x2f\0\x40\0\0\0";
        let control = control();
        assert_eq!(webp_size(lossless, &control).unwrap(), (1, 2));
        let mut extended = *b"RIFF\x24\0\0\0WEBPVP8X\x0a\0\0\0\x10\0\0\0\0\0\0\x01\0\0VP8L\x05\0\0\0\x2f\0\x40\0\0\0";
        assert_eq!(webp_size(&extended, &control).unwrap(), (1, 2));
        extended[39] = 1; // coded width 2, not the declared canvas width 1
        assert!(matches!(webp_size(&extended, &control), Err(Refusal::Unreadable)));
    }

    #[test]
    fn webp_rejects_declared_and_undeclared_animation() {
        let control = control();
        let timing = b"ANIM\x06\0\0\0\0\0\0\0\0\0";
        let mut rectangle = [0; 16];
        rectangle[9] = 1; // a 1-by-2 frame at the origin
        let frame = [b"ANMF\x1e\0\0\0".as_slice(), &rectangle, b"VP8L\x05\0\0\0\x2f\0\x40\0\0\0"].concat();
        let canvas = b"RIFF\x4a\0\0\0WEBPVP8X\x0a\0\0\0\x02\0\0\0\x07\0\0\x07\0\0";
        let animated = [canvas.as_slice(), timing, &frame].concat();
        assert!(matches!(webp_size(&animated, &control), Err(Refusal::UnsupportedFormat)));

        let still = b"RIFF\x24\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0\0\0\0\x01\0\0VP8L\x05\0\0\0\x2f\0\x40\0\0\0";
        for animation_chunk in [timing.as_slice(), &frame] {
            let mut undeclared = still.to_vec();
            undeclared.extend_from_slice(animation_chunk);
            let len = u32::try_from(undeclared.len() - 8).unwrap();
            undeclared[4..8].copy_from_slice(&len.to_le_bytes());
            assert!(matches!(webp_size(&undeclared, &control), Err(Refusal::Unreadable)));
        }
    }

    #[test]
    fn gif_requires_one_image_rectangle_inside_the_canvas() {
        let control = control();
        let screen = b"GIF89a\x03\0\x02\0\x80\0\0\0\0\0\xff\xff\xff!\xfe\x02,;\0"; // palette and comment
        let frame = b",\x02\0\x01\0\x01\0\x01\0\0\x02\x02D\x01\0";
        let mut gif = [screen.as_slice(), frame, b";"].concat();
        assert_eq!(gif_size(&gif, &control).unwrap(), (3, 2));
        for (field, outside) in [(1, 3), (3, 2), (5, 2), (7, 2)] {
            gif[screen.len() + field] = outside;
            assert!(matches!(gif_size(&gif, &control), Err(Refusal::Unreadable)));
            gif[screen.len() + field] = frame[field];
        }

        gif.pop();
        gif.extend_from_slice(frame);
        gif.push(b';');
        assert!(matches!(gif_size(&gif, &control), Err(Refusal::UnsupportedFormat)));
    }

    #[test]
    fn tiff_requires_one_complete_image_directory() {
        let control = control();
        let single = b"II\x2a\0\x08\0\0\0\x02\0\
            \0\x01\x04\0\x01\0\0\0\x02\0\0\0\
            \x01\x01\x03\0\x01\0\0\0\x03\0\0\0\0\0\0\0";
        assert_eq!(tiff_size(single, &control).unwrap(), (2, 3));
        assert!(matches!(tiff_size(&single[..single.len() - 1], &control), Err(Refusal::Unreadable)));

        let mut multipage = single.to_vec();
        multipage[34..38].copy_from_slice(&38_u32.to_le_bytes());
        multipage.extend_from_slice(&single[8..]);
        assert!(matches!(tiff_size(&multipage, &control), Err(Refusal::UnsupportedFormat)));

        let mut nested = single[..34].to_vec();
        nested[8] = 3;
        nested.extend_from_slice(b"\x4a\x01\x04\0\x01\0\0\0\x32\0\0\0\0\0\0\0");
        nested.extend_from_slice(&single[8..]);
        assert!(matches!(tiff_size(&nested, &control), Err(Refusal::UnsupportedFormat)));
    }
}
