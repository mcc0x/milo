//! Decodes a still picture into a small RGB raster with the Windows Imaging
//! Component, in a separate copy of Milo.
//!
//! Milo hands the copy the checked bytes over a pipe, never a path, so the
//! decoder neither reads nor writes the project. The copy has no console and
//! runs in a job that ends it with Milo, holds it to 384 MiB of committed
//! memory and keeps it from starting any other program; it refuses to decode
//! outside such a job, and lowers itself to low integrity first, so Windows
//! refuses it write access to the user's files. Only the decoders Windows registers for its own formats
//! are created, by their fixed class ids, so an installed codec cannot claim
//! the file. Windows itself has no WebP decoder, so WebP is refused.

use std::ffi::{OsString, c_void};
use std::io::{self, Read as _};
use std::ptr::{NonNull, null, null_mut};
use std::sync::atomic::AtomicBool;

use super::headers::gif_layout;
use super::{DecodeControl, FILE_LIMIT, RASTER_SIDE, Raster, Refusal, over_gray, source_size, write_png_pixels};

/// The internal command that runs the decoder. Not for people to run.
pub const DECODER_COMMAND: &str = "picture-decoder";
const MEMORY_LIMIT: usize = 384 << 20;
/// The decoder's answer: the raster's width and height, then its top-down BGRA rows.
const HEADER: usize = 8;
const OUTPUT_LIMIT: usize = HEADER + 4 * RASTER_SIDE as usize * RASTER_SIDE as usize;
const REFUSED: i32 = 2;
const TOO_LARGE: i32 = 3;

pub(super) fn decode(
    bytes: &[u8],
    format: &str,
    size: (u32, u32),
    control: &DecodeControl<'_>,
) -> Result<Raster, Refusal> {
    decoder_class(format)?;
    let mut pixels = Vec::new();
    let input = if format == "png" {
        // Like the Kitty copy, the decoder sees only the chunks that draw the picture.
        write_png_pixels(bytes, control, |data| {
            pixels.extend_from_slice(data);
            Ok(())
        })?;
        &pixels
    } else {
        bytes
    };
    let (status, output) = run(input, format, size, control)?;
    match status {
        0 => raster(&output, scaled(size)).ok_or(Refusal::Unreadable),
        TOO_LARGE => Err(Refusal::TooLarge),
        _ => Err(Refusal::Unreadable),
    }
}

/// Starts this program as the decoder, gives it the picture, and waits for its answer.
#[cfg(not(test))]
fn run(bytes: &[u8], format: &str, size: (u32, u32), control: &DecodeControl<'_>) -> Result<(i32, Vec<u8>), Refusal> {
    use Refusal::Unreadable;
    use std::io::Write as _;
    use std::os::windows::process::CommandExt as _;
    use std::process::{Command, Stdio};
    const IO_CHUNK: usize = 64 << 10;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

    control.check()?;
    let program = std::env::current_exe().map_err(|_| Unreadable)?;
    let mut child = Command::new(&program)
        .arg(DECODER_COMMAND)
        .args([format.to_owned(), size.0.to_string(), size.1.to_string()])
        .current_dir(program.parent().ok_or(Unreadable)?)
        .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Unreadable)?;
    // The decoder reads all of its input before it checks for this job, and the
    // input is written only once the job holds it.
    let job = crate::sys::Job::confine(&child, MEMORY_LIMIT);
    let (Ok(job), Some(mut input), Some(output)) = (job, child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Unreadable);
    };
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for chunk in bytes.chunks(IO_CHUNK) {
                if input.write_all(chunk).is_err() {
                    break;
                }
            }
        });
        let answer = scope.spawn(move || {
            let mut answer = Vec::new();
            output.take(OUTPUT_LIMIT as u64 + 1).read_to_end(&mut answer).map(|_| answer)
        });
        let status = wait(&mut child, control);
        // Ending the job closes the pipes of anything left, so both threads finish.
        drop(job);
        let answer = answer.join().map_err(|_| Unreadable)?.map_err(|_| Unreadable)?;
        Ok((status?, answer))
    })
}

