//! Reports directory changes to the event loop through macOS file notifications.
//! Other systems skip notifications and depend on periodic checks.
//!
//! FSEvents watches a whole directory tree through a single stream.
//! The loop also checks periodically on macOS in case notifications miss a change.

use crate::sys::{AsRawFd, RawFd};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{ErrorKind, PipeReader, Read};
#[cfg(any(test, target_os = "macos"))]
use std::path::Components;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// FSEvents accepts at most this many directory exclusions.
const NATIVE_LIMIT: usize = 8;

/// What to watch, what to leave out, and whether the roots cover every change.
pub struct Scope {
    pub roots: Vec<PathBuf>,
    pub ignored: Arc<Ignored>,
    pub complete: bool,
}

#[derive(Default, PartialEq, Eq)]
pub struct Ignored {
    pub dirs: HashSet<PathBuf>,
    pub files: HashSet<PathBuf>,
    pub(crate) git: Option<GitFilter>,
    pub events: Events,
}

#[derive(Default, PartialEq, Eq)]
pub enum Events {
    #[default]
    All,
    Reader {
        file: PathBuf,
        worktree: Option<PathBuf>,
        metadata: [Option<PathBuf>; 3],
    },
    /// Filter descendants only, never the directory entry shown by Outline.
    Collapsed(HashSet<PathBuf>),
}

/// Git puts linked worktrees in private directories under the common directory.
/// Only known private paths are filtered; shared and unclassified paths still wake us.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GitFilter {
    common: PathBuf,
    linked_id: Option<OsString>,
}

impl GitFilter {
    pub(crate) fn new(git: &Path, common: &Path) -> Option<Self> {
        if !common.is_absolute() {
            return None;
        }
        let linked_id = if git == common {
            None
        } else {
            let mut parts = git.strip_prefix(common).ok()?.components();
            if parts.next()?.as_os_str() != "worktrees" {
                return None;
            }
            let id = parts.next()?;
            if !matches!(id, std::path::Component::Normal(_)) || parts.next().is_some() {
                return None;
            }
            Some(id.as_os_str().to_owned())
        };
        Some(Self { common: common.to_owned(), linked_id })
    }

    #[cfg(any(test, target_os = "macos"))]
    fn allows(&self, path: &Path) -> bool {
        // Only exclude paths under the discovered directory spelling. A
        // case-folded prefix could also match unrelated paths on a sensitive volume.
        let Ok(relative) = path.strip_prefix(&self.common) else { return true };
        let mut parts = relative.components();
        let Some(first) = parts.next() else { return true };
        let first = first.as_os_str().as_encoded_bytes();
        if first == b"worktrees" {
            // An event for the container could include changes to the current worktree.
            let Some(id) = parts.next() else { return true };
            return self.linked_id.as_deref().is_some_and(|own| {
                let id = id.as_os_str();
                let [own_bytes, id_bytes] = [own.as_encoded_bytes(), id.as_encoded_bytes()];
                own_bytes.eq_ignore_ascii_case(id_bytes)
                    // FSEvents can use a different spelling on a folding volume.
                    // Don't rule out Unicode aliases or reinterpret invalid path bytes.
                    || ((!own_bytes.is_ascii() || !id_bytes.is_ascii())
                        && own.to_str().is_some() == id.to_str().is_some())
            });
        }
        self.linked_id.is_none() || !main_private(first, parts)
    }
}

