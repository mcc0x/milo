//! Runs the system raster decoder against private, size-limited copies.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{
    LONGEST_SIDE, PNG_LIMIT, Prepared, Raster, Refusal, le32, png_has_image_data, png_size, source_size,
    write_png_pixels,
};
use crate::fs_ops;
use crate::read::{self, Content};
use crate::term;

const DEADLINE: Duration = Duration::from_secs(5);
/// The terminal can't use every pixel in the Kitty PNG as a colored cell.
const RASTER_SIDE: u32 = 512;
const BMP_LIMIT: u64 = 4 * RASTER_SIDE as u64 * RASTER_SIDE as u64 + 4096;

enum Output {
    Png,
    Bmp,
}

impl Output {
    fn properties(self) -> (&'static str, u32, u64) {
        match self {
            Self::Png => ("png", LONGEST_SIDE, PNG_LIMIT),
            Self::Bmp => ("bmp", RASTER_SIDE, BMP_LIMIT),
        }
    }
}

pub(super) fn decode(bytes: &[u8], format: &str, original: Option<(u32, u32)>) -> Result<Prepared, Refusal> {
    use Refusal::{TooLarge, Unreadable};
    let deadline = Instant::now() + DEADLINE;
    let scratch = Scratch::new().ok_or(Unreadable)?;
    let input = scratch.0.join("in").with_extension(format);
    if format == "png" {
        let mut file = BufWriter::new(File::create(&input).map_err(|_| Unreadable)?);
        write_png_pixels(bytes, &mut file).and_then(|()| file.flush()).map_err(|_| Unreadable)?;
    } else {
        fs::write(&input, bytes).map_err(|_| Unreadable)?;
    }

    // Probe before any full decode, and make sure magic and ImageIO agree.
    let size = probe(&scratch, &input, format, deadline).ok_or(Unreadable)?;
    if !source_size(size) {
        return Err(TooLarge);
    }
    let png_path = scratch.0.join("out.png");
    let (png, source, (width, height)) = if let Some(expected) = original {
        if expected != size {
            return Err(Unreadable);
        }
        (None, &input, size)
    } else {
        convert(&scratch, &input, &png_path, Output::Png, size.0.max(size.1), deadline).ok_or(Unreadable)?;
        let Content::Bytes(png) = read::file(&png_path, PNG_LIMIT) else { return Err(Unreadable) };
        let size =
            png_size(&png).filter(|&(w, h)| w.max(h) <= LONGEST_SIDE && png_has_image_data(&png)).ok_or(Unreadable)?;
        (Some(png), &png_path, size)
    };
    let bmp_path = scratch.0.join("out.bmp");
    convert(&scratch, source, &bmp_path, Output::Bmp, width.max(height), deadline).ok_or(Unreadable)?;
    let Content::Bytes(bmp) = read::file(&bmp_path, BMP_LIMIT) else { return Err(Unreadable) };
    Ok(Prepared { png, width, height, raster: bitmap(&bmp).ok_or(Unreadable)? })
}

fn probe(scratch: &Scratch, source: &Path, format: &str, deadline: Instant) -> Option<(u32, u32)> {
    let mut command = scratch.command();
    command.args(["-g", "format", "-g", "pixelWidth", "-g", "pixelHeight"]).arg(source);
    let mut stdout = finish(command, Stdio::piped(), deadline)?.stdout?;
    // The child has exited. Read at most what is already in the pipe, even if
    // an inherited writer is still open. Normal output is a path and 3 lines.
    let mut len: libc::c_int = 0;
    // SAFETY: The pipe is live; FIONREAD writes one c_int to len.
    if unsafe { libc::ioctl(stdout.as_raw_fd(), libc::FIONREAD, &raw mut len) } != 0 {
        return None;
    }
    let mut buffer = [0; 2048];
    let properties = buffer.get_mut(..usize::try_from(len).ok()?)?;
    stdout.read_exact(properties).ok()?;
    dimensions(properties, format)
}

