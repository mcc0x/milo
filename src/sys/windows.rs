//! Windows forms of the facilities in [`super`]: handle values, readiness checks and file identity.

use std::ffi::{OsString, c_void};
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::MetadataExt as _;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
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
    // SAFETY: The call only inspects the handle and fails for one that is not open.
    let kind = unsafe { GetFileType(handle(fd)) };
    // Windows reports a socket as a pipe, but only Winsock can say what a socket is ready for.
    if kind == FILE_TYPE_PIPE && is_socket(fd) {
        return socket_readiness(fd, events);
    }
    // Writes to pipes and the console are not made to fail early, so a writer is never held back here.
    let mut ready = events & POLLOUT;
    if events & POLLIN == 0 {
        return ready;
    }
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

#[repr(C)]
struct WsaPollFd {
    socket: usize,
    events: i16,
    revents: i16,
}

const SOL_SOCKET: i32 = 0xffff;
const SO_TYPE: i32 = 0x1008;
const WSA_POLLRDNORM: i16 = 0x100;
const WSA_POLLWRNORM: i16 = 0x10;
const WSA_POLLERR: i16 = 0x1;
const WSA_POLLHUP: i16 = 0x2;
const WSA_POLLNVAL: i16 = 0x4;

#[link(name = "ws2_32")]
unsafe extern "system" {
    fn getsockopt(socket: usize, level: i32, name: i32, value: *mut c_void, length: *mut i32) -> i32;
    fn WSAPoll(fds: *mut WsaPollFd, count: u32, timeout: i32) -> i32;
}

fn is_socket(fd: RawFd) -> bool {
    let (mut kind, mut length) = (0i32, 4i32);
    // SAFETY: Both outputs are writable and `length` is the size of `kind`; a handle that is not a socket fails the call.
    unsafe { getsockopt(fd.cast_unsigned(), SOL_SOCKET, SO_TYPE, (&raw mut kind).cast(), &raw mut length) == 0 }
}

/// A socket's readiness in the Unix form: a closed or failed socket is ready for whatever was
/// asked, so that the read or write that follows reports it.
fn socket_readiness(fd: RawFd, events: i16) -> i16 {
    let wanted =
        if events & POLLIN != 0 { WSA_POLLRDNORM } else { 0 } | if events & POLLOUT != 0 { WSA_POLLWRNORM } else { 0 };
    let mut record = WsaPollFd { socket: fd.cast_unsigned(), events: wanted, revents: 0 };
    // SAFETY: One writable record is passed, and a zero timeout only checks the socket.
    if unsafe { WSAPoll(&raw mut record, 1, 0) } < 0 {
        return POLLHUP | (events & (POLLIN | POLLOUT));
    }
    let mut ready = 0;
    if record.revents & WSA_POLLRDNORM != 0 {
        ready |= POLLIN;
    }
    if record.revents & WSA_POLLWRNORM != 0 {
        ready |= POLLOUT;
    }
    if record.revents & (WSA_POLLERR | WSA_POLLHUP | WSA_POLLNVAL) != 0 {
        ready |= POLLHUP | (events & (POLLIN | POLLOUT));
    }
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

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptGenRandom(algorithm: *mut c_void, buffer: *mut u8, size: u32, flags: u32) -> i32;
}

const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 2;

/// Fills `buffer` from the system's cryptographic random number generator.
pub(crate) fn random(buffer: &mut [u8]) -> io::Result<()> {
    let size = u32::try_from(buffer.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: The buffer is writable for `size` bytes, and this flag needs no algorithm handle.
    let status =
        unsafe { BCryptGenRandom(std::ptr::null_mut(), buffer.as_mut_ptr(), size, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status != 0 {
        return Err(io::Error::other(format!("the system random number generator failed ({status:#x})")));
    }
    Ok(())
}

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}

const TOKEN_QUERY: u32 = 0x8;
const TOKEN_USER: i32 = 1;
const SDDL_REVISION_1: u32 = 1;
const OWNER_SECURITY_INFORMATION: u32 = 0x1;
const DACL_SECURITY_INFORMATION: u32 = 0x4;
const SE_FILE_OBJECT: i32 = 1;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ_WRITE: u32 = 0x3;
const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x7;
const CREATE_NEW: u32 = 1;
const OPEN_EXISTING: u32 = 3;
const OPEN_ALWAYS: u32 = 4;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const ERROR_ALREADY_EXISTS: i32 = 183;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(token: *mut c_void, class: i32, info: *mut c_void, size: u32, needed: *mut u32) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
        descriptor: *mut c_void,
        revision: u32,
        information: u32,
        text: *mut *mut u16,
        length: *mut u32,
    ) -> i32;
    fn GetSecurityInfo(
        handle: *mut c_void,
        kind: i32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    safe fn GetCurrentProcess() -> *mut c_void;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const SecurityAttributes,
        disposition: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn CreateDirectoryW(name: *const u16, security: *const SecurityAttributes) -> i32;
    fn GetFinalPathNameByHandleW(file: *mut c_void, path: *mut u16, len: u32, flags: u32) -> u32;
}

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// FOLDERID_LocalAppData.
static LOCAL_APP_DATA: Guid =
    Guid { data1: 0xF1B3_2785, data2: 0x6FBA, data3: 0x4FCF, data4: [0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91] };

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHGetKnownFolderPath(id: *const Guid, flags: u32, token: *mut c_void, path: *mut *mut u16) -> i32;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(memory: *mut c_void);
}

