//! Reports directory changes to the event loop through macOS file notifications.
//! Other systems skip notifications and depend on periodic checks.
//!
//! FSEvents watches a whole directory tree through a single stream.
//! The loop also checks periodically on macOS in case notifications miss a change.

use std::io::{ErrorKind, PipeReader, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;

/// What to watch, what to leave out, and whether the roots cover every change.
pub struct Scope {
    pub roots: Vec<PathBuf>,
    pub ignored: Vec<PathBuf>,
    pub complete: bool,
}

/// One watcher per process: the callback finds its pipe through a static.
/// A running stream, its queue and its write end last until the process exits.
pub struct Watcher {
    /// Readable whenever something changed since the last `drain`.
    heard: Option<PipeReader>,
}

impl Watcher {
    pub fn new(scope: &Scope) -> Watcher {
        Watcher { heard: imp::start(&scope.roots, &scope.ignored) }
    }

    /// A descriptor to wait on alongside the keyboard, if watching works here.
    pub fn fd(&self) -> Option<RawFd> {
        self.heard.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Clears the waiting notification bytes; they carry no path details.
    pub fn drain(&self) {
        let Some(mut pipe) = self.heard.as_ref() else { return };
        let mut buf = [0u8; 256];
        loop {
            match pipe.read(&mut buf) {
                Ok(n) if n > 0 => {}
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                _ => break,
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::io::{self, PipeReader};
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicI32, Ordering};

    /// Seconds FSEvents can wait to combine follow-up notifications.
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
    // SAFETY: Declarations match CFArray.h, CFString.h and CFBase.h in the macOS SDK.
    unsafe extern "C" {
        // CFArray.h
        static kCFTypeArrayCallBacks: Opaque;
        // CFString.h
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> *const c_void;
        // CFArray.h
        fn CFArrayCreate(
            alloc: *const c_void,
            values: *mut *const c_void,
            count: isize,
            callbacks: *const Opaque,
        ) -> *const c_void;
        // CFBase.h
        fn CFRelease(object: *const c_void);
    }

    #[link(name = "CoreServices", kind = "framework")]
    // SAFETY: Declarations and callback ABI match FSEvents.h in the macOS SDK.
    unsafe extern "C" {
        fn FSEventStreamCreate(
            alloc: *const c_void,
            callback: Callback,
            context: *mut c_void,
            paths: *const c_void,
            since: u64,
            latency: f64,
            flags: u32,
        ) -> *mut c_void;
        fn FSEventStreamSetExclusionPaths(stream: *mut c_void, paths: *const c_void) -> u8;
        fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
        fn FSEventStreamStart(stream: *mut c_void) -> u8;
        fn FSEventStreamInvalidate(stream: *mut c_void);
        fn FSEventStreamRelease(stream: *mut c_void);
    }

    // SAFETY: Declarations match dispatch/queue.h and dispatch/object.h in the macOS SDK.
    unsafe extern "C" {
        // dispatch/queue.h
        fn dispatch_queue_create(label: *const libc::c_char, attr: *const c_void) -> *mut c_void;
        // dispatch/object.h
        fn dispatch_release(object: *mut c_void);
    }

    /// Runs on the stream's queue. The event loop checks what actually changed.
    extern "C" fn on_event(_: *const c_void, _: *mut c_void, _: usize, _: *mut c_void, _: *const u32, _: *const u64) {
        crate::term::wake(SIGNAL.load(Ordering::Relaxed));
    }

    pub(super) fn start(roots: &[PathBuf], ignored: &[PathBuf]) -> Option<PipeReader> {
        let (reader, writer) = io::pipe().ok()?;
        for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
            // SAFETY: each fd is an open pipe end; F_SETFL accepts an integer flag.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
                return None;
            }
        }
        SIGNAL.compare_exchange(-1, writer.as_raw_fd(), Ordering::Relaxed, Ordering::Relaxed).ok()?;
        if stream(roots, ignored) {
            // The callback can write for the rest of the process.
            let _ = writer.into_raw_fd();
            Some(reader)
        } else {
            SIGNAL.store(-1, Ordering::Relaxed);
            None
        }
    }

    /// Makes a Core Foundation array of paths that the caller must release.
    fn path_array(paths: &[PathBuf]) -> *const c_void {
        let Ok(count) = isize::try_from(paths.len()) else { return null() };
        let mut strings = Vec::with_capacity(paths.len());
        for path in paths {
            let bytes = path.as_os_str().as_bytes();
            let Ok(len) = isize::try_from(bytes.len()) else { break };
            // SAFETY: the bytes are readable for their stated length and copied by this call.
            let string = unsafe { CFStringCreateWithBytes(null(), bytes.as_ptr(), len, UTF8, 0) };
            if string.is_null() {
                break;
            }
            strings.push(string);
        }
        let array = if strings.len() == paths.len() {
            let callbacks = &raw const kCFTypeArrayCallBacks;
            // SAFETY: The slice holds `count` live CFStrings, which the array retains through the callbacks.
            unsafe { CFArrayCreate(null(), strings.as_mut_ptr(), count, callbacks) }
        } else {
            null()
        };
        for string in strings {
            // SAFETY: each non-null string was created above and is released once here.
            unsafe { CFRelease(string) };
        }
        array
    }

    /// Starts a stream on its own queue. The stream keeps the paths it needs.
    fn stream(roots: &[PathBuf], ignored: &[PathBuf]) -> bool {
        let paths = path_array(roots);
        if paths.is_null() {
            return false;
        }
        // SAFETY: paths is a live CFArray and the callback takes no context.
        let stream = unsafe { FSEventStreamCreate(null(), on_event, null_mut(), paths, SINCE_NOW, LATENCY, NO_DEFER) };
        // SAFETY: paths was created above and the stream keeps its own paths.
        unsafe { CFRelease(paths) };
        if stream.is_null() {
            return false;
        }
        if !ignored.is_empty() {
            let ignored = path_array(ignored);
            if ignored.is_null() {
                // SAFETY: stream is live and has not been scheduled.
                unsafe { FSEventStreamRelease(stream) };
                return false;
            }
            // SAFETY: stream is live and ignored is a live CFArray of directory strings.
            let excluded = unsafe { FSEventStreamSetExclusionPaths(stream, ignored) };
            // SAFETY: the stream keeps its own exclusions; release our array.
            unsafe { CFRelease(ignored) };
            if excluded == 0 {
                // SAFETY: stream is live and has not been scheduled.
                unsafe { FSEventStreamRelease(stream) };
                return false;
            }
        }
        // SAFETY: the label is a NUL-terminated string; null asks for a serial queue.
        let queue = unsafe { dispatch_queue_create(c"strata.watch".as_ptr(), null()) };
        if queue.is_null() {
            // SAFETY: stream is live and has not been scheduled.
            unsafe { FSEventStreamRelease(stream) };
            return false;
        }
        // SAFETY: The stream and queue are both live; setting the queue schedules the stream.
        unsafe { FSEventStreamSetDispatchQueue(stream, queue) };
        // SAFETY: The live stream has been scheduled on its queue.
        if unsafe { FSEventStreamStart(stream) } != 0 {
            return true;
        }
        // SAFETY: The live stream was scheduled above, so it can be invalidated.
        unsafe { FSEventStreamInvalidate(stream) };
        // SAFETY: This releases our reference to the stream after invalidating it.
        unsafe { FSEventStreamRelease(stream) };
        // SAFETY: This releases our reference to the queue after detaching the stream.
        unsafe { dispatch_release(queue) };
        false
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::io::PipeReader;
    use std::path::PathBuf;

    pub(super) fn start(_: &[PathBuf], _: &[PathBuf]) -> Option<PipeReader> {
        None
    }
}