#[cfg(not(test))]
fn wait(child: &mut std::process::Child, control: &DecodeControl<'_>) -> Result<i32, Refusal> {
    let error = loop {
        let remaining = match control.check() {
            Ok(remaining) => remaining,
            Err(reason) => break reason,
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                control.check()?;
                return status.code().ok_or(Refusal::Unreadable);
            }
            Ok(None) => std::thread::sleep(remaining.min(std::time::Duration::from_millis(5))),
            Err(_) => break Refusal::Unreadable,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    Err(error)
}

/// The test binary cannot be started as the decoder, so unit tests run the
/// decoder's own steps in the test process and read the same answer.
#[cfg(test)]
fn run(bytes: &[u8], format: &str, size: (u32, u32), control: &DecodeControl<'_>) -> Result<(i32, Vec<u8>), Refusal> {
    control.check()?;
    let mut answer = Vec::new();
    let status = respond(&mut bytes.to_vec(), format, size, &mut answer);
    Ok((status, answer))
}

/// Runs the decoder: the picture arrives on stdin, and its raster leaves on stdout.
pub fn command(args: &[OsString]) -> i32 {
    let number = |arg: &OsString| arg.to_str()?.parse::<u32>().ok();
    let [format, width, height] = args else { return REFUSED };
    let (Some(format), Some(width), Some(height)) = (format.to_str(), number(width), number(height)) else {
        return REFUSED;
    };
    let mut bytes = Vec::new();
    let read = io::stdin().lock().take(FILE_LIMIT + 1).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > FILE_LIMIT || !crate::sys::Job::confined(MEMORY_LIMIT) {
        return REFUSED;
    }
    if crate::sys::lower_integrity().is_err() {
        return REFUSED;
    }
    respond(&mut bytes, format, (width, height), &mut io::stdout().lock())
}

fn respond(bytes: &mut [u8], format: &str, size: (u32, u32), out: &mut impl io::Write) -> i32 {
    match pixels(bytes, format, size) {
        Ok(((width, height), bgra)) => {
            let header = [width.to_le_bytes(), height.to_le_bytes()].concat();
            let written = out.write_all(&header).and_then(|()| out.write_all(&bgra)).and_then(|()| out.flush());
            if written.is_ok() { 0 } else { REFUSED }
        }
        Err(Refusal::TooLarge) => TOO_LARGE,
        Err(_) => REFUSED,
    }
}

/// Reads the decoder's answer: the expected size, then exactly that many pixels.
fn raster(answer: &[u8], expected: (u32, u32)) -> Option<Raster> {
    let (header, pixels) = answer.split_first_chunk::<HEADER>()?;
    let (width, height) = header.split_at(4);
    let size = (u32::from_le_bytes(width.try_into().ok()?), u32::from_le_bytes(height.try_into().ok()?));
    let (width, height) = (size.0 as usize, size.1 as usize);
    let (bgra, rest) = pixels.as_chunks::<4>();
    if size != expected || !rest.is_empty() || bgra.len() != width * height {
        return None;
    }
    let rgb = bgra.iter().flat_map(|&[blue, green, red, alpha]| over_gray([red, green, blue], alpha)).collect();
    Some(Raster { rgb, width, height })
}

/// The raster's size: the picture's own, or reduced until its longer side fits.
fn scaled((width, height): (u32, u32)) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= RASTER_SIDE {
        return (width, height);
    }
    let side = |n: u32| {
        let reduced = (u64::from(n) * u64::from(RASTER_SIDE) + u64::from(longest) / 2) / u64::from(longest);
        u32::try_from(reduced).expect("no longer than the raster").max(1)
    };
    (side(width), side(height))
}

