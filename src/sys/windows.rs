//! Windows forms of the facilities in [`super`]: handle values, readiness checks and file identity.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::time::{Duration, Instant};

/// A handle's value. -1 is `INVALID_HANDLE_VALUE`, which a wait skips like a negative descriptor.
pub(crate) type RawFd = isize;

/// The raw value of anything that owns or borrows a handle, under the Unix name.
pub(crate) trait AsRawFd {
    fn as_raw_fd(&self) -> RawFd;
}

impl<T: AsRawHandle + ?Sized> AsRawFd for T {
    fn as_raw_fd(&self) -> RawFd {
        self.as_raw_handle() as RawFd
    }
}

/// The record a wait fills, shaped like the Unix one.
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct pollfd {
    pub fd: RawFd,
    pub events: i16,
    pub revents: i16,
}

pub(crate) const POLLIN: i16 = 0x1;
pub(crate) const POLLOUT: i16 = 0x4;
pub(crate) const POLLHUP: i16 = 0x10;

const FILE_TYPE_DISK: u32 = 1;
const FILE_TYPE_CHAR: u32 = 2;
const FILE_TYPE_PIPE: u32 = 3;
const ERROR_BROKEN_PIPE: i32 = 109;
const PIPE_NOWAIT: u32 = 1;
const WAIT_OBJECT_0: u32 = 0;
/// How long a wait sleeps between checks of handles that cannot be waited on.
const CHECK_INTERVAL: Duration = Duration::from_millis(8);

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}

#[repr(C)]
#[derive(Default)]
struct ByHandleFileInformation {
    attributes: u32,
    created: FileTime,
    accessed: FileTime,
    written: FileTime,
    volume_serial: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFileType(handle: *mut c_void) -> u32;
    fn PeekNamedPipe(
        handle: *mut c_void,
        buffer: *mut c_void,
        size: u32,
        read: *mut u32,
        available: *mut u32,
        left: *mut u32,
    ) -> i32;
    fn GetNumberOfConsoleInputEvents(handle: *mut c_void, count: *mut u32) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn SetNamedPipeHandleState(handle: *mut c_void, mode: *const u32, count: *const u32, timeout: *const u32) -> i32;
    fn WriteFile(
        handle: *mut c_void,
        buffer: *const c_void,
        size: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn GetFileInformationByHandle(handle: *mut c_void, info: *mut ByHandleFileInformation) -> i32;
}

fn handle(fd: RawFd) -> *mut c_void {
    fd as *mut c_void
}

/// What a pipe holds for reading.
pub(super) enum Available {
    Bytes(u32),
    Closed,
    /// Not a pipe.
    Unknown,
}

pub(super) fn available(fd: RawFd) -> Available {
    let mut bytes = 0;
    // SAFETY: Only the count output is requested, and it is writable; a handle that is not a pipe fails the call.
    let peeked = unsafe {
        PeekNamedPipe(handle(fd), std::ptr::null_mut(), 0, std::ptr::null_mut(), &raw mut bytes, std::ptr::null_mut())
    };
    if peeked != 0 {
        Available::Bytes(bytes)
    } else if io::Error::last_os_error().raw_os_error() == Some(ERROR_BROKEN_PIPE) {
        Available::Closed
    } else {
        Available::Unknown
    }
}

/// What a handle is ready for now, out of `events`.
fn readiness(fd: RawFd, events: i16) -> i16 {
    // Writes to pipes and the console are not made to fail early, so a writer is never held back here.
    let mut ready = events & POLLOUT;
    if events & POLLIN == 0 {
        return ready;
    }
    // SAFETY: The call only inspects the handle and fails for one that is not open.
    let kind = unsafe { GetFileType(handle(fd)) };
    ready |= match kind {
        FILE_TYPE_PIPE => match available(fd) {
            Available::Bytes(0) => 0,
            Available::Bytes(_) => POLLIN,
            Available::Closed => POLLIN | POLLHUP,
            Available::Unknown => POLLHUP,
        },
        FILE_TYPE_CHAR => {
            let mut count = 0;
            // SAFETY: The count output is writable; a character device that is not a console fails the call.
            if unsafe { GetNumberOfConsoleInputEvents(handle(fd), &raw mut count) } == 0 || count > 0 {
                POLLIN
            } else {
                0
            }
        }
        FILE_TYPE_DISK => POLLIN,
        // Events, processes and threads report whether they are signalled.
        // SAFETY: A zero timeout only checks the object's state.
        _ if unsafe { WaitForSingleObject(handle(fd), 0) } == WAIT_OBJECT_0 => POLLIN,
        _ => 0,
    };
    ready
}

pub(super) fn poll(fds: &mut [pollfd], timeout_ms: i32) -> i32 {
    let deadline = u64::try_from(timeout_ms).ok().map(|ms| Instant::now() + Duration::from_millis(ms));
    loop {
        let mut count = 0;
        for fd in fds.iter_mut() {
            fd.revents = if fd.fd < 0 { 0 } else { readiness(fd.fd, fd.events) };
            if fd.revents != 0 {
                count += 1;
            }
        }
        let now = Instant::now();
        let left = deadline.map(|deadline| deadline.saturating_duration_since(now));
        if count > 0 || left.is_some_and(|left| left.is_zero()) {
            return count;
        }
        std::thread::sleep(left.map_or(CHECK_INTERVAL, |left| left.min(CHECK_INTERVAL)));
    }
}

pub(super) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let mode = PIPE_NOWAIT;
    // SAFETY: The mode lives for the call; the count and timeout are left as they are.
    let set = unsafe { SetNamedPipeHandleState(handle(fd), &raw const mode, std::ptr::null(), std::ptr::null()) };
    if set != 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

pub(super) fn write_byte(fd: RawFd) {
    let mut written = 0;
    // SAFETY: The byte and the count live for the call, and the write is not overlapped.
    unsafe { WriteFile(handle(fd), [1u8].as_ptr().cast(), 1, &raw mut written, std::ptr::null_mut()) };
}

pub(super) fn identity(fd: RawFd) -> io::Result<(u64, u64)> {
    let mut info = ByHandleFileInformation::default();
    // SAFETY: The record is writable and sized for this call.
    if unsafe { GetFileInformationByHandle(handle(fd), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(info.volume_serial), u64::from(info.index_high) << 32 | u64::from(info.index_low)))
}