fn dimensions(properties: &[u8], format: &str) -> Option<(u32, u32)> {
    let mut lines = properties.trim_ascii_end().rsplit(|&byte| byte == b'\n');
    let value = |line: &[u8], key: &[u8]| {
        let text = line.trim_ascii_start().strip_prefix(key)?.trim_ascii();
        std::str::from_utf8(text).ok()?.parse::<u32>().ok()
    };
    let height = value(lines.next()?, b"pixelHeight:")?;
    let width = value(lines.next()?, b"pixelWidth:")?;
    let actual_format = lines.next()?.trim_ascii_start().strip_prefix(b"format:")?.trim_ascii();
    (actual_format == format.as_bytes() && width != 0 && height != 0).then_some((width, height))
}

fn convert(
    scratch: &Scratch,
    source: &Path,
    output: &Path,
    kind: Output,
    longest: u32,
    deadline: Instant,
) -> Option<()> {
    let (format, max_side, limit) = kind.properties();
    let mut command = scratch.command();
    if longest > max_side {
        // `-Z` can enlarge small pictures; only request a reduction.
        command.args(["-Z", &max_side.to_string()]);
    }
    command.args(["-s", "format", format]).arg(source).arg("--out").arg(output);
    // SAFETY: In the post-fork child, this only calls setrlimit and reads
    // errno on failure. It applies the limit before sips can write.
    unsafe { command.pre_exec(move || limit_output_size(limit)) };
    finish(command, Stdio::null(), deadline).map(|_| ())
}