fn decoder_class(format: &str) -> Result<&'static Guid, Refusal> {
    match format {
        "png" => Ok(&PNG_DECODER),
        "jpeg" => Ok(&JPEG_DECODER),
        "gif" => Ok(&GIF_DECODER),
        "bmp" => Ok(&BMP_DECODER),
        "tiff" => Ok(&TIFF_DECODER),
        "webp" => Err(Refusal::UnsupportedPlatform),
        _ => Err(Refusal::UnsupportedFormat),
    }
}

/// Decodes the one image in `bytes`, which must be `size` pixels, to BGRA no larger than the raster.
/// The bytes are the decoder's own, so a codec that wrote to them would harm nothing else.
fn pixels(bytes: &mut [u8], format: &str, size: (u32, u32)) -> Result<((u32, u32), Vec<u8>), Refusal> {
    use Refusal::{TooLarge, Unreadable};
    if !source_size(size) {
        return Err(TooLarge);
    }
    let class = decoder_class(format)?;
    // A GIF's one image can sit anywhere on its canvas; Windows decodes only the image.
    let [left, top, width, height] = if format == "gif" {
        let cancel = AtomicBool::new(false);
        let (canvas, image) = gif_layout(bytes, &DecodeControl::new(&cancel))?;
        if canvas != size {
            return Err(Unreadable);
        }
        image
    } else {
        [0, 0, size.0, size.1]
    };
    let length = u32::try_from(bytes.len()).map_err(|_| TooLarge)?;
    // SAFETY: The reserved argument is null; a repeated call on this thread only adds a count.
    if unsafe { CoInitializeEx(null_mut(), COINIT_MULTITHREADED | COINIT_DISABLE_OLE1DDE) } < 0 {
        return Err(Unreadable);
    }
    // Made before every object below, so it is dropped after all of them.
    let _apartment = Apartment;
    // SAFETY: The imaging factory class is created for IWICImagingFactory, whose table is `Factory`.
    let factory = unsafe { create::<Factory>(&IMAGING_FACTORY, &FACTORY_INTERFACE) }?;
    // SAFETY: The class is a Windows decoder, created for IWICBitmapDecoder, whose table is `Decoder`.
    let decoder = unsafe { create::<Decoder>(class, &DECODER_INTERFACE) }?;

    let mut raw = null_mut();
    // SAFETY: The factory is live and the output is writable.
    let result = unsafe { (factory.table().create_stream)(factory.raw(), &raw mut raw) };
    // SAFETY: CreateStream hands over an IWICStream.
    let stream = unsafe { Com::<Stream>::adopt(result, raw) }?;
    // SAFETY: The bytes are borrowed exclusively for longer than the stream and every object that reads it.
    check(unsafe { (stream.table().initialize_from_memory)(stream.raw(), bytes.as_mut_ptr(), length) })?;
    // SAFETY: Both objects are live; the decoder reads metadata only if asked, and it is not.
    check(unsafe { (decoder.table().initialize)(decoder.raw(), stream.raw(), DECODE_METADATA_ON_DEMAND) })?;
    let mut frames = 0;
    // SAFETY: The decoder is initialized and the count is writable.
    check(unsafe { (decoder.table().get_frame_count)(decoder.raw(), &raw mut frames) })?;
    if frames != 1 {
        return Err(Unreadable);
    }
    let mut raw = null_mut();
    // SAFETY: The decoder has this frame and the output is writable.
    let result = unsafe { (decoder.table().get_frame)(decoder.raw(), 0, &raw mut raw) };
    // SAFETY: GetFrame hands over an IWICBitmapFrameDecode, whose table begins with `Source`.
    let frame = unsafe { Com::<Source>::adopt(result, raw) }?;
    let (mut decoded_width, mut decoded_height) = (0, 0);
    // SAFETY: The frame is live and both outputs are writable.
    check(unsafe { (frame.table().get_size)(frame.raw(), &raw mut decoded_width, &raw mut decoded_height) })?;
    if (decoded_width, decoded_height) != (width, height) {
        return Err(Unreadable);
    }

    let mut raw = null_mut();
    // SAFETY: The factory is live and the output is writable.
    let result = unsafe { (factory.table().create_format_converter)(factory.raw(), &raw mut raw) };
    // SAFETY: CreateFormatConverter hands over an IWICFormatConverter.
    let converter = unsafe { Com::<Converter>::adopt(result, raw) }?;
    // SAFETY: Both objects are live, the format id outlives the call, and no palette is needed.
    check(unsafe {
        (converter.table().initialize)(converter.raw(), frame.raw(), &raw const BGRA, 0, null_mut(), 0.0, 0)
    })?;

    // The raster covers the canvas; the image keeps its place and share of it.
    let (columns, rows) = scaled(size);
    let share = |n: u32, whole: u32, room: u32| {
        let shared = (u64::from(n) * u64::from(room) + u64::from(whole) / 2) / u64::from(whole);
        u32::try_from(shared).expect("no longer than the raster")
    };
    let (x, y) = (share(left, size.0, columns).min(columns - 1), share(top, size.1, rows).min(rows - 1));
    let shown = (share(width, size.0, columns).clamp(1, columns - x), share(height, size.1, rows).clamp(1, rows - y));
    let image = if shown == (width, height) {
        copy_bgra(&converter, shown)?
    } else {
        let mut raw = null_mut();
        // SAFETY: The factory is live and the output is writable.
        let result = unsafe { (factory.table().create_bitmap_scaler)(factory.raw(), &raw mut raw) };
        // SAFETY: CreateBitmapScaler hands over an IWICBitmapScaler.
        let scaler = unsafe { Com::<Scaler>::adopt(result, raw) }?;
        // SAFETY: Both objects are live.
        check(unsafe {
            (scaler.table().initialize)(scaler.raw(), converter.raw(), shown.0, shown.1, INTERPOLATE_FANT)
        })?;
        copy_bgra(&scaler, shown)?
    };
    if shown == (columns, rows) {
        return Ok(((columns, rows), image));
    }
    // Uncovered canvas is transparent, as a GIF's background is when shown.
    let mut canvas = vec![0; 4 * columns as usize * rows as usize];
    for (row, line) in image.chunks_exact(4 * shown.0 as usize).enumerate() {
        let at = 4 * ((y as usize + row) * columns as usize + x as usize);
        canvas[at..at + line.len()].copy_from_slice(line);
    }
    Ok(((columns, rows), canvas))
}