/// Paths stored directly in the common directory for the main worktree only.
#[cfg(any(test, target_os = "macos"))]
fn main_private(first: &[u8], mut rest: Components<'_>) -> bool {
    let private_ref = |name: &[u8]| matches!(name, b"bisect" | b"rewritten" | b"worktree");
    let mut next = || rest.next().map(|part| part.as_os_str().as_encoded_bytes());
    match first.strip_suffix(b".lock").unwrap_or(first) {
        b"refs" => next().is_some_and(private_ref),
        b"logs" => match next() {
            Some(b"refs") => next().is_some_and(private_ref),
            Some(b"HEAD" | b"HEAD.lock") => true,
            _ => false,
        },
        b"info" => matches!(next(), Some(b"sparse-checkout" | b"sparse-checkout.lock")),
        b"index" | b"config.worktree" | b"HEAD" | b"ORIG_HEAD" | b"FETCH_HEAD" => true,
        b"AUTO_MERGE" | b"MERGE_AUTOSTASH" | b"MERGE_HEAD" | b"MERGE_MODE" | b"MERGE_MSG" | b"MERGE_RR" => true,
        b"BISECT_ANCESTORS_OK" | b"BISECT_EXPECTED_REV" | b"BISECT_HEAD" | b"BISECT_LOG" => true,
        b"BISECT_NAMES" | b"BISECT_RUN" | b"BISECT_START" | b"BISECT_TERMS" => true,
        b"CHERRY_PICK_HEAD" | b"REVERT_HEAD" | b"REBASE_HEAD" | b"COMMIT_EDITMSG" | b"SQUASH_MSG" => true,
        b"rebase-apply" | b"rebase-merge" | b"sequencer" => true,
        name => name.starts_with(b"sharedindex."),
    }
}

#[cfg(any(test, target_os = "macos"))]
impl Ignored {
    fn needs_paths(&self) -> bool {
        !self.dirs.is_empty()
            || !self.files.is_empty()
            || self.git.is_some()
            || match &self.events {
                Events::All => false,
                Events::Reader { .. } => true,
                Events::Collapsed(dirs) => !dirs.is_empty(),
            }
    }

    fn allows(&self, path: &Path, is_dir: bool) -> bool {
        if (!is_dir && self.files.contains(path))
            || (!self.dirs.is_empty() && path.ancestors().any(|p| self.dirs.contains(p)))
            || self.git.as_ref().is_some_and(|git| !git.allows(path))
        {
            return false;
        }
        let (file, worktree, metadata) = match &self.events {
            Events::All => return true,
            Events::Collapsed(dirs) => return !path.ancestors().skip(1).any(|p| dirs.contains(p)),
            Events::Reader { file, worktree, metadata } => (file, worktree, metadata),
        };
        // Reader permissions can be broader if the volume folds case.
        let under = |path: &Path, base: &Path| {
            let mut parts = path.components();
            base.components().all(|part| {
                parts.next().is_some_and(|p| {
                    p.as_os_str().as_encoded_bytes().eq_ignore_ascii_case(part.as_os_str().as_encoded_bytes())
                })
            })
        };
        under(file, path)
            || metadata.iter().flatten().any(|dir| under(path, dir) || under(dir, path))
            || worktree.as_ref().is_some_and(|root| {
                path.file_name().is_some_and(|name| {
                    let name = name.as_encoded_bytes();
                    (name.eq_ignore_ascii_case(b".gitignore") || name.eq_ignore_ascii_case(b".gitattributes"))
                        && path.parent().is_some_and(|dir| under(dir, root) && under(file, dir))
                })
            })
    }
}

pub struct Watcher {
    listener: Option<Listener>,
    scope: Scope,
    native: Vec<PathBuf>,
    retry: bool,
}

struct Listener {
    // Stop callbacks before closing the read end.
    stream: imp::Stream,
    heard: PipeReader,
}

impl Watcher {
    pub fn new(scope: Scope) -> Watcher {
        let native = native_exclusions(&scope.ignored.dirs, &[]);
        let listener = imp::start(&scope.roots, &native, Arc::clone(&scope.ignored));
        let retry = can_retry(&scope, &listener);
        Watcher { listener, scope, native, retry }
    }

