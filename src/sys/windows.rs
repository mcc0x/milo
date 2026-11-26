//! Windows forms of the facilities in [`super`]: handle values, readiness checks and file identity.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle as _, OwnedHandle};
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

/// A borrowed handle, under the Unix name.
pub(crate) type BorrowedFd<'a> = BorrowedHandle<'a>;

/// Borrows the handle of anything that owns one, under the Unix name.
pub(crate) trait AsFd {
    fn as_fd(&self) -> BorrowedFd<'_>;
}

impl<T: AsHandle + ?Sized> AsFd for T {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.as_handle()
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

#[repr(C)]
#[derive(Default)]
struct ExtendedLimits {
    per_process_user_time: i64,
    per_job_user_time: i64,
    flags: u32,
    minimum_working_set: usize,
    maximum_working_set: usize,
    active_processes: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
    io_counters: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}

const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
    fn SetInformationJobObject(job: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    fn TerminateJobObject(job: *mut c_void, code: u32) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn GetExitCodeProcess(process: *mut c_void, code: *mut u32) -> i32;
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const STILL_ACTIVE: u32 = 259;
const ERROR_ACCESS_DENIED: i32 = 5;

pub(super) fn process_alive(pid: u32) -> bool {
    // SAFETY: Opening only queries the process and the result is checked before use.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED);
    }
    // SAFETY: The handle was just opened for this call and is closed with the value.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut code = 0;
    // SAFETY: The handle is open with query access and the code output is writable.
    unsafe { GetExitCodeProcess(process.as_raw_handle(), &raw mut code) != 0 && code == STILL_ACTIVE }
}

/// A child and every program it starts once it is assigned here, ended together on `kill` or drop.
/// It stands in for a Unix process group. A program the child starts before `Job::contain`
/// returns is not included, so contain a child as soon as it is spawned.
pub(crate) struct Job(OwnedHandle);

impl Job {
    pub(crate) fn contain(child: &std::process::Child) -> io::Result<Self> {
        // SAFETY: Null asks for default security and no name; the result is checked before use.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: A new job handle is owned by this call and closed with the value.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let limits = ExtendedLimits { flags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, ..ExtendedLimits::default() };
        let size = u32::try_from(size_of::<ExtendedLimits>()).expect("job limits fit in a u32");
        // SAFETY: The limits record lives for the call and `size` is its exact length.
        let set = unsafe {
            SetInformationJobObject(
                job.handle(),
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&raw const limits).cast(),
                size,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Both handles are open; the child has not been waited for, so its handle is valid.
        if unsafe { AssignProcessToJobObject(job.handle(), child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn handle(&self) -> *mut c_void {
        self.0.as_raw_handle()
    }

    pub(crate) fn kill(&self) {
        // SAFETY: The job handle is open; ending its processes is the purpose of the call.
        unsafe { TerminateJobObject(self.handle(), 1) };
    }
}