/// The user's local application data folder as the system records it for this account, not as the
/// environment names it, so nothing another program set can move it.
pub(crate) fn local_app_data() -> io::Result<PathBuf> {
    let mut path = std::ptr::null_mut();
    // SAFETY: The id is a valid GUID, no token is passed, and the path output is writable.
    let result = unsafe { SHGetKnownFolderPath(&raw const LOCAL_APP_DATA, 0, std::ptr::null_mut(), &raw mut path) };
    let text = (result >= 0 && !path.is_null()).then(|| {
        let mut length = 0;
        // SAFETY: The system wrote a NUL-terminated path here, and every unit up to the NUL is read.
        while unsafe { path.wrapping_add(length).read() } != 0 {
            length += 1;
        }
        // SAFETY: The `length` units before the NUL were just read from this allocation.
        PathBuf::from(OsString::from_wide(unsafe { std::slice::from_raw_parts(path, length) }))
    });
    // SAFETY: The memory, or null, came from SHGetKnownFolderPath and is freed only here.
    unsafe { CoTaskMemFree(path.cast()) };
    text.ok_or_else(|| io::Error::other(format!("the local application data folder is unknown ({result:#x})")))
}

/// The path the system resolves an open file or folder to now, through any link or junction.
pub(crate) fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; 32_768];
    let len = u32::try_from(buffer.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: The handle is open, and the buffer holds `len` units.
    let written = unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), buffer.as_mut_ptr(), len, 0) };
    let written = usize::try_from(written).map_err(|_| io::ErrorKind::InvalidData)?;
    if written == 0 {
        return Err(io::Error::last_os_error());
    }
    let units = buffer.get(..written).ok_or(io::ErrorKind::InvalidData)?;
    Ok(PathBuf::from(OsString::from_wide(units)))
}

/// Whether an open file is directly inside an open folder, as both are resolved now.
pub(crate) fn inside(file: &File, folder: &File) -> io::Result<bool> {
    Ok(final_path(file)?.parent() == Some(final_path(folder)?.as_path()))
}

/// Memory the system allocated for a result, freed with this value.
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: The memory came from a call that allocates with LocalAlloc, and is freed only here.
        unsafe { LocalFree(self.0) };
    }
}

impl Local {
    /// The NUL-terminated UTF-16 text the system wrote here.
    fn text(&self) -> String {
        let start = self.0.cast::<u16>();
        let mut length = 0;
        // SAFETY: Every unit up to and including the terminating NUL is part of the allocation.
        while unsafe { start.wrapping_add(length).read() } != 0 {
            length += 1;
        }
        // SAFETY: The `length` units before the NUL were just read from this allocation.
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(start, length) })
    }
}

/// This process's user as a SID string, such as S-1-5-21-…-1001.
fn user_sid() -> io::Result<String> {
    let mut token = std::ptr::null_mut();
    // SAFETY: This process's pseudo handle needs no closing, and the token output is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The token was just opened for this call and is closed with the value.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    // A TOKEN_USER record: a pointer to the user's SID and its attributes, then the SID itself.
    let mut record = [0usize; 64];
    let size = u32::try_from(size_of_val(&record)).expect("the token record fits in a u32");
    let mut needed = 0;
    // SAFETY: The record is writable, aligned for its pointer and `size` bytes long.
    let read = unsafe {
        GetTokenInformation(token.as_raw_handle(), TOKEN_USER, record.as_mut_ptr().cast(), size, &raw mut needed)
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut text = std::ptr::null_mut();
    // SAFETY: The record's first field points at the SID stored later in the record, which is alive.
    if unsafe { ConvertSidToStringSidW(record[0] as *mut c_void, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Local(text.cast()).text())
}

/// This user owns a private file, and only this user and SYSTEM may open it. The protected
/// list takes nothing from the folder, whoever else the folder lets in.
fn private_descriptor() -> io::Result<Local> {
    let user = user_sid()?;
    let text: Vec<u16> = format!("O:{user}D:P(A;;FA;;;{user})(A;;FA;;;SY)").encode_utf16().chain([0]).collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: The text is NUL-terminated and the descriptor output is writable; the size is not asked for.
    let made = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if made == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Local(descriptor))
}

/// A descriptor's owner and access list in the system's text form.
fn security_text(descriptor: &Local) -> io::Result<String> {
    let mut text = std::ptr::null_mut();
    // SAFETY: The descriptor is valid and the text output is writable; the length is not asked for.
    let written = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor.0,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut text,
            std::ptr::null_mut(),
        )
    };
    if written == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Local(text.cast()).text())
}