    #[cfg(test)]
    pub(super) fn stop_listening(&mut self) {
        self.listener = None;
        self.retry = false;
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Reuses the native stream when only callback exclusions change.
    /// True means a follow-up snapshot is needed after changing live coverage.
    pub fn refresh(&mut self, scope: Scope) -> bool {
        let roots_changed = self.scope.roots != scope.roots;
        let ignored_changed = self.scope.ignored != scope.ignored;
        let mut restart = roots_changed
            || self.retry
            || self.listener.as_ref().is_some_and(|listener| listener.stream.take_root_change());
        if !restart && !ignored_changed {
            self.scope.complete = scope.complete;
            return false;
        }
        let was_live = self.listener.is_some();
        if ignored_changed && self.scope.ignored.dirs != scope.ignored.dirs {
            let native = native_exclusions(&scope.ignored.dirs, &self.native);
            restart |= self.native != native;
            self.native = native;
        }
        if restart {
            // Start listening on the new stream before dropping the old one.
            self.listener = imp::start(&scope.roots, &self.native, Arc::clone(&scope.ignored));
            self.retry = can_retry(&scope, &self.listener);
        } else if ignored_changed && let Some(listener) = &self.listener {
            listener.stream.set_ignored(Arc::clone(&scope.ignored));
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

fn can_retry(scope: &Scope, listener: &Option<Listener>) -> bool {
    cfg!(target_os = "macos") && listener.is_none() && !scope.roots.is_empty()
}

/// Keep occupied native slots so adding an overflow directory need not restart the stream.
fn native_exclusions(dirs: &HashSet<PathBuf>, previous: &[PathBuf]) -> Vec<PathBuf> {
    let mut native: Vec<_> = previous.iter().filter(|&path| dirs.contains(path)).cloned().collect();
    if native.len() < NATIVE_LIMIT {
        let mut candidates: Vec<_> = dirs
            .iter()
            .filter(|&path| path.to_str().is_some() && !previous.contains(path))
            .map(|path| (path.components().count(), path))
            .collect();
        candidates.sort_unstable();
        native.extend(candidates.into_iter().take(NATIVE_LIMIT - native.len()).map(|(_, path)| path.clone()));
    }
    native
}

#[cfg(target_os = "macos")]
mod imp {
    use crate::sys::AsRawFd;
    use std::ffi::{CStr, OsStr, c_void};
    use std::io::{self, PipeWriter};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use super::{Ignored, Listener};

    /// Seconds FSEvents can wait to combine follow-up notifications.
    const LATENCY: f64 = 0.05;
    const SINCE_NOW: u64 = u64::MAX;
    /// Deliver the first event of a burst at once, not after `LATENCY`.
    const NO_DEFER: u32 = 0x2;
    /// Report replacement or loss of a watched directory itself or one of its parents.
    const WATCH_ROOT: u32 = 0x4;
    /// Directory-only notifications cannot identify writes to individual files.
    const FILE_EVENTS: u32 = 0x10;
    const IS_DIR: u32 = 0x0002_0000;
    const UTF8: u32 = 0x0800_0100;
    const ROOT_CHANGED: u32 = 0x20;
    // Stream-wide flags (dropped/coalesced events, changed roots and mounted volumes)
    // require a check regardless of path. HistoryDone (0x10) isn't a change.
    const RESCAN: u32 = 0xef;

    struct CallbackState {
        signal: PipeWriter,
        ignored: Mutex<Arc<Ignored>>,
        root_changed: AtomicBool,
    }

    pub(super) struct Stream {
        stream: *mut c_void,
        queue: *mut c_void,
        callback: Arc<CallbackState>,
    }

    impl Stream {
        pub(super) fn set_ignored(&self, ignored: Arc<Ignored>) {
            *self.callback.ignored.lock().unwrap_or_else(PoisonError::into_inner) = ignored;
        }

        pub(super) fn take_root_change(&self) -> bool {
            self.callback.root_changed.swap(false, Ordering::Relaxed)
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
        if flags.is_null() {
            crate::term::wake(state.signal.as_raw_fd());
            return;
        }
        // SAFETY: FSEvents supplies one flag per event.
        let flags = unsafe { std::slice::from_raw_parts(flags, count) };
        let ignored = state.ignored.lock().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: Without UseCFTypes, FSEvents supplies `count` C strings when paths is non-null.
        let paths = (!paths.is_null() && ignored.needs_paths())
            .then(|| unsafe { std::slice::from_raw_parts(paths.cast::<*const libc::c_char>(), count) });
        let mut relevant = paths.is_none();
        for (i, &flag) in flags.iter().enumerate() {
            // The root flag can arrive after other relevant events in the batch.
            if flag & ROOT_CHANGED != 0 {
                state.root_changed.store(true, Ordering::Relaxed);
                relevant = true;
                break;
            }
            if let Some(paths) = paths.filter(|_| !relevant) {
                let path = paths[i];
                relevant = if flag & RESCAN != 0 || path.is_null() {
                    true
                } else {
                    // SAFETY: FSEvents guarantees the event path is a NUL-terminated string.
                    let path = unsafe { CStr::from_ptr(path) };
                    let path = Path::new(OsStr::from_bytes(path.to_bytes()));
                    ignored.allows(path, flag & IS_DIR != 0)
                };
            }
        }
        if relevant {
            crate::term::wake(state.signal.as_raw_fd());
        }
    }

    pub(super) fn start(roots: &[PathBuf], native: &[PathBuf], ignored: Arc<Ignored>) -> Option<Listener> {
        if roots.is_empty() || roots.iter().any(|path| !path.is_dir()) {
            return None;
        }
        let (reader, writer) = io::pipe().ok()?;
        for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
            crate::sys::set_nonblocking(fd).ok()?;
        }
        let callback = Arc::new(CallbackState {
            signal: writer,
            ignored: Mutex::new(ignored),
            root_changed: AtomicBool::new(false),
        });
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
        let stream = unsafe {
            FSEventStreamCreate(
                null(),
                on_event,
                &mut context,
                paths,
                SINCE_NOW,
                LATENCY,
                NO_DEFER | WATCH_ROOT | FILE_EVENTS,
            )
        };
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
    use std::sync::Arc;

    use super::{Ignored, Listener};

    pub(super) struct Stream;
    impl Stream {
        pub(super) fn set_ignored(&self, _: Arc<Ignored>) {}

        pub(super) fn take_root_change(&self) -> bool {
            false
        }
    }

    pub(super) fn start(_: &[PathBuf], _: &[PathBuf], _: Arc<Ignored>) -> Option<Listener> {
        None
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use crate::fixture::Fixture;
    use crate::git::Repo;
    #[cfg(unix)]
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn repository_scopes_filter_other_worktrees_but_keep_shared_git_changes() {
        let f = Fixture::new();
        f.write(".gitignore", "ignored.log\n");
        f.commit("first");
        f.git(&["worktree", "add", "-q", "../linked"]);
        f.git(&["worktree", "add", "-q", "../linked-other"]);

        let root = f.root.with_file_name("linked");
        let common = f.root.join(".git");
        let main = Repo::open(&f.root).unwrap().watch_scope();
        let mut repo = Repo::open(&root).unwrap();
        let linked = repo.watch_scope();
        assert_eq!(main.roots.as_slice(), std::slice::from_ref(&f.root));
        assert_eq!(linked.roots, [root.clone(), common.clone()]);
        assert!(main.ignored.needs_paths() && linked.ignored.needs_paths());

        for path in [
            "index",
            "index.lock",
            "sharedindex.0123456789abcdef",
            "HEAD",
            "config.worktree.lock",
            "logs/HEAD",
            "refs/rewritten/branch",
            "info/sparse-checkout",
            "rebase-merge/head-name",
        ] {
            let path = common.join(path);
            assert!(main.ignored.allows(&path, false), "{path:?}");
            assert!(!linked.ignored.allows(&path, false), "{path:?}");
        }

        for path in ["index", "index.lock", "HEAD", "config.worktree", "locked", "logs/HEAD", "commondir"] {
            let own = common.join("worktrees/linked").join(path);
            let sibling = common.join("worktrees/linked-other").join(path);
            assert!(!main.ignored.allows(&own, false), "{own:?}");
            assert!(linked.ignored.allows(&own, false), "{own:?}");
            assert!(!main.ignored.allows(&sibling, false), "{sibling:?}");
            assert!(!linked.ignored.allows(&sibling, false), "{sibling:?}");
        }

        for dir in ["", "worktrees", "logs", "logs/refs", "refs", "info", "objects", "reftable"] {
            let dir = common.join(dir);
            assert!(main.ignored.allows(&dir, true), "{dir:?}");
            assert!(linked.ignored.allows(&dir, true), "{dir:?}");
        }
        assert!(linked.ignored.allows(&common.join("worktrees/linked"), true));
        assert!(!linked.ignored.allows(&common.join("worktrees/linked-other"), true));
        for path in ["config", "config.lock", "info/exclude", "packed-refs", "refs/heads/main", "objects/01/blob"] {
            let path = common.join(path);
            assert!(main.ignored.allows(&path, false), "{path:?}");
            assert!(linked.ignored.allows(&path, false), "{path:?}");
        }
        assert!(main.ignored.allows(&common.with_file_name(".git-backup").join("worktrees/linked/index"), false));
        assert!(linked.ignored.allows(&root.join("index"), false));

        let ignored_file = root.join("ignored.log");
        std::fs::write(&ignored_file, "new ignored file").unwrap();
        repo.read();
        let after = repo.watch_scope().ignored;
        assert!(!after.allows(&ignored_file, false));
        assert!(!after.allows(&common.join("worktrees/linked-other/index"), false));
        assert!(after.allows(&common.join("worktrees/linked/index"), false));

        let reader = repo.reader_filter(Path::new("page.txt"));
        assert!(reader.allows(&root.join(".git"), false));
        assert!(reader.allows(&common.join("worktrees"), true));
        assert!(reader.allows(&common.join("refs/heads/main"), false));
        assert!(reader.allows(&common.join("worktrees/linked/index"), false));
        assert!(!reader.allows(&common.join("worktrees/linked-other/index"), false));
    }

    #[cfg(unix)]
    #[test]
    fn worktree_filter_has_component_boundaries_and_preserves_raw_path_bytes() {
        let common = Path::new("/project/.git");
        let main = GitFilter::new(common, common).unwrap();
        let linked = GitFilter::new(&common.join("worktrees/Current"), common).unwrap();
        let worktrees = common.join("worktrees");
        let invalid = OsStr::from_bytes(b"other-\xff");
        let sibling = worktrees.join(invalid);
        assert!(!main.allows(&sibling.join("index")));
        assert!(!linked.allows(&sibling.join("index")));
        assert!(!linked.allows(&worktrees.join("current-other").join(invalid)));
        assert!(linked.allows(&worktrees.join("CURRENT").join(invalid)));
        assert!(linked.allows(&common.join("refs/heads").join(invalid)));
        assert!(linked.allows(Path::new("/project/.Git/worktrees/current-other/index")));

        let raw_dir = common.join("worktrees").join(invalid);
        let raw = GitFilter::new(&raw_dir, common).unwrap();
        assert!(raw.allows(&raw_dir.join("index")));
        assert!(!raw.allows(&worktrees.join("current/index")));
        let unicode = GitFilter::new(&common.join("worktrees/Å"), common).unwrap();
        assert!(unicode.allows(&worktrees.join("a\u{30a}/index")));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use crate::git::Repo;
    use crate::keys::Key;
    use crate::outline::Outline;
    use crate::pane::{Pane, check_watched_pane, refresh_watch_after_input, watch_pane};
    use crate::reader::Reader;
    use crate::screen::Screen;

    fn changed(watcher: &Watcher, timeout_ms: i32) -> bool {
        let fd = watcher.fd().expect("the stream should have a notification descriptor");
        let mut fds = [crate::sys::pollfd { fd, events: crate::sys::POLLIN, revents: 0 }];
        match crate::term::wait_ready(&mut fds, timeout_ms) {
            0 => false,
            1 => {
                assert_eq!(fds[0].revents, crate::sys::POLLIN);
                watcher.drain();
                true
            }
            _ => panic!("watch poll failed: {}", std::io::Error::last_os_error()),
        }
    }

    fn settle(watcher: &Watcher) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while changed(watcher, 300) {
            assert!(std::time::Instant::now() < deadline, "fixture never settled");
        }
    }

    #[test]
    fn outline_watches_visible_directory_changes_and_updates_coverage_when_folds_change() {
        let f = Fixture::folder();
        f.write("fold/nested/hidden.txt", "old\n");
        let mut outline = Outline::new(Repo::open(&f.root).unwrap(), false);
        let (mut watcher, mut pace) = watch_pane(&mut outline, false);
        let fd = watcher.fd().unwrap();
        assert!(watcher.native.is_empty(), "native exclusions could hide the directory entry itself");
        let mut screen = Screen::sized(60, 8);
        let draw = |outline: &mut Outline, screen: &mut Screen, name: &str| {
            outline.draw(screen);
            screen.text().iter().any(|row| row.contains(name))
        };

        f.write("top.txt", "present\n");
        assert!(changed(&watcher, 30_000));
        assert!(check_watched_pane(&mut outline, &mut watcher, &mut pace, false));
        assert!(draw(&mut outline, &mut screen, "top.txt"));
        settle(&watcher);
        f.write("fold/nested/hidden.txt", "edited\n");
        f.write("fold/nested/added.txt", "new\n");
        assert!(!changed(&watcher, 300), "changes inside the closed folder must not wake Outline");

        outline.key(Key::Right);
        refresh_watch_after_input(&outline, &mut watcher, &mut pace, false);
        assert_eq!(watcher.fd(), Some(fd), "folds should not restart the native stream");
        f.write("fold/visible.txt", "new immediate child\n");
        assert!(changed(&watcher, 30_000));
        assert!(check_watched_pane(&mut outline, &mut watcher, &mut pace, false));
        assert!(draw(&mut outline, &mut screen, "visible.txt"));
        assert!(!draw(&mut outline, &mut screen, "added.txt"));
        settle(&watcher);
        f.write("fold/nested/later.txt", "new within the nested fold\n");
        assert!(!changed(&watcher, 300), "opening a parent must not open coverage under a nested fold");

        outline.key(Key::Down);
        outline.key(Key::Right);
        refresh_watch_after_input(&outline, &mut watcher, &mut pace, false);
        assert!(draw(&mut outline, &mut screen, "later.txt"), "opening a fold reads its current contents");
        outline.key(Key::Left);
        refresh_watch_after_input(&outline, &mut watcher, &mut pace, false);
        std::fs::rename(f.root.join("fold/nested"), f.root.join("fold/renamed")).unwrap();
        assert!(changed(&watcher, 30_000), "renaming the visible closed directory must wake Outline");
        assert!(check_watched_pane(&mut outline, &mut watcher, &mut pace, false));
        assert!(draw(&mut outline, &mut screen, "renamed"));
        assert!(!draw(&mut outline, &mut screen, "nested"));
        settle(&watcher);
        f.write("fold/renamed/still-hidden.txt", "new\n");
        assert!(!changed(&watcher, 300), "the renamed directory should now be filtered");
    }

    #[test]
    fn an_incomplete_git_directory_keeps_waking_outline_until_promotion() {
        let f = Fixture::folder();
        f.write("fold/hidden.txt", "untracked\n");
        let mut outline = Outline::new(Repo::open(&f.root).unwrap(), false);
        let (mut watcher, mut pace) = watch_pane(&mut outline, false);

        std::fs::create_dir(f.root.join(".git")).unwrap();
        assert!(changed(&watcher, 30_000));
        assert!(check_watched_pane(&mut outline, &mut watcher, &mut pace, false));
        assert!(outline.promoted_worktree().is_none());
        settle(&watcher);

        f.git(&["init", "-q", "--template=", "-b", "main"]);
        assert!(changed(&watcher, 30_000), "changes within .git must be noticed before promotion");
        assert!(check_watched_pane(&mut outline, &mut watcher, &mut pace, false));
        assert!(outline.promoted_worktree().is_some());
        let mut screen = Screen::sized(60, 8);
        outline.draw(&mut screen);
        assert!(screen.text()[0].contains("1 to commit"));
        settle(&watcher);

        f.write("fold/also-untracked.txt", "new\n");
        assert!(changed(&watcher, 30_000), "a promoted Outline needs the whole worktree");
        assert!(check_watched_pane(&mut outline, &mut watcher, &mut pace, false));
        outline.draw(&mut screen);
        assert!(screen.text()[0].contains("2 to commit"));
    }

    #[cfg(unix)]
    #[test]
    fn reader_wakes_for_its_file_rules_git_and_ancestor_but_not_other_work() {
        let f = Fixture::folder();
        let git = f.root.with_file_name("metadata");
        f.git(&["init", "-q", "--template=", "-b", "main", "--separate-git-dir", git.to_str().unwrap()]);
        f.write("src/page.txt", "the original text\n");
        f.write("src/.gitattributes", "page.txt diff\n");
        f.write("src/other.txt", "old sibling content\n");
        f.write("unrelated/.gitattributes", "* diff\n");
        f.commit("first");
        let mut reader = Reader::open(&f.root.join("src/page.txt")).unwrap();
        let mut watcher = Watcher::new(reader.watch_scope());
        let check = |reader: &mut Reader, watcher: &mut Watcher, expected: &str| {
            assert!(changed(watcher, 30_000), "expected a wake for {expected:?}");
            reader.filesystem_changed();
            assert!(reader.check());
            watcher.refresh(reader.watch_scope());
            let mut screen = Screen::sized(60, 8);
            reader.draw(&mut screen);
            let rows = screen.text();
            assert!(rows.iter().any(|row| row.contains(expected)), "{expected:?}: {rows:?}");
            settle(watcher);
        };

        f.write("src/page.txt", "edited text\n");
        check(&mut reader, &mut watcher, "+ edited text");
        f.write("src/other.txt", "new sibling content\n");
        f.write("unrelated/.gitattributes", "* -diff\n");
        assert!(!changed(&watcher, 300), "edits unrelated to the Reader must not wake it");

        f.write("src/.gitattributes", "page.txt -diff\n");
        check(&mut reader, &mut watcher, "This file is not text.");
        f.write("src/.gitattributes", "page.txt diff\n");
        check(&mut reader, &mut watcher, "+ edited text");
        f.write("src/.gitignore", "other.txt\n");
        check(&mut reader, &mut watcher, "+ edited text");
        f.commit("second");
        check(&mut reader, &mut watcher, "1   edited text");

        // Rename only the parent: there need not be an event for its contents.
        std::fs::rename(f.root.join("src"), f.root.join("moved")).unwrap();
        check(&mut reader, &mut watcher, "- edited text");

        // A replacement directory link can make the file live at a different
        // path under the watch root. Do not lose writes to that resolved file.
        std::os::unix::fs::symlink("moved", f.root.join("src")).unwrap();
        check(&mut reader, &mut watcher, "1   edited text");
        f.write("moved/page.txt", "through an ancestor link\n");
        check(&mut reader, &mut watcher, "through an ancestor link");
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
        let scope = |dirs: &[PathBuf]| Scope {
            roots: vec![f.root.clone()],
            ignored: Arc::new(Ignored { dirs: dirs.iter().cloned().collect(), ..Ignored::default() }),
            complete: true,
        };
        let mut watcher = Watcher::new(scope(&ignored));
        let original = watcher.fd().unwrap();

        // A path ahead of the existing eight in sort order must not evict one.
        ignored.insert(0, f.root.join("a-build"));
        assert!(watcher.refresh(scope(&ignored)));
        assert_eq!(watcher.fd(), Some(original));
        assert!(!watcher.refresh(scope(&ignored)));
        // Startup can deliver coalesced events from creating the fixture. Observe a
        // visible write, then drain that batch before measuring only ignored writes.
        f.write("a-build-source/code", "ready");
        assert!(changed(&watcher, 30_000));
        settle(&watcher);
        f.write("cache-0/log", "kernel exclusion");
        f.write("a-build/log", "overflow exclusion");
        assert!(!changed(&watcher, 300), "ignored build writes must not wake Git");

        f.write("a-build/log", "more output");
        f.write("a-build-source/code", "new source");
        assert!(changed(&watcher, 30_000), "an ignored prefix must not hide its sibling");

        ignored.remove(0);
        assert!(watcher.refresh(scope(&ignored)));
        assert_eq!(watcher.fd(), Some(original));
        f.write("a-build/log", "no longer ignored");
        assert!(changed(&watcher, 30_000));
    }

    #[test]
    fn ignored_files_are_callback_only_and_get_rechecked_after_ignore_and_index_edits() {
        let f = Fixture::new();
        f.write(".gitignore", ".gitignore\nbuild.log\nscratch.log\n");
        f.write("build.log", "old build output");
        f.write("scratch.log", "old scratch output");
        f.write("build.log-source", "source");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let mut watcher = Watcher::new(repo.watch_scope());
        let original = watcher.fd().unwrap();
        assert!(watcher.native.is_empty(), "files must not use native directory slots");

        f.write("build.log-source", "ready");
        assert!(changed(&watcher, 30_000), "a prefix shared with an ignored file is still visible");
        settle(&watcher);
        f.write("build.log", "new build output");
        f.write("scratch.log", "new scratch output");
        assert!(!changed(&watcher, 300), "ignored file writes must not wake Git");

        f.write(".gitignore", ".gitignore\nscratch.log\n");
        assert!(changed(&watcher, 30_000), "an ignored .gitignore must still wake Git");
        repo.read();
        assert!(watcher.refresh(repo.watch_scope()));
        assert_eq!(watcher.fd(), Some(original));
        settle(&watcher);
        f.write("build.log", "no longer ignored");
        assert!(changed(&watcher, 30_000));
        settle(&watcher);

        f.git(&["add", "-f", "--", "scratch.log"]);
        assert!(changed(&watcher, 30_000), "changing the index must still wake Git");
        repo.read();
        assert!(watcher.refresh(repo.watch_scope()));
        assert_eq!(watcher.fd(), Some(original));
        settle(&watcher);
        f.write("scratch.log", "now tracked");
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

    #[test]
    fn a_removed_watch_root_is_reattached_when_recreated_at_the_same_path() {
        let f = Fixture::folder();
        let root = f.root.join("watched");
        std::fs::create_dir(&root).unwrap();
        let scope = || Scope { roots: vec![root.clone()], ignored: Arc::default(), complete: true };
        let mut watcher = Watcher::new(scope());
        f.write("watched/ready", "ready");
        assert!(changed(&watcher, 30_000));
        settle(&watcher);

        std::fs::rename(&root, f.root.join("old")).unwrap();
        assert!(changed(&watcher, 30_000), "the root's own rename must wake the pane");
        while !watcher.refresh(scope()) {
            assert!(changed(&watcher, 30_000), "the root change must invalidate the stream");
        }
        assert!(watcher.fd().is_none(), "a missing root cannot provide live coverage");

        std::fs::create_dir(&root).unwrap();
        assert!(watcher.refresh(scope()), "an unchanged path must be reattached after it returns");
        f.write("watched/later", "change in the replacement root");
        assert!(changed(&watcher, 30_000));
    }
}