/// A converter or scaler, whose table begins with `Source`.
trait Bitmap {
    fn source(&self) -> &Source;
}

impl Bitmap for Converter {
    fn source(&self) -> &Source {
        &self.source
    }
}

impl Bitmap for Scaler {
    fn source(&self) -> &Source {
        &self.source
    }
}

/// Copies all of a bitmap of `size` pixels, which must give straight BGRA.
fn copy_bgra<T: Bitmap>(bitmap: &Com<T>, (columns, rows): (u32, u32)) -> Result<Vec<u8>, Refusal> {
    let table = bitmap.table().source();
    let mut format = guid(0, 0, 0, [0; 8]);
    // SAFETY: The bitmap is live and the format output is writable.
    check(unsafe { (table.get_pixel_format)(bitmap.raw(), &raw mut format) })?;
    if format != BGRA {
        return Err(Refusal::Unreadable);
    }
    let mut bgra = vec![0; 4 * columns as usize * rows as usize];
    let total = u32::try_from(bgra.len()).expect("a raster fits in a u32");
    // SAFETY: The bitmap is live, a null rectangle is all of it, and the buffer holds `rows` rows
    // of `4 * columns` bytes.
    check(unsafe { (table.copy_pixels)(bitmap.raw(), null(), 4 * columns, total, bgra.as_mut_ptr()) })?;
    Ok(bgra)
}

/// This thread's use of COM, ended once every object made in it is released.
struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: Balances the successful CoInitializeEx made just before this value.
        unsafe { CoUninitialize() };
    }
}

fn check(result: i32) -> Result<(), Refusal> {
    if result < 0 { Err(Refusal::Unreadable) } else { Ok(()) }
}

