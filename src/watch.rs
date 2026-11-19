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

pub struct Watcher {
    listener: Option<Listener>,
}

struct Listener {
    // Stop callbacks before closing the read end.
    _stream: imp::Stream,
    heard: PipeReader,
}

impl Watcher {
    pub fn new(scope: &Scope) -> Watcher {
        Watcher { listener: imp::start(&scope.roots, &scope.ignored) }
    }

    /// A descriptor to wait on alongside the keyboard, if watching works here.
    pub fn fd(&self) -> Option<RawFd> {
        self.listener.as_ref().map(|listener| listener.heard.as_raw_fd())
    }

    /// Clears the waiting notification bytes; they carry no path details.
    pub fn drain(&self) {
        let Some(listener) = &self.listener else { return };
        let mut pipe = &listener.heard;
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
    use std::io::{self, PipeWriter};
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::ptr::{null, null_mut, without_provenance_mut};

    use super::Listener;

    /// Seconds FSEvents can wait to combine follow-up notifications.
    const LATENCY: f64 = 0.05;
    const SINCE_NOW: u64 = u64::MAX;
    /// Deliver the first event of a burst at once, not after `LATENCY`.
    const NO_DEFER: u32 = 0x2;
    const UTF8: u32 = 0x0800_0100;

    pub(super) struct Stream {
        stream: *mut c_void,
        queue: *mut c_void,
        _signal: PipeWriter,
    }

    impl Drop for Stream {
        fn drop(&mut self) {
            extern "C" fn drained(_: *mut c_void) {}
            // SAFETY: This stream was started on our serial queue. Stop and detach it,
            // then wait for any callback already on the queue before closing its pipe.
            unsafe { FSEventStreamStop(self.stream) };
            // SAFETY: The stopped stream is still scheduled on the queue.
            unsafe { FSEventStreamInvalidate(self.stream) };
            // SAFETY: The queue remains live and this drop runs outside it.
            unsafe { dispatch_sync_f(self.queue, null_mut(), drained) };
            // SAFETY: Release our retained references after detaching the stream.
            unsafe { FSEventStreamRelease(self.stream) };
            // SAFETY: We own the queue and have drained it.
            unsafe { dispatch_release(self.queue) };
        }
    }

    #[repr(C)]
    struct Opaque {
        _private: [u8; 0],
    }

    type Callback = extern "C" fn(*const c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);

    #[repr(C)]
    struct Context {
        version: isize,
        info: *mut c_void,
        retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
        release: Option<extern "C" fn(*const c_void)>,
        describe: Option<extern "C" fn(*const c_void) -> *const c_void>,
    }

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
            context: *mut Context,
            paths: *const c_void,
            since: u64,
            latency: f64,
            flags: u32,
        ) -> *mut c_void;
        fn FSEventStreamSetExclusionPaths(stream: *mut c_void, paths: *const c_void) -> u8;
        fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
        fn FSEventStreamStart(stream: *mut c_void) -> u8;
        fn FSEventStreamStop(stream: *mut c_void);
        fn FSEventStreamInvalidate(stream: *mut c_void);
        fn FSEventStreamRelease(stream: *mut c_void);
    }

    // SAFETY: Declarations match dispatch/queue.h and dispatch/object.h in the macOS SDK.
    unsafe extern "C" {
        // dispatch/queue.h
        fn dispatch_queue_create(label: *const libc::c_char, attr: *const c_void) -> *mut c_void;
        fn dispatch_sync_f(queue: *mut c_void, context: *mut c_void, task: extern "C" fn(*mut c_void));
        // dispatch/object.h
        fn dispatch_release(object: *mut c_void);
    }

    /// Runs on the stream's queue. The event loop checks what actually changed.
    extern "C" fn on_event(
        _: *const c_void,
        info: *mut c_void,
        _: usize,
        _: *mut c_void,
        _: *const u32,
        _: *const u64,
    ) {
        if let Ok(fd) = RawFd::try_from(info.addr().wrapping_sub(1)) {
            crate::term::wake(fd);
        }
    }

    pub(super) fn start(roots: &[PathBuf], ignored: &[PathBuf]) -> Option<Listener> {
        let (reader, writer) = io::pipe().ok()?;
        for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
            // SAFETY: each fd is an open pipe end; F_SETFL accepts an integer flag.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
                return None;
            }
        }
        let (stream, queue) = start_stream(roots, ignored, writer.as_raw_fd())?;
        Some(Listener { _stream: Stream { stream, queue, _signal: writer }, heard: reader })
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
    fn start_stream(roots: &[PathBuf], ignored: &[PathBuf], signal: RawFd) -> Option<(*mut c_void, *mut c_void)> {
        let paths = path_array(roots);
        if paths.is_null() {
            return None;
        }
        // Pass the descriptor as an integer cookie, never as an address to dereference.
        let info = without_provenance_mut(usize::try_from(signal).expect("an open pipe descriptor") + 1);
        let mut context = Context { version: 0, info, retain: None, release: None, describe: None };
        // SAFETY: paths is a live CFArray. The stream copies the fields of context.
        let stream =
            unsafe { FSEventStreamCreate(null(), on_event, &mut context, paths, SINCE_NOW, LATENCY, NO_DEFER) };
        // SAFETY: paths was created above and the stream keeps its own paths.
        unsafe { CFRelease(paths) };
        if stream.is_null() {
            return None;
        }
        if !ignored.is_empty() {
            let ignored = path_array(ignored);
            if ignored.is_null() {
                // SAFETY: stream is live and has not been scheduled.
                unsafe { FSEventStreamRelease(stream) };
                return None;
            }
            // SAFETY: stream is live and ignored is a live CFArray of directory strings.
            let excluded = unsafe { FSEventStreamSetExclusionPaths(stream, ignored) };
            // SAFETY: the stream keeps its own exclusions; release our array.
            unsafe { CFRelease(ignored) };
            if excluded == 0 {
                // SAFETY: stream is live and has not been scheduled.
                unsafe { FSEventStreamRelease(stream) };
                return None;
            }
        }
        // SAFETY: the label is a NUL-terminated string; null asks for a serial queue.
        let queue = unsafe { dispatch_queue_create(c"milo.watch".as_ptr(), null()) };
        if queue.is_null() {
            // SAFETY: stream is live and has not been scheduled.
            unsafe { FSEventStreamRelease(stream) };
            return None;
        }
        // SAFETY: The stream and queue are both live; setting the queue schedules the stream.
        unsafe { FSEventStreamSetDispatchQueue(stream, queue) };
        // SAFETY: The live stream has been scheduled on its queue.
        if unsafe { FSEventStreamStart(stream) } != 0 {
            return Some((stream, queue));
        }
        // SAFETY: The live stream was scheduled above, so it can be invalidated.
        unsafe { FSEventStreamInvalidate(stream) };
        // SAFETY: This releases our reference to the stream after invalidating it.
        unsafe { FSEventStreamRelease(stream) };
        // SAFETY: This releases our reference to the queue after detaching the stream.
        unsafe { dispatch_release(queue) };
        None
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::path::PathBuf;

    use super::Listener;

    pub(super) struct Stream;
    pub(super) fn start(_: &[PathBuf], _: &[PathBuf]) -> Option<Listener> {
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn the_next_pane_gets_live_changes_when_it_replaces_a_watched_pane() {
        let watch = |folder: &Fixture| {
            Watcher::new(&Scope { roots: vec![folder.root.clone()], ignored: Vec::new(), complete: true })
        };
        let first = Fixture::folder();
        let original = watch(&first);
        assert!(original.fd().is_some());

        let second = Fixture::folder();
        let replacement = watch(&second);
        drop(original);
        second.write("new.txt", "recent change\n");
        let fd = replacement.fd().expect("the next pane should also get a watcher");
        let mut fds = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }];
        assert_eq!(crate::term::wait_ready(&mut fds, 30_000), 1);
        assert_eq!(fds[0].revents, libc::POLLIN);
    }
}