/// The owner and access list of an open file, in the form [`private_security`] gives.
pub(crate) fn file_security(file: &File) -> io::Result<String> {
    let mut descriptor = std::ptr::null_mut();
    let information = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let null = std::ptr::null_mut;
    // SAFETY: The handle is open, and only the descriptor output is asked for, which is writable.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            information,
            null(),
            null(),
            null(),
            null(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(i32::try_from(status).unwrap_or(i32::MAX)));
    }
    security_text(&Local(descriptor))
}

/// The owner and access list every private file is given.
pub(crate) fn private_security() -> io::Result<String> {
    security_text(&private_descriptor()?)
}

/// What [`private_file`] does when the name is taken.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Existing {
    Refuse,
    Open,
}

/// Opens a file for reading and writing that only this user and SYSTEM may open, creating it
/// with that security in place from the start, so no other program can open it in between.
/// A link or other reparse point is never followed, and the file is refused unless it is a plain
/// file with exactly that security, so a new file on a volume that keeps no access lists is
/// removed and refused too. A file that may already exist is a lock, and is opened sharing no
/// deletion, so nobody can delete or rename it while it is held.
pub(crate) fn private_file(path: &Path, existing: Existing) -> io::Result<File> {
    let descriptor = private_descriptor()?;
    let attributes = SecurityAttributes {
        length: u32::try_from(size_of::<SecurityAttributes>()).expect("security attributes fit in a u32"),
        descriptor: descriptor.0,
        inherit: 0,
    };
    let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let (disposition, share) = match existing {
        Existing::Open => (OPEN_ALWAYS, FILE_SHARE_READ_WRITE),
        Existing::Refuse => (CREATE_NEW, FILE_SHARE_READ_WRITE_DELETE),
    };
    // SAFETY: The name is NUL-terminated, and the attributes and their descriptor live for the call.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            share,
            &raw const attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw as RawFd == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The handle was just created for this call and is closed with the file.
    let file = File::from(unsafe { OwnedHandle::from_raw_handle(raw) });
    if let Err(error) = private_as_made(&file, &descriptor, false) {
        if existing == Existing::Refuse {
            drop(file);
            let _ = std::fs::remove_file(path);
        }
        return Err(error);
    }
    Ok(file)
}

/// Whether an open file or folder is a plain one, not a reparse point, with exactly the security
/// `descriptor` gives.
fn private_as_made(file: &File, descriptor: &Local, folder: bool) -> io::Result<()> {
    let info = file.metadata()?;
    if info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.is_dir() != folder {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if file_security(file)? != security_text(descriptor)? {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "not private to this user"));
    }
    Ok(())
}

/// Opens a folder that only this user and SYSTEM may open, list or change, creating it with that
/// security from the start. Nobody else can add, remove or replace a name in it. An existing
/// folder must be a plain folder, not a link or junction, with exactly that security. The handle
/// shares no deletion, so the folder cannot be removed or renamed while it is held.
pub(crate) fn private_dir(path: &Path) -> io::Result<File> {
    let descriptor = private_descriptor()?;
    let attributes = SecurityAttributes {
        length: u32::try_from(size_of::<SecurityAttributes>()).expect("security attributes fit in a u32"),
        descriptor: descriptor.0,
        inherit: 0,
    };
    let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: The name is NUL-terminated, and the attributes and their descriptor live for the call.
    if unsafe { CreateDirectoryW(name.as_ptr(), &raw const attributes) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS) {
            return Err(error);
        }
    }
    // SAFETY: The name is NUL-terminated; no security or template is passed.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw as RawFd == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The handle was just opened for this call and is closed with the file.
    let folder = File::from(unsafe { OwnedHandle::from_raw_handle(raw) });
    private_as_made(&folder, &descriptor, true)?;
    Ok(folder)
}

/// A verbatim drive path without its \\?\ prefix, when the plain form names the same file:
/// Windows trims a trailing dot or space, reads a device name such as CON as the device and
/// stops at the old length limit, so such paths keep the prefix.
pub(crate) fn plain_path(path: PathBuf) -> PathBuf {
    let Some(plain) = path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) else { return path };
    let mut names = plain.split('\\');
    let drive = names.next().is_some_and(|drive| drive.len() == 2 && drive.ends_with(':'));
    let reserved = |name: &str| {
        let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
        matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
    };
    let changed = |name: &str| name.ends_with(['.', ' ']) || reserved(name);
    if !drive || plain.len() >= 260 || names.any(changed) {
        return path;
    }
    PathBuf::from(plain)
}