/// Creates a Windows class for the interface whose method table the caller names as `T`.
///
/// # Safety
///
/// `T` must be the table of `interface`.
unsafe fn create<T>(class: &Guid, interface: &Guid) -> Result<Com<T>, Refusal> {
    let mut raw = null_mut();
    // SAFETY: Both ids are valid GUIDs, no outer object is given, and the output is writable.
    let result = unsafe { CoCreateInstance(class, null_mut(), CLSCTX_INPROC_SERVER, interface, &raw mut raw) };
    // SAFETY: On success the object implements `interface`, whose table the caller vouched is `T`.
    unsafe { Com::adopt(result, raw) }
}

/// One counted reference to a COM object whose method table begins with `T`.
struct Com<T>(NonNull<*const T>);

impl<T> Com<T> {
    /// # Safety
    ///
    /// On success, `raw` must be one reference to an object whose method table begins with `T`.
    unsafe fn adopt(result: i32, raw: *mut c_void) -> Result<Self, Refusal> {
        NonNull::new(raw.cast()).filter(|_| result >= 0).map(Self).ok_or(Refusal::Unreadable)
    }

    fn raw(&self) -> *mut c_void {
        self.0.as_ptr().cast()
    }

    fn table(&self) -> &T {
        // SAFETY: A COM object begins with a pointer to its method table, which `adopt` was told is a `T`.
        let table = unsafe { self.0.as_ptr().read() };
        // SAFETY: The table lasts as long as the object, which this reference keeps alive.
        unsafe { &*table }
    }
}

impl<T> Drop for Com<T> {
    fn drop(&mut self) {
        // SAFETY: Every method table in this file begins with IUnknown's, as every COM table does.
        let table = unsafe { self.0.as_ptr().cast::<*const Unknown>().read() };
        // SAFETY: The table lasts as long as the object.
        let release = unsafe { (*table).release };
        // SAFETY: This value owns one reference and gives it back once.
        unsafe { release(self.raw()) };
    }
}

#[repr(C)]
#[derive(PartialEq, Eq)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const fn guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Guid {
    Guid { data1, data2, data3, data4 }
}

static IMAGING_FACTORY: Guid = guid(0xcaca_f262, 0x9370, 0x4615, [0xa1, 0x3b, 0x9f, 0x55, 0x39, 0xda, 0x4c, 0x0a]);
static FACTORY_INTERFACE: Guid = guid(0xec5e_c8a9, 0xc395, 0x4314, [0x9c, 0x77, 0x54, 0xd7, 0xa9, 0x35, 0xff, 0x70]);
static DECODER_INTERFACE: Guid = guid(0x9edd_e9e7, 0x8dee, 0x47ea, [0x99, 0xdf, 0xe6, 0xfa, 0xf2, 0xed, 0x44, 0xbf]);
static PNG_DECODER: Guid = guid(0xe018_945b, 0xaa86, 0x4008, [0x9b, 0xd4, 0x67, 0x77, 0xa1, 0xe4, 0x0c, 0x11]);
static JPEG_DECODER: Guid = guid(0x9456_a480, 0xe88b, 0x43ea, [0x9e, 0x73, 0x0b, 0x2d, 0x9b, 0x71, 0xb1, 0xca]);
static GIF_DECODER: Guid = guid(0x381d_da3c, 0x9ce9, 0x4834, [0xa2, 0x3e, 0x1f, 0x98, 0xf8, 0xfc, 0x52, 0xbe]);
static BMP_DECODER: Guid = guid(0x6b46_2062, 0x7cbf, 0x400d, [0x9f, 0xdb, 0x81, 0x3d, 0xd1, 0x0f, 0x27, 0x78]);
static TIFF_DECODER: Guid = guid(0xb54e_85d9, 0xfe23, 0x499f, [0x8b, 0x88, 0x6a, 0xce, 0xa7, 0x13, 0x75, 0x2b]);
/// GUID_WICPixelFormat32bppBGRA, with straight rather than premultiplied alpha.
static BGRA: Guid = guid(0x6fdd_c324, 0x4e03, 0x4bfe, [0xb1, 0x85, 0x3d, 0x77, 0x76, 0x8d, 0xc9, 0x0f]);