fn limit_output_size(bytes: u64) -> io::Result<()> {
    let limit = libc::rlimit { rlim_cur: bytes, rlim_max: bytes };
    // SAFETY: setrlimit reads one initialized rlimit and retains no pointer.
    if unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &raw const limit) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Option<Self> {
        let mut name = *b"/private/tmp/milo-picture.XXXXXX\0";
        // SAFETY: mkdtemp takes a writable NUL-terminated template and keeps
        // no reference to it. It creates the unique directory with mode 0700.
        if unsafe { libc::mkdtemp(name.as_mut_ptr().cast()) }.is_null() {
            return None;
        }
        let path = std::str::from_utf8(&name[..name.len() - 1]).ok()?;
        Some(Self(PathBuf::from(path)))
    }

    fn command(&self) -> Command {
        let mut command = Command::new("/usr/bin/sips");
        command.env_clear().env("LC_ALL", "C").env("TMPDIR", &self.0).current_dir(&self.0);
        command
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn finish(mut command: Command, stdout: Stdio, deadline: Instant) -> Option<Child> {
    if Instant::now() >= deadline || term::TERMINATED.load(Ordering::Relaxed) {
        return None;
    }
    let mut child = command.stdin(Stdio::null()).stdout(stdout).stderr(Stdio::null()).spawn().ok()?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || term::TERMINATED.load(Ordering::Relaxed) {
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => return status.success().then_some(child),
            Ok(None) if fs_ops::over_memory_budget(&child) => break,
            Ok(None) => std::thread::sleep(remaining.min(Duration::from_millis(5))),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// sips writes 24-bit BGR or 32-bit BGRA bitfields, usually with top-down rows.
fn bitmap(bytes: &[u8]) -> Option<Raster> {
    if !bytes.starts_with(b"BM") || bytes.get(26..28)? != [1, 0] {
        return None;
    }
    let offset = usize::try_from(le32(bytes, 10)?).ok()?;
    let header = le32(bytes, 14)?;
    if header < 40 || usize::try_from(header).ok()?.checked_add(14)? > offset {
        return None;
    }
    let width = le32(bytes, 18)?;
    let signed_height = i32::from_le_bytes(bytes.get(22..26)?.try_into().ok()?);
    let height = signed_height.unsigned_abs();
    if width == 0 || height == 0 || width.max(height) > RASTER_SIDE {
        return None;
    }
    let (pixel_size, alpha) = match (bytes.get(28..30)?, le32(bytes, 30)?) {
        ([24, 0], 0) => (3, false),
        ([32, 0], 0) => (4, false),
        ([32, 0], 3) if header >= 56 => {
            if [le32(bytes, 54)?, le32(bytes, 58)?, le32(bytes, 62)?] != [0xff0000, 0xff00, 0xff] {
                return None;
            }
            match le32(bytes, 66)? {
                0 => (4, false),
                0xff00_0000 => (4, true),
                _ => return None,
            }
        }
        _ => return None,
    };
    let (width, height) = (width as usize, height as usize);
    let row_pixels = width * pixel_size;
    let stride = row_pixels.next_multiple_of(4);
    let pixels = bytes.get(offset..)?.get(..stride * height)?;
    let mut rgb = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        let y = if signed_height < 0 { row } else { height - 1 - row };
        for pixel in pixels[y * stride..y * stride + row_pixels].chunks_exact(pixel_size) {
            let [blue, green, red, ..] = pixel else { unreachable!("validated pixel size") };
            let mut color = [*red, *green, *blue];
            if alpha && pixel[3] != 255 {
                // RGB24 has no alpha. Neutral gray is legible against terminals
                // with either a light or a dark background.
                let opacity = u16::from(pixel[3]);
                color =
                    color.map(|c| u8::try_from((u16::from(c) * opacity + 128 * (255 - opacity) + 127) / 255).unwrap());
            }
            rgb.extend_from_slice(&color);
        }
    }
    Some(Raster { rgb, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_require_the_expected_raster_format_and_usable_dimensions() {
        let props = b"/path\n  format: webp\n  pixelWidth: 25\n  pixelHeight: 50\n";
        assert_eq!(dimensions(props, "webp"), Some((25, 50)));
        assert_eq!(dimensions(props, "png"), None);
        assert_eq!(dimensions(b"  format: webp\n  pixelWidth: <nil>\n  pixelHeight: 50\n", "webp"), None);
        assert!(source_size((8000, 5000)));
        assert!(!source_size((8000, 5001)));
        assert!(!source_size((16_385, 1)));
    }

    #[test]
    fn bitmap_rows_are_reordered_and_padding_does_not_become_a_pixel() {
        let mut bmp = [0_u8; 62];
        bmp[..2].copy_from_slice(b"BM");
        bmp[10] = 54;
        bmp[14] = 40;
        bmp[18] = 1;
        bmp[22] = 2;
        bmp[26] = 1;
        bmp[28] = 24;
        bmp[54..].copy_from_slice(&[255, 0, 0, 42, 0, 0, 255, 43]); // blue below red
        let raster = bitmap(&bmp).unwrap();
        assert_eq!((raster.width, raster.height), (1, 2));
        assert_eq!(raster.rgb, [255, 0, 0, 0, 0, 255]);
        assert!(bitmap(&bmp[..61]).is_none());
        bmp[22..26].copy_from_slice(&(-2_i32).to_le_bytes());
        assert_eq!(bitmap(&bmp).unwrap().rgb, [0, 0, 255, 255, 0, 0]);
        bmp[22..26].copy_from_slice(&i32::MIN.to_le_bytes());
        assert!(bitmap(&bmp).is_none());
    }

    #[test]
    fn bitmap_alpha_is_composited_before_exposing_rgb24() {
        let mut bmp = [0_u8; 146];
        bmp[..2].copy_from_slice(b"BM");
        bmp[10] = 138;
        bmp[14] = 124;
        bmp[18] = 2;
        bmp[22..26].copy_from_slice(&(-1_i32).to_le_bytes());
        bmp[26] = 1;
        bmp[28] = 32;
        bmp[30] = 3;
        bmp[54..70].copy_from_slice(b"\0\0\xff\0\0\xff\0\0\xff\0\0\0\0\0\0\xff");
        bmp[138..].copy_from_slice(&[0, 0, 255, 128, 0, 255, 0, 0]);
        assert_eq!(bitmap(&bmp).unwrap().rgb, [192, 64, 64, 128, 128, 128]);
    }
}