#[cfg(test)]
mod private_file_tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn a_private_file_is_owned_by_this_user_and_shut_to_everyone_else() {
        let fixture = Fixture::folder();
        let path = fixture.root.join("secret.txt");
        let file = private_file(&path, Existing::Refuse).unwrap();
        let expected = private_security().unwrap();
        // Windows writes a well-known account, such as the built-in Administrator, by its alias.
        let owner = expected.strip_prefix("O:").and_then(|rest| rest.split_once("D:")).unwrap().0;
        assert!(owner == user_sid().unwrap() || owner.len() == 2, "{expected}");
        assert_eq!(expected, format!("O:{owner}D:P(A;;FA;;;{owner})(A;;FA;;;SY)"));
        assert_eq!(file_security(&file).unwrap(), expected, "nothing is inherited from the folder");
        drop(file);
        assert_eq!(private_file(&path, Existing::Refuse).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        let reopened = private_file(&path, Existing::Open).unwrap();
        assert_eq!(file_security(&reopened).unwrap(), expected);

        let inherited = fixture.root.join("inherited.txt");
        std::fs::write(&inherited, "").unwrap();
        assert_ne!(file_security(&File::open(&inherited).unwrap()).unwrap(), expected);
        let refused = private_file(&inherited, Existing::Open).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_private_folder_is_shut_to_everyone_else_and_held_in_place() {
        let fixture = Fixture::folder();
        let path = fixture.root.join("private");
        let folder = private_dir(&path).unwrap();
        let expected = private_security().unwrap();
        assert_eq!(file_security(&folder).unwrap(), expected);
        assert!(std::fs::rename(&path, fixture.root.join("moved")).is_err(), "a held folder was renamed");
        assert_eq!(file_security(&private_dir(&path).unwrap()).unwrap(), expected, "a second open is refused");
        drop(folder);

        let inherited = fixture.root.join("inherited");
        std::fs::create_dir(&inherited).unwrap();
        assert_eq!(private_dir(&inherited).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        std::fs::write(fixture.root.join("file"), "").unwrap();
        assert!(private_dir(&fixture.root.join("file")).is_err());
        let junction = fixture.root.join("junction");
        let made =
            std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&junction).arg(&path).output().unwrap();
        assert!(made.status.success(), "cannot make a junction");
        assert_eq!(private_dir(&junction).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_lock_file_cannot_be_removed_while_it_is_open() {
        let fixture = Fixture::folder();
        let path = fixture.root.join("held.lock");
        let lock = private_file(&path, Existing::Open).unwrap();
        assert!(std::fs::remove_file(&path).is_err());
        assert!(std::fs::rename(&path, fixture.root.join("moved.lock")).is_err());
        drop(lock);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_private_file_is_never_reached_through_a_link() {
        let fixture = Fixture::folder();
        let target = fixture.root.join("target.txt");
        drop(private_file(&target, Existing::Refuse).unwrap());
        let link = fixture.root.join("link.txt");
        // Creating a link on Windows needs Developer Mode or an elevated user.
        if std::os::windows::fs::symlink_file(&target, &link).is_err() {
            return;
        }
        assert!(private_file(&link, Existing::Open).is_err());
        assert_eq!(private_file(&link, Existing::Refuse).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn random_bytes_differ_between_draws() {
        let (mut first, mut second) = ([0u8; 32], [0u8; 32]);
        random(&mut first).unwrap();
        random(&mut second).unwrap();
        assert_ne!(first, second);
        assert_ne!(first, [0; 32]);
    }
}

#[cfg(test)]
mod plain_path_tests {
    use super::plain_path;
    use std::path::PathBuf;

    #[test]
    fn only_a_drive_path_that_means_the_same_without_the_prefix_loses_it() {
        let plain = |text: &str| plain_path(PathBuf::from(text)).to_str().unwrap().to_owned();
        assert_eq!(plain(r"\\?\D:\a\_temp\project"), r"D:\a\_temp\project");
        assert_eq!(plain(r"\\?\D:\"), r"D:\");
        for kept in [r"\\?\D:\a\name.", r"\\?\D:\a\name ", r"\\?\D:\a\con.txt", r"\\?\D:\COM1", r"\\?\UNC\host\share\a"]
        {
            assert_eq!(plain(kept), kept);
        }
        let long = format!(r"\\?\D:\{}", "a".repeat(300));
        assert_eq!(plain(&long), long);
    }
}