const COINIT_MULTITHREADED: u32 = 0;
const COINIT_DISABLE_OLE1DDE: u32 = 4;
const CLSCTX_INPROC_SERVER: u32 = 1;
const DECODE_METADATA_ON_DEMAND: u32 = 0;
const INTERPOLATE_FANT: u32 = 3;

type Hresult = i32;
type Make = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult;
/// A method Milo does not call.
type Unused = usize;

#[repr(C)]
struct Unknown {
    _query_interface: Unused,
    _add_ref: Unused,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

/// IWICImagingFactory.
#[repr(C)]
struct Factory {
    unknown: Unknown,
    _decoders_to_palette: [Unused; 7],
    create_format_converter: Make,
    create_bitmap_scaler: Make,
    _clipper_and_rotator: [Unused; 2],
    create_stream: Make,
}

/// IWICStream, after IStream's methods and two other ways to initialize it.
#[repr(C)]
struct Stream {
    unknown: Unknown,
    _stream_to_filename: [Unused; 13],
    initialize_from_memory: unsafe extern "system" fn(*mut c_void, *mut u8, u32) -> Hresult,
}

/// IWICBitmapDecoder.
#[repr(C)]
struct Decoder {
    unknown: Unknown,
    _query_capability: Unused,
    initialize: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> Hresult,
    _container_to_thumbnail: [Unused; 7],
    get_frame_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> Hresult,
    get_frame: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> Hresult,
}

/// IWICBitmapSource, which frames, converters and scalers begin with.
#[repr(C)]
struct Source {
    unknown: Unknown,
    get_size: unsafe extern "system" fn(*mut c_void, *mut u32, *mut u32) -> Hresult,
    get_pixel_format: unsafe extern "system" fn(*mut c_void, *mut Guid) -> Hresult,
    _resolution_and_palette: [Unused; 2],
    copy_pixels: unsafe extern "system" fn(*mut c_void, *const c_void, u32, u32, *mut u8) -> Hresult,
}

/// IWICFormatConverter.
#[repr(C)]
struct Converter {
    source: Source,
    initialize: unsafe extern "system" fn(*mut c_void, *mut c_void, *const Guid, u32, *mut c_void, f64, u32) -> Hresult,
}

/// IWICBitmapScaler.
#[repr(C)]
struct Scaler {
    source: Source,
    initialize: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32) -> Hresult,
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> Hresult;
    fn CoUninitialize();
    fn CoCreateInstance(
        class: *const Guid,
        outer: *mut c_void,
        context: u32,
        interface: *const Guid,
        object: *mut *mut c_void,
    ) -> Hresult;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture::Picture;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn large_pictures_shrink_to_the_raster_and_keep_their_shape() {
        assert_eq!(scaled((64, 32)), (64, 32));
        assert_eq!(scaled((512, 512)), (512, 512));
        assert_eq!(scaled((3200, 40)), (512, 6));
        assert_eq!(scaled((40, 16_384)), (1, 512));
    }

    #[test]
    fn an_answer_must_hold_exactly_the_pixels_of_the_expected_size() {
        let mut answer = [2_u32.to_le_bytes(), 1_u32.to_le_bytes()].concat();
        answer.extend([0, 0, 255, 255, 0, 255, 0, 0]);
        assert_eq!(raster(&answer, (2, 1)).unwrap().rgb, [255, 0, 0, 128, 128, 128]);
        assert!(raster(&answer, (1, 2)).is_none());
        assert!(raster(&answer[..answer.len() - 1], (2, 1)).is_none());
        answer.extend([0; 4]);
        assert!(raster(&answer, (2, 1)).is_none());
    }

