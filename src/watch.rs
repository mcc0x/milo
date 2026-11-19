//! Reports directory changes to the event loop through macOS file notifications.
//! Other systems skip notifications and depend on periodic checks.
//!
//! FSEvents watches a whole directory tree through a single stream.
//! The loop also checks periodically on macOS in case notifications miss a change.

use std::io::{ErrorKind, PipeReader, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::rc::Rc;

/// FSEvents accepts at most this many directory exclusions.
const NATIVE_LIMIT: usize = 8;

/// What to watch, what to leave out, and whether the roots cover every change.
pub struct Scope {
    pub roots: Vec<PathBuf>,
    pub ignored: Rc<[PathBuf]>,
    pub complete: bool,
}

pub struct Watcher {
    listener: Option<Listener>,
    scope: Scope,
    native: Vec<PathBuf>,
}

struct Listener {
    // Stop callbacks before closing the read end.
    stream: imp::Stream,
    heard: PipeReader,
}

impl Watcher {
    pub fn new(scope: Scope) -> Watcher {
        let native = native_exclusions(&scope.ignored, &[]);
        let listener = imp::start(&scope.roots, &native, &scope.ignored);
        Watcher { listener, scope, native }
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Reuses the native stream when only overflow exclusions change.
    /// True means a follow-up snapshot is needed after changing live coverage.
    pub fn refresh(&mut self, scope: Scope) -> bool {
        let roots_changed = self.scope.roots != scope.roots;
        let ignored_changed = self.scope.ignored != scope.ignored;
        if !roots_changed && !ignored_changed {
            self.scope = scope;
            return false;
        }
        let was_live = self.listener.is_some();
        let mut restart = roots_changed;
        if ignored_changed {
            let native = native_exclusions(&scope.ignored, &self.native);
            restart |= self.native != native;
            self.native = native;
        }
        if restart {
            // Start listening on the new stream before dropping the old one.
            self.listener = imp::start(&scope.roots, &self.native, &scope.ignored);
        } else if ignored_changed && let Some(listener) = &self.listener {
            listener.stream.set_ignored(&scope.ignored);
        }
        self.scope = scope;
        was_live || self.listener.is_some()
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

/// Keep occupied native slots so adding an overflow directory need not restart the stream.
fn native_exclusions(ignored: &[PathBuf], previous: &[PathBuf]) -> Vec<PathBuf> {
    previous
        .iter()
        .filter(|path| ignored.contains(path))
        .chain(ignored.iter().filter(|path| path.to_str().is_some() && !previous.contains(path)))
        .take(NATIVE_LIMIT)
        .cloned()
        .collect()
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashSet;
    use std::ffi::{CStr, OsStr, c_void};
    use std::io::{self, PipeWriter};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};
    use std::sync::{Arc, Mutex, PoisonError};

    use super::Listener;

    /// Seconds FSEvents can wait to combine follow-up notifications.
    const LATENCY: f64 = 0.05;
    const SINCE_NOW: u64 = u64::MAX;
    /// Deliver the first event of a burst at once, not after `LATENCY`.
    const NO_DEFER: u32 = 0x2;
    const UTF8: u32 = 0x0800_0100;
    // Stream-wide flags (dropped/coalesced events, changed roots and mounted volumes)
    // require a check regardless of path. HistoryDone (0x10) isn't a change.
    const RESCAN: u32 = 0xef;

    struct CallbackState {
        signal: PipeWriter,
        ignored: Mutex<HashSet<PathBuf>>,
    }

    pub(super) struct Stream {
        stream: *mut c_void,
        queue: *mut c_void,
        callback: Arc<CallbackState>,
    }

    impl Stream {
        pub(super) fn set_ignored(&self, paths: &[PathBuf]) {
            let ignored = paths.iter().cloned().collect();
            *self.callback.ignored.lock().unwrap_or_else(PoisonError::into_inner) = ignored;
        }
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
        count: usize,
        paths: *mut c_void,
        flags: *const u32,
        _: *const u64,
    ) {
        // SAFETY: The context is held by Stream until its serial callback queue is drained.
        let state = unsafe { &*info.cast::<CallbackState>() };
        let ignored = state.ignored.lock().unwrap_or_else(PoisonError::into_inner);
        let relevant = if ignored.is_empty() || paths.is_null() || flags.is_null() {
            true
        } else {
            // SAFETY: Without UseCFTypes, FSEvents supplies `count` C strings and flags.
            let paths = unsafe { std::slice::from_raw_parts(paths.cast::<*const libc::c_char>(), count) };
            // SAFETY: The flags array contains one entry per path.
            let flags = unsafe { std::slice::from_raw_parts(flags, count) };
            paths.iter().zip(flags).any(|(&path, &flags)| {
                if flags & RESCAN != 0 || path.is_null() {
                    return true;
                }
                // SAFETY: FSEvents guarantees the event path is a NUL-terminated string.
                let path = unsafe { CStr::from_ptr(path) };
                let path = Path::new(OsStr::from_bytes(path.to_bytes()));
                !path.ancestors().any(|ancestor| ignored.contains(ancestor))
            })
        };
        if relevant {
            crate::term::wake(state.signal.as_raw_fd());
        }
    }

    pub(super) fn start(roots: &[PathBuf], native: &[PathBuf], ignored: &[PathBuf]) -> Option<Listener> {
        if roots.is_empty() {
            return None;
        }
        let (reader, writer) = io::pipe().ok()?;
        for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
            // SAFETY: each fd is an open pipe end; F_SETFL accepts an integer flag.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
                return None;
            }
        }
        let ignored = Mutex::new(ignored.iter().cloned().collect());
        let callback = Arc::new(CallbackState { signal: writer, ignored });
        let (stream, queue) = start_stream(roots, native, Arc::as_ptr(&callback))?;
        Some(Listener { stream: Stream { stream, queue, callback }, heard: reader })
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
    fn start_stream(
        roots: &[PathBuf],
        native: &[PathBuf],
        callback: *const CallbackState,
    ) -> Option<(*mut c_void, *mut c_void)> {
        let paths = path_array(roots);
        if paths.is_null() {
            return None;
        }
        let info = callback.cast_mut().cast::<c_void>();
        let mut context = Context { version: 0, info, retain: None, release: None, describe: None };
        // SAFETY: paths is a live CFArray. The stream copies the fields of context.
        let stream =
            unsafe { FSEventStreamCreate(null(), on_event, &mut context, paths, SINCE_NOW, LATENCY, NO_DEFER) };
        // SAFETY: paths was created above and the stream keeps its own paths.
        unsafe { CFRelease(paths) };
        if stream.is_null() {
            return None;
        }
        if !native.is_empty() {
            let paths = path_array(native);
            if !paths.is_null() {
                // SAFETY: the unscheduled stream and the CFArray of paths are live.
                // If native filtering fails, the callback still filters all ignored paths.
                unsafe { FSEventStreamSetExclusionPaths(stream, paths) };
                // SAFETY: the stream keeps its own exclusion paths.
                unsafe { CFRelease(paths) };
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
    impl Stream {
        pub(super) fn set_ignored(&self, _: &[PathBuf]) {}
    }

    pub(super) fn start(_: &[PathBuf], _: &[PathBuf], _: &[PathBuf]) -> Option<Listener> {
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn changed(watcher: &Watcher, timeout_ms: i32) -> bool {
        let fd = watcher.fd().expect("the stream should have a notification descriptor");
        let mut fds = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }];
        match crate::term::wait_ready(&mut fds, timeout_ms) {
            0 => false,
            1 => {
                assert_eq!(fds[0].revents, libc::POLLIN);
                watcher.drain();
                true
            }
            _ => panic!("watch poll failed: {}", std::io::Error::last_os_error()),
        }
    }

    #[test]
    fn overflow_exclusions_are_live_without_restarting_or_hiding_a_source_sibling() {
        let f = Fixture::folder();
        let mut ignored: Vec<_> = (0..NATIVE_LIMIT).map(|i| f.root.join(format!("cache-{i}"))).collect();
        for path in &ignored {
            std::fs::create_dir(path).unwrap();
        }
        f.write("a-build/log", "old");
        f.write("a-build-source/code", "old");
        let roots = vec![f.root.clone()];
        let mut watcher = Watcher::new(Scope { roots: roots.clone(), ignored: ignored.clone().into(), complete: true });
        let original = watcher.fd().unwrap();

        // A path ahead of the existing eight in sort order must not evict one.
        ignored.insert(0, f.root.join("a-build"));
        assert!(watcher.refresh(Scope { roots: roots.clone(), ignored: ignored.clone().into(), complete: true }));
        assert_eq!(watcher.fd(), Some(original));
        assert!(!watcher.refresh(Scope { roots: roots.clone(), ignored: ignored.clone().into(), complete: true }));
        // Startup can deliver coalesced events from creating the fixture. Observe a
        // visible write, then drain that batch before measuring only ignored writes.
        f.write("a-build-source/code", "ready");
        assert!(changed(&watcher, 30_000));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while changed(&watcher, 300) {
            assert!(std::time::Instant::now() < deadline, "fixture never settled");
        }
        f.write("cache-0/log", "kernel exclusion");
        f.write("a-build/log", "overflow exclusion");
        assert!(!changed(&watcher, 300), "ignored build writes must not wake Git");

        f.write("a-build/log", "more output");
        f.write("a-build-source/code", "new source");
        assert!(changed(&watcher, 30_000), "an ignored prefix must not hide its sibling");

        ignored.remove(0);
        assert!(watcher.refresh(Scope { roots, ignored: ignored.into(), complete: true }));
        assert_eq!(watcher.fd(), Some(original));
        f.write("a-build/log", "no longer ignored");
        assert!(changed(&watcher, 30_000));
    }

    #[test]
    fn the_next_pane_gets_live_changes_when_it_replaces_a_watched_pane() {
        let watch = |folder: &Fixture| {
            Watcher::new(Scope { roots: vec![folder.root.clone()], ignored: Default::default(), complete: true })
        };
        let first = Fixture::folder();
        let original = watch(&first);
        assert!(original.fd().is_some());

        let second = Fixture::folder();
        let replacement = watch(&second);
        drop(original);
        second.write("new.txt", "recent change\n");
        assert!(changed(&replacement, 30_000), "the next pane should also get live changes");
    }
}
