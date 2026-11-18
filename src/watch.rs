//! Hears from the operating system the moment anything under a directory
//! changes, so the outline need not wait for its next periodic check.
//!
//! On macOS this is FSEvents, which watches a whole tree through one stream.
//! Elsewhere there is no watcher and the periodic check does all the work.

use std::os::fd::RawFd;
use std::path::Path;

pub struct Watcher {
    /// Readable whenever something changed since the last `drain`.
    fd: Option<RawFd>,
}

impl Watcher {
    pub fn new(root: &Path) -> Watcher {
        Watcher { fd: imp::start(root) }
    }

    /// A descriptor to wait on alongside the keyboard, if watching works here.
    pub fn fd(&self) -> Option<RawFd> {
        self.fd
    }

    /// Forgets the changes heard so far.
    pub fn drain(&self) {
        let Some(fd) = self.fd else { return };
        let mut buf = [0u8; 256];
        // The pipe is non-blocking: read until it is empty.
        while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::os::fd::RawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// Changes within this many seconds of each other arrive as one event.
    const LATENCY: f64 = 0.05;
    const SINCE_NOW: u64 = u64::MAX;
    /// Deliver the first event of a burst at once, not after `LATENCY`.
    const NO_DEFER: u32 = 0x2;
    const UTF8: u32 = 0x0800_0100;

    /// The write end of the pipe the callback signals through.
    static SIGNAL: AtomicI32 = AtomicI32::new(-1);

    #[repr(C)]
    struct Opaque {
        _private: [u8; 0],
    }

    type Callback = extern "C" fn(*const c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFTypeArrayCallBacks: Opaque;
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> *const c_void;
        fn CFArrayCreate(
            alloc: *const c_void,
            values: *const *const c_void,
            count: isize,
            callbacks: *const Opaque,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn FSEventStreamCreate(
            alloc: *const c_void,
            callback: Callback,
            context: *const c_void,
            paths: *const c_void,
            since: u64,
            latency: f64,
            flags: u32,
        ) -> *mut c_void;
        fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
        fn FSEventStreamStart(stream: *mut c_void) -> u8;
    }

    extern "C" {
        fn dispatch_queue_create(label: *const libc::c_char, attr: *const c_void) -> *mut c_void;
    }

    /// Runs on the stream's own queue. It only says "something changed": the
    /// outline works out what by looking, as it does for its periodic check.
    extern "C" fn on_event(_: *const c_void, _: *mut c_void, _: usize, _: *mut c_void, _: *const u32, _: *const u64) {
        let fd = SIGNAL.load(Ordering::Relaxed);
        // A full pipe already says so; a failed write loses nothing.
        unsafe {
            libc::write(fd, [1u8].as_ptr().cast(), 1);
        }
    }

    /// Starts watching `root` for the life of the process. Returns the read
    /// end of a pipe that becomes readable on every change.
    pub fn start(root: &Path) -> Option<RawFd> {
        let mut fds = [0 as RawFd; 2];
        unsafe {
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return None;
            }
            // Non-blocking, and not inherited by the git commands we run.
            for fd in fds {
                libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            SIGNAL.store(fds[1], Ordering::Relaxed);
            if stream(root) {
                return Some(fds[0]);
            }
            // Nothing will ever write to the pipe: give both ends back.
            SIGNAL.store(-1, Ordering::Relaxed);
            libc::close(fds[0]);
            libc::close(fds[1]);
            None
        }
    }

    /// Creates the stream on `root` and starts it. The stream keeps its own
    /// copy of the paths, so the string and the array are released here.
    unsafe fn stream(root: &Path) -> bool {
        let bytes = root.as_os_str().as_bytes();
        let path = CFStringCreateWithBytes(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, UTF8, 0);
        if path.is_null() {
            return false;
        }
        let paths = CFArrayCreate(std::ptr::null(), &path, 1, &kCFTypeArrayCallBacks);
        CFRelease(path);
        if paths.is_null() {
            return false;
        }
        let stream =
            FSEventStreamCreate(std::ptr::null(), on_event, std::ptr::null(), paths, SINCE_NOW, LATENCY, NO_DEFER);
        CFRelease(paths);
        if stream.is_null() {
            return false;
        }
        let queue = dispatch_queue_create(c"strata.watch".as_ptr(), std::ptr::null());
        FSEventStreamSetDispatchQueue(stream, queue);
        FSEventStreamStart(stream) != 0
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::os::fd::RawFd;
    use std::path::Path;

    pub fn start(_: &Path) -> Option<RawFd> {
        None
    }
}