    #[test]
    fn the_decoder_refuses_a_picture_whose_size_is_not_the_one_checked() {
        let png = Picture::sample_png(4, 2);
        assert!(pixels(&mut png.clone(), "png", (4, 2)).is_ok());
        assert!(matches!(pixels(&mut png.clone(), "png", (2, 4)), Err(Refusal::Unreadable)));
        assert!(matches!(pixels(&mut png.clone(), "png", (16_385, 1)), Err(Refusal::TooLarge)));
        assert!(matches!(pixels(&mut png.clone(), "jpeg", (4, 2)), Err(Refusal::Unreadable)));
        assert!(matches!(pixels(&mut png.clone(), "webp", (4, 2)), Err(Refusal::UnsupportedPlatform)));
    }

    #[test]
    fn an_impossible_header_is_unreadable_and_damaged_data_stays_within_its_checked_size() {
        let cancel = AtomicBool::new(false);
        let control = DecodeControl::new(&cancel);
        let mut png = Picture::sample_png(8, 8);
        png[24] = 3; // a bit depth PNG does not have
        assert!(matches!(decode(&png, "png", (8, 8), &control), Err(Refusal::Unreadable)));
        // Windows draws what it can of data that cannot be inflated, like a truncated download.
        let mut png = Picture::sample_png(8, 8);
        let data = png.windows(4).position(|kind| kind == b"IDAT").unwrap() + 4;
        png[data + 2] = 0b111; // the first block claims the reserved type
        let raster = decode(&png, "png", (8, 8), &control).unwrap();
        assert_eq!((raster.width, raster.height, raster.rgb.len()), (8, 8, 8 * 8 * 3));
    }

    #[test]
    fn colors_come_through_bmp_and_tiff_exactly() {
        let mut bmp = vec![0_u8; 62];
        bmp[..2].copy_from_slice(b"BM");
        for (at, value) in [(2, 62_u32), (10, 54), (14, 40), (18, 2), (22, 1), (34, 8)] {
            bmp[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        bmp[26..30].copy_from_slice(&[1, 0, 24, 0]);
        bmp[54..60].copy_from_slice(&[0, 0, 255, 255, 0, 0]); // red, then blue
        let mut tiff = b"II\x2a\0\x08\0\0\0\x0a\0".to_vec();
        let short = |tag: u16, count: u32, value: u32| (tag, 3_u16, count, value);
        for (tag, kind, count, value) in [
            short(256, 1, 2),
            short(257, 1, 1),
            short(258, 3, 134),
            short(259, 1, 1),
            short(262, 1, 2),
            (273, 4, 1, 140),
            short(277, 1, 3),
            short(278, 1, 1),
            (279, 4, 1, 6),
            short(284, 1, 1),
        ] {
            tiff.extend([tag.to_le_bytes(), kind.to_le_bytes()].concat());
            tiff.extend([count.to_le_bytes(), value.to_le_bytes()].concat());
        }
        tiff.extend([0, 0, 0, 0, 8, 0, 8, 0, 8, 0, 255, 0, 0, 0, 0, 255]);
        for (data, name) in [(bmp, "bmp"), (tiff, "tiff")] {
            let picture = Picture::decode(data).unwrap();
            assert_eq!(picture.raster().unwrap(), (&[255, 0, 0, 0, 0, 255][..], 2, 1), "{name}");
        }
    }

    #[test]
    fn a_gif_image_keeps_its_place_on_the_canvas() {
        let screen = b"GIF89a\x03\0\x02\0\x80\0\0\0\0\0\xff\xff\xff!\xfe\x02,;\0";
        let image = b",\x02\0\x01\0\x01\0\x01\0\0\x02\x02D\x01\0";
        let picture = Picture::decode([screen.as_slice(), image, b";"].concat()).unwrap();
        let (rgb, width, height) = picture.raster().unwrap();
        assert_eq!((width, height), (3, 2));
        // Black in the lower right; the rest of the canvas is transparent, drawn gray.
        assert_eq!(rgb[..15], [128; 15]);
        assert_eq!(rgb[15..], [0, 0, 0]);
    }
}
