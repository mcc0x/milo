//! Walks the session trees without following links and reads within a refresh budget.
//!

#[cfg(unix)]
use crate::sys::RawFd;
use std::ffi::OsStr;
#[cfg(unix)]
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read};
#[cfg(unix)]
use std::mem::MaybeUninit;
use std::ops::ControlFlow;
#[cfg(unix)]
use std::os::fd::{FromRawFd, IntoRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use crate::fs_ops;
#[cfg(unix)]
use crate::fs_ops::READ_FLAGS;
use crate::sys::AsRawFd;

pub(super) const CODEX_FILES: usize = 4;
const CLAUDE_FILES: usize = 128;
const MAX_ENTRIES: usize = 32_768;
const MAX_LINE: usize = 2 * 1024 * 1024;
const BLOCK: usize = 32 * 1024;

/// Pathless notifications arrive on writes as well as additions. Coalesce the walks.
const DISCOVERY_REST: Duration = Duration::from_secs(2);
const DISCOVERY_IDLE: Duration = Duration::from_secs(30);

/// Incremental transcripts use file identity, length and mtime, not metadata-only changes such as chmod.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct FileVersion {
    device: i128,
    inode: u128,
    modified: (i128, i128),
    pub len: u64,
}

impl FileVersion {
    /// Other filesystem objects are not session data.
    #[cfg(unix)]
    fn new(stat: &libc::stat) -> Option<Self> {
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return None;
        }
        Some(Self {
            // MetadataExt exposes the device number as an unsigned 64-bit value.
            device: i128::from(stat.st_dev).rem_euclid(1_i128 << 64),
            inode: u128::from(stat.st_ino),
            modified: (i128::from(stat.st_mtime), i128::from(stat.st_mtime_nsec)),
            len: u64::try_from(stat.st_size).ok()?,
        })
    }

    /// Check the actual descriptor in case the directory entry changed during open.
    #[cfg(unix)]
    pub(super) fn opened(file: &File) -> Option<Self> {
        let mut stat = MaybeUninit::uninit();
        // SAFETY: The descriptor is live and stat points to writable memory.
        (unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } == 0).then_some(())?;
        // SAFETY: A successful fstat initialized stat.
        Self::new(&unsafe { stat.assume_init() })
    }

    #[cfg(windows)]
    pub(super) fn opened(file: &File) -> Option<Self> {
        let metadata = file.metadata().ok()?;
        let (volume, index) = crate::sys::identity(file).ok()?;
        metadata.is_file().then(|| Self {
            device: i128::from(volume),
            inode: u128::from(index),
            modified: modified(&metadata),
            len: metadata.len(),
        })
    }

    /// Appends keep their identity even when the timestamp and size change.
    pub(super) fn same_file(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }

    pub(super) fn modified_second(self) -> i128 {
        self.modified.0
    }

    fn discovery_rank(self) -> (bool, (i128, i128)) {
        (self.len != 0, self.modified)
    }
}

/// When the file was last written, as whole seconds and nanoseconds since the Unix epoch.
#[cfg(windows)]
fn modified(metadata: &std::fs::Metadata) -> (i128, i128) {
    let Ok(since) = metadata.modified().map(|time| time.duration_since(std::time::UNIX_EPOCH)) else { return (0, 0) };
    since.map_or((0, 0), |since| (i128::from(since.as_secs()), i128::from(since.subsec_nanos())))
}

/// Whole-file snapshots also detect in-place rewrites that preserve size and mtime.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct SnapshotVersion {
    file: FileVersion,
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(unix)]
    mode: u32,
}

impl SnapshotVersion {
    /// An empty file is a leftover, not something a tool saved.
    pub(super) fn len(self) -> u64 {
        self.file.len
    }

    #[cfg(unix)]
    fn regular(metadata: &std::fs::Metadata) -> Option<Self> {
        metadata.is_file().then(|| Self {
            file: FileVersion {
                device: i128::from(metadata.dev()),
                inode: u128::from(metadata.ino()),
                modified: (i128::from(metadata.mtime()), i128::from(metadata.mtime_nsec())),
                len: metadata.len(),
            },
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            mode: metadata.mode(),
        })
    }

    /// The file's identity is known only from an open handle here, so a path alone gives a length
    /// and a modification time.
    #[cfg(windows)]
    fn regular(metadata: &std::fs::Metadata) -> Option<Self> {
        metadata.is_file().then(|| Self {
            file: FileVersion { device: 0, inode: 0, modified: modified(metadata), len: metadata.len() },
        })
    }
}

/// Reads a regular file only when its size fits, including one byte to detect concurrent growth.
pub(super) fn read_small(file: File, version: FileVersion, cap: u64, budget: &mut Budget) -> Option<Vec<u8>> {
    if version.len > cap || budget.remaining <= version.len {
        return None;
    }
    let mut bytes = Vec::new();
    let result = file.take((cap + 1).min(budget.remaining)).read_to_end(&mut bytes);
    budget.debit(bytes.len());
    result.ok()?;
    let len = u64::try_from(bytes.len()).ok()?;
    (len <= cap && len == version.len).then_some(bytes)
}

/// Reads only changed regular files; returns whether access failed, rather than
/// assuming no reading was saved. Removed and non-regular paths are not sources.
pub(super) fn read_cached<T>(
    path: &Path,
    previous: &mut Option<SnapshotVersion>,
    snapshot: &mut Option<T>,
    budget: &mut Budget,
    parse: impl FnOnce(File, FileVersion, &mut Budget) -> Option<T>,
) -> bool {
    let clear = |previous: &mut Option<SnapshotVersion>, snapshot: &mut Option<T>| {
        *previous = None;
        *snapshot = None;
    };
    let version = match path.symlink_metadata() {
        Ok(metadata) => SnapshotVersion::regular(&metadata),
        Err(error) => {
            clear(previous, snapshot);
            return error.kind() != io::ErrorKind::NotFound;
        }
    };
    let Some(version) = version else {
        clear(previous, snapshot);
        return false;
    };
    if *previous == Some(version) {
        return false;
    }
    let opened = fs_ops::open_read(path).and_then(|file| {
        let version = SnapshotVersion::regular(&file.metadata()?);
        Ok(version.map(|v| (file, v)))
    });
    match opened {
        Ok(Some((file, version))) => {
            *previous = Some(version);
            *snapshot = parse(file, version.file, budget);
            false
        }
        Ok(None) => {
            clear(previous, snapshot);
            false
        }
        Err(error) => {
            clear(previous, snapshot);
            error.kind() != io::ErrorKind::NotFound
        }
    }
}

/// Bounds pipe capture even if its writer stops producing output without closing it.
#[cfg(unix)]
pub(super) fn read_pipe(input: impl AsRawFd, max: usize, deadline: Instant) -> Option<Vec<u8>> {
    let fd = input.as_raw_fd();
    let mut poll = crate::sys::pollfd { fd, events: crate::sys::POLLIN, revents: 0 };
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let millis = i32::try_from(left.as_millis() + 1).ok()?;
        // SAFETY: `poll` points to one writable pollfd whose descriptor is live.
        match unsafe { libc::poll(&raw mut poll, 1, millis) } {
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => continue,
            n if n <= 0 => return None,
            _ => {}
        }
        let cap = (max + 1 - bytes.len()).min(buffer.len());
        // SAFETY: This process is the only reader and poll reported readiness.
        // `buffer` is writable for `cap` bytes and `input` keeps `fd` open.
        let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), cap) };
        match read {
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            n if n < 0 => return None,
            0 => return Some(bytes),
            n => {
                let n = usize::try_from(n).ok()?;
                if bytes.len() + n > max {
                    return None;
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
        }
    }
    None
}

/// Pipes are read only where the providers that need them run, which is not yet Windows.
#[cfg(windows)]
pub(super) fn read_pipe(_input: impl AsRawFd, _max: usize, _deadline: Instant) -> Option<Vec<u8>> {
    None
}

/// Restrict each walk to the layout its provider actually uses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tree {
    Codex,
    Claude { since: i64 },
}

impl Tree {
    fn directory(&self, name: &[u8], depth: usize) -> bool {
        match (self, depth) {
            (Self::Codex, 0..=2) => name.len() == if depth == 0 { 4 } else { 2 } && name.iter().all(u8::is_ascii_digit),
            (Self::Claude { .. }, 0 | 1) => true,
            (Self::Claude { .. }, 2) => name == b"subagents",
            _ => false,
        }
    }

    fn file(&self, name: &[u8], depth: usize) -> bool {
        name.ends_with(b".jsonl")
            && match (self, depth) {
                (Self::Codex, 3) => name.starts_with(b"rollout-"),
                (Self::Claude { .. }, 1) => true,
                (Self::Claude { .. }, 3) => name.starts_with(b"agent-"),
                _ => false,
            }
    }

    /// Do not spend Claude's budget on transcripts older than today's activity.
    fn includes(&self, version: FileVersion) -> bool {
        match *self {
            Self::Codex => true,
            Self::Claude { since } => version.modified_second() >= i128::from(since),
        }
    }

    fn limit(&self) -> usize {
        match self {
            Self::Codex => CODEX_FILES,
            Self::Claude { .. } => CLAUDE_FILES,
        }
    }
}

#[cfg(unix)]
struct Entry<'a> {
    name: &'a CStr,
    kind: u8,
}

#[cfg(unix)]
impl Entry<'_> {
    /// A directory's hint can spare a syscall but cannot replace a guarded open.
    fn may_be(&self, kind: u8) -> bool {
        self.kind == kind || self.kind == libc::DT_UNKNOWN
    }
}

/// Own the stream so borrowed entries cannot outlive their directory buffer.
#[cfg(unix)]
struct Dir(NonNull<libc::DIR>);

#[cfg(unix)]
impl Dir {
    fn new(file: File) -> io::Result<Self> {
        let fd = file.as_raw_fd();
        // SAFETY: fd is a live directory descriptor; fdopendir owns it only on success.
        let dir = NonNull::new(unsafe { libc::fdopendir(fd) }).ok_or_else(io::Error::last_os_error)?;
        let _ = file.into_raw_fd();
        Ok(Self(dir))
    }

    fn fd(&self) -> RawFd {
        // SAFETY: The directory stream is valid for self's lifetime.
        unsafe { libc::dirfd(self.0.as_ptr()) }
    }

    fn next(&mut self) -> Option<Entry<'_>> {
        // SAFETY: The stream is live and uniquely borrowed. The result lasts until the next read.
        let entry = NonNull::new(unsafe { libc::readdir(self.0.as_ptr()) })?;
        // SAFETY: readdir supplies the type as part of the live entry.
        let kind = unsafe { (*entry.as_ptr()).d_type };
        // SAFETY: The entry contains the name, but not necessarily the space for the whole struct.
        let name = unsafe { &raw const (*entry.as_ptr()).d_name };
        // SAFETY: readdir supplies a nul-terminated name in the entry.
        Some(Entry { name: unsafe { CStr::from_ptr(name.cast()) }, kind })
    }
}

#[cfg(unix)]
impl Drop for Dir {
    fn drop(&mut self) {
        // SAFETY: This stream is owned by self and is closed exactly once.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

/// Resolve components relative to an already open directory, never following links.
#[cfg(unix)]
fn open_at(dir: RawFd, path: &CStr, flags: libc::c_int) -> io::Result<File> {
    // SAFETY: dir is a live directory descriptor, path is nul-terminated and no mode is needed.
    let fd = unsafe { libc::openat(dir, path.as_ptr(), READ_FLAGS | libc::O_RDONLY | flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: A nonnegative result is a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Compare entries before opening files, without trusting a link's target.
#[cfg(unix)]
fn version_at(dir: RawFd, path: &CStr) -> Option<FileVersion> {
    let mut stat = MaybeUninit::uninit();
    // SAFETY: dir is live, path is nul-terminated and stat points to writable memory.
    (unsafe { libc::fstatat(dir, path.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } == 0).then_some(())?;
    // SAFETY: A successful fstatat initialized stat.
    FileVersion::new(&unsafe { stat.assume_init() })
}

/// A candidate is not known to be the same file until it is opened again.
struct Candidate {
    path: PathBuf,
    version: FileVersion,
}

/// Cap both the number examined and the number held, independently.
struct Scan {
    remaining: usize,
    files: Vec<Candidate>,
    partial: bool,
    inaccessible: bool,
}

#[cfg(windows)]
mod windows;

#[cfg(unix)]
impl Scan {
    fn directory(&mut self, mut dir: Dir, tree: &Tree, path: &mut PathBuf, depth: usize) {
        let fd = dir.fd();
        if depth < 3 && matches!(tree, Tree::Codex) {
            for name in codex_folders(&mut dir, tree, depth) {
                if !self.descend(fd, &name, tree, path, depth) {
                    break;
                }
            }
            return;
        }
        while let Some(entry) = dir.next() {
            let bytes = entry.name.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if entry.may_be(libc::DT_REG)
                && tree.file(bytes, depth)
                && let Some(version) = version_at(fd, entry.name)
                && tree.includes(version)
            {
                if !self.admit() {
                    break;
                }
                self.keep(tree.limit(), path, OsStr::from_bytes(bytes), version);
            }
            if entry.may_be(libc::DT_DIR)
                && tree.directory(bytes, depth)
                && !self.descend(fd, entry.name, tree, path, depth)
            {
                break;
            }
        }
    }

    fn descend(&mut self, fd: RawFd, name: &CStr, tree: &Tree, path: &mut PathBuf, depth: usize) -> bool {
        match open_at(fd, name, libc::O_DIRECTORY).and_then(Dir::new) {
            Ok(child) => {
                if !self.admit() {
                    return false;
                }
                path.push(OsStr::from_bytes(name.to_bytes()));
                self.directory(child, tree, path, depth + 1);
                path.pop();
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                self.inaccessible = true;
                self.partial = true;
            }
            Err(_) => {}
        }
        true
    }
}

impl Scan {
    /// Report a partial result only when another qualifying entry would exceed the budget.
    fn admit(&mut self) -> bool {
        if self.remaining == 0 {
            self.partial = true;
            return false;
        }
        self.remaining -= 1;
        true
    }

    /// Keep the newest transcripts, with empty files as possible append targets only.
    fn keep(&mut self, limit: usize, dir: &Path, name: &OsStr, version: FileVersion) {
        let rank = version.discovery_rank();
        if self.files.len() == limit {
            self.partial |= version.len != 0 && self.files.last().is_some_and(|old| old.version.len != 0);
            if self.files.last().is_some_and(|old| old.version.discovery_rank() > rank) {
                return;
            }
        }
        let path = dir.join(name);
        let at = self.files.partition_point(|old| {
            let prior = old.version.discovery_rank();
            prior > rank || (prior == rank && old.path < path)
        });
        if at < limit {
            if self.files.len() == limit {
                self.files.pop();
            }
            self.files.insert(at, Candidate { path, version });
        }
    }
}

/// Visits newer days first, so a full walk's budget runs out in older folders.
#[cfg(unix)]
fn codex_folders(dir: &mut Dir, tree: &Tree, depth: usize) -> Vec<CString> {
    let mut names = Vec::new();
    while let Some(entry) = dir.next() {
        if entry.may_be(libc::DT_DIR) && tree.directory(entry.name.to_bytes(), depth) {
            names.push(entry.name.to_owned());
        }
    }
    names.sort_unstable_by(|a, b| b.cmp(a));
    names
}

/// Discover a bounded set of paths, then check their versions without walking the tree.
pub(super) struct Sessions<T> {
    last: Option<Discovery>,
    pending: bool,
    pub entries: Vec<SessionFile<T>>,
    pub partial: bool,
    pub inaccessible: bool,
}

struct Discovery {
    root: (u64, u64),
    tree: Tree,
    at: Instant,
}

pub(super) struct SessionFile<T> {
    pub path: PathBuf,
    pub version: FileVersion,
    pub cached: Option<T>,
}

impl<T> Default for Sessions<T> {
    fn default() -> Self {
        Self { last: None, pending: false, entries: Vec::new(), partial: false, inaccessible: false }
    }
}

impl<T> Sessions<T> {
    pub(super) fn changed(&mut self) {
        self.pending = true;
    }

    pub(super) fn pending(&self) -> bool {
        self.pending
    }

    pub(super) fn cached(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().filter_map(|file| file.cached.as_ref())
    }

    pub(super) fn refresh(
        &mut self,
        path: Option<&Path>,
        tree: Tree,
        clock: Instant,
        force: bool,
    ) -> io::Result<(SessionRoot, bool)> {
        let result = self.check(path, tree, clock, force);
        if result.is_err() {
            self.last = None;
            self.pending = false;
            self.entries.clear();
            self.partial = false;
            self.inaccessible = false;
        }
        result
    }

    /// Returns the root for later reads and whether the selected paths changed.
    fn check(
        &mut self,
        path: Option<&Path>,
        tree: Tree,
        clock: Instant,
        force: bool,
    ) -> io::Result<(SessionRoot, bool)> {
        let root = SessionRoot(fs_ops::open_dir(path.ok_or(io::ErrorKind::NotFound)?)?);
        let identity = crate::sys::identity(&root.0)?;
        let discover = force
            || self.last.as_ref().is_none_or(|last| {
                let elapsed = clock.saturating_duration_since(last.at);
                last.root != identity
                    || last.tree != tree
                    || elapsed >= DISCOVERY_IDLE
                    || self.pending && elapsed >= DISCOVERY_REST
            });
        let count = self.entries.len();
        if !discover {
            self.entries.retain_mut(|file| {
                let Some(version) = root.version(&file.path).filter(|&version| tree.includes(version)) else {
                    return false;
                };
                file.version = version;
                true
            });
            let changed = self.entries.len() != count;
            self.pending |= changed;
            return Ok((root, changed));
        }

        let scan = root.scan(tree, MAX_ENTRIES)?;
        self.last = Some(Discovery { root: identity, tree, at: clock });
        self.pending = false;
        self.partial = scan.partial;
        self.inaccessible = scan.inaccessible;
        if scan.files.len() == count && scan.files.iter().zip(&self.entries).all(|(new, old)| new.path == old.path) {
            for (new, old) in scan.files.into_iter().zip(&mut self.entries) {
                old.version = new.version;
            }
            return Ok((root, false));
        }

        self.entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        let mut changed = scan.files.len() != count;
        self.entries = scan
            .files
            .into_iter()
            .map(|Candidate { path, version }| {
                let cached = match self.entries.binary_search_by(|file| file.path.cmp(&path)) {
                    Ok(index) => self.entries[index].cached.take(),
                    Err(_) => {
                        changed = true;
                        None
                    }
                };
                SessionFile { path, version, cached }
            })
            .collect();
        Ok((root, changed))
    }
}

/// Anchor all checks and reads to the same root, refusing links in every component.
pub(super) struct SessionRoot(File);

#[cfg(windows)]
impl SessionRoot {
    fn scan(&self, tree: Tree, remaining: usize) -> io::Result<Scan> {
        let mut scan = Scan { remaining, files: Vec::new(), partial: false, inaccessible: false };
        scan.directory(&self.0, &tree, &mut PathBuf::new(), 0);
        Ok(scan)
    }

    fn version(&self, path: &Path) -> Option<FileVersion> {
        windows::version(&self.0, path)
    }

    pub(super) fn open(&self, path: &Path) -> Option<(File, FileVersion)> {
        windows::open(&self.0, path)
    }
}

#[cfg(unix)]
impl SessionRoot {
    fn scan(&self, tree: Tree, remaining: usize) -> io::Result<Scan> {
        let dir = open_at(self.0.as_raw_fd(), c".", libc::O_DIRECTORY).and_then(Dir::new)?;
        let mut scan = Scan { remaining, files: Vec::new(), partial: false, inaccessible: false };
        scan.directory(dir, &tree, &mut PathBuf::new(), 0);
        Ok(scan)
    }

    fn version(&self, path: &Path) -> Option<FileVersion> {
        self.entry(path, version_at)
    }

    pub(super) fn open(&self, path: &Path) -> Option<(File, FileVersion)> {
        self.entry(path, |fd, name| {
            let file = open_at(fd, name, 0).ok()?;
            let version = FileVersion::opened(&file)?;
            Some((file, version))
        })
    }

    fn entry<T>(&self, path: &Path, read: impl FnOnce(RawFd, &CStr) -> Option<T>) -> Option<T> {
        let mut parts = path.iter().peekable();
        let mut folder = None::<File>;
        let mut name = [0; libc::PATH_MAX as usize];
        while let Some(part) = parts.next() {
            let fd = folder.as_ref().unwrap_or(&self.0).as_raw_fd();
            let bytes = part.as_bytes();
            let name = name.get_mut(..=bytes.len())?;
            name[..bytes.len()].copy_from_slice(bytes);
            name[bytes.len()] = 0;
            let name = CStr::from_bytes_with_nul(name).ok()?;
            if parts.peek().is_none() {
                return read(fd, name);
            }
            folder = Some(open_at(fd, name, libc::O_DIRECTORY).ok()?);
        }
        None
    }
}

/// A shared allowance bounds all reads a provider can do during one check.
pub(super) struct Budget {
    pub remaining: u64,
    #[cfg(test)]
    pub read: u64,
}

impl Budget {
    pub(super) fn new(remaining: u64) -> Self {
        Self {
            remaining,
            #[cfg(test)]
            read: 0,
        }
    }

    pub(super) fn debit(&mut self, count: usize) {
        let bytes = u64::try_from(count).unwrap_or(u64::MAX);
        self.remaining -= bytes;
        #[cfg(test)]
        {
            self.read += bytes;
        }
    }

    /// Keep partial reads and interruptions inside the same byte allowance.
    fn read_at(&mut self, file: &File, data: &mut [u8], offset: u64) -> io::Result<usize> {
        let cap = data.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        loop {
            #[cfg(unix)]
            let read = std::os::unix::fs::FileExt::read_at(file, &mut data[..cap], offset);
            #[cfg(windows)]
            let read = std::os::windows::fs::FileExt::seek_read(file, &mut data[..cap], offset);
            match read {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                result => {
                    let read = result?;
                    self.debit(read);
                    return Ok(read);
                }
            }
        }
    }
}

/// Finds the last complete report and carries the final unterminated line into the stream.
pub(super) fn tail<T>(
    file: &File,
    len: u64,
    max: u64,
    budget: &mut Budget,
    mut find: impl FnMut(&[u8]) -> Option<T>,
) -> io::Result<(Option<T>, Stream)> {
    let stop = len.saturating_sub(max.min(budget.remaining));
    let mut reverse = Reverse::new(len);
    let mut stream = Stream::resume(len);
    while reverse.offset > stop {
        if let ControlFlow::Break(found) = reverse.block(file, stop, budget, |line, at_end| {
            if at_end {
                stream.remember_tail(line);
                return ControlFlow::Continue(());
            }
            line.and_then(&mut find).map_or(ControlFlow::Continue(()), ControlFlow::Break)
        })? {
            return Ok((Some(found), stream));
        }
    }
    if len != 0 && reverse.at_end {
        // The last line crosses the read boundary. Ignore its unseen prefix.
        stream.remember_tail(None);
    }
    Ok((None, stream))
}

#[derive(Default)]
/// Carry a split record across blocks without accumulating an unbounded line.
struct Line {
    bytes: Vec<u8>,
    skipping: bool,
}

impl Line {
    fn clear(&mut self) {
        self.bytes.clear();
        self.skipping = false;
    }

    fn push(&mut self, bytes: impl ExactSizeIterator<Item = u8>) {
        if !self.skipping {
            if self.bytes.len() + bytes.len() > MAX_LINE {
                self.bytes.clear();
                self.skipping = true;
            } else {
                self.bytes.extend(bytes);
            }
        }
    }
}

/// Walks complete lines from the end, keeping the line split across blocks.
pub(super) struct Reverse {
    pub offset: u64,
    line: Line,
    at_end: bool,
}

impl Reverse {
    pub(super) fn new(len: u64) -> Self {
        Self { offset: len, line: Line::default(), at_end: true }
    }

    /// Keep scanning after an oversized record, but do not pass it to the parser.
    pub(super) fn block<T>(
        &mut self,
        file: &File,
        stop: u64,
        budget: &mut Budget,
        mut found: impl FnMut(Option<&[u8]>, bool) -> ControlFlow<T>,
    ) -> io::Result<ControlFlow<T>> {
        let mut buf = [0; BLOCK];
        let n = usize::try_from((self.offset - stop).min(BLOCK as u64).min(budget.remaining)).unwrap_or(BLOCK);
        let offset = self.offset - n as u64;
        let mut read = 0;
        while read < n {
            let count = budget.read_at(file, &mut buf[read..n], offset + read as u64)?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            read += count;
        }
        self.offset = offset;
        let mut parts = buf[..n].rsplit(|b| *b == b'\n').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() && offset != 0 {
                self.line.push(part.iter().rev().copied());
                break;
            }
            let line = if self.line.bytes.is_empty() && !self.line.skipping {
                Some(part)
            } else {
                self.line.push(part.iter().rev().copied());
                self.line.bytes.reverse();
                (!self.line.skipping).then_some(self.line.bytes.as_slice())
            };
            if let ControlFlow::Break(value) = found(line, self.at_end) {
                return Ok(ControlFlow::Break(value));
            }
            self.at_end = false;
            self.line.clear();
        }
        Ok(ControlFlow::Continue(()))
    }
}

/// Keep the current append position and any incomplete trailing record.
pub(super) struct Stream {
    pub offset: u64,
    line: Line,
}

impl Stream {
    pub(super) fn resume(offset: u64) -> Self {
        Self { offset, line: Line::default() }
    }

    /// The first forward read must finish, not re-read, the last reverse record.
    pub(super) fn remember_tail(&mut self, line: Option<&[u8]>) {
        if let Some(line) = line {
            self.line.push(line.iter().copied());
        } else {
            self.line.skipping = true;
        }
    }

    /// A changed byte at the old end means the suffix may no longer be line-aligned.
    pub(super) fn can_resume(&self, file: &File, budget: &mut Budget) -> io::Result<bool> {
        if self.offset == 0 || self.line.skipping {
            return Ok(true);
        }
        let expected = self.tail().last().copied().unwrap_or(b'\n');
        let mut byte = [0];
        Ok(budget.read_at(file, &mut byte, self.offset - 1)? == 1 && byte[0] == expected)
    }

    /// Before losing the old EOF in a bounded tail read, check whether an
    /// append actually ended its provisional JSON record.
    pub(super) fn tail_terminated(&self, file: &File, len: u64, budget: &mut Budget) -> io::Result<bool> {
        let mut suffix = [0; 64];
        let cap = usize::try_from((len - self.offset).min(suffix.len() as u64)).unwrap_or(suffix.len());
        let n = budget.read_at(file, &mut suffix[..cap], self.offset)?;
        let next = suffix[..n].iter().position(|&b| !matches!(b, b' ' | b'\t' | b'\r'));
        Ok(next.is_some_and(|i| suffix[i] == b'\n' && self.tail().len() + i <= MAX_LINE))
    }

    /// Stop at the observed size; bytes appended during the read belong to the next check.
    pub(super) fn read(
        &mut self,
        file: &File,
        len: u64,
        budget: &mut Budget,
        mut found: impl FnMut(&[u8]),
    ) -> io::Result<()> {
        let mut buf = [0; BLOCK];
        while self.offset < len && budget.remaining > 0 {
            let cap = usize::try_from((len - self.offset).min(BLOCK as u64)).unwrap_or(BLOCK);
            let n = budget.read_at(file, &mut buf[..cap], self.offset)?;
            if n == 0 {
                break;
            }
            self.offset += n as u64;
            self.lines(&buf[..n], &mut found);
        }
        if self.line.bytes.is_empty() {
            self.line.bytes = Vec::new();
        }
        Ok(())
    }

    fn lines(&mut self, bytes: &[u8], found: &mut impl FnMut(&[u8])) {
        let mut parts = bytes.split(|b| *b == b'\n').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                self.line.push(part.iter().copied());
                break;
            }
            if self.line.bytes.is_empty() && !self.line.skipping {
                found(part);
            } else {
                self.line.push(part.iter().copied());
                if !self.line.skipping {
                    found(&self.line.bytes);
                }
            }
            self.line.clear();
        }
    }

    /// A complete JSON record may already be usable before its newline arrives.
    pub(super) fn tail(&self) -> &[u8] {
        &self.line.bytes
    }
}

/// A cheap prefilter keeps ordinary transcript records out of the JSON parser.
pub(super) fn has_marker(line: &[u8], marker: &[u8]) -> bool {
    line.split(|b| *b == b'"').skip(1).any(|part| part == marker)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[cfg(unix)]
    #[test]
    fn a_snapshot_distinguishes_permission_changes_with_the_same_change_time() {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = Fixture::folder();
        fixture.write("limits.json", "{}");
        let path = fixture.root.join("limits.json");
        let version = |mode| {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            SnapshotVersion::regular(&path.symlink_metadata().unwrap()).unwrap()
        };
        let before = version(0o600);
        let mut after = version(0o400);
        after.changed = before.changed;
        assert!(before != after);
    }

    #[cfg(unix)]
    #[test]
    fn the_cache_reloads_a_same_size_rewrite_with_a_restored_mtime() {
        use std::time::{Duration, UNIX_EPOCH};

        let f = Fixture::folder();
        let path = f.root.join("limits.json");
        let modified = UNIX_EPOCH + Duration::from_secs(1);
        f.write("limits.json", "false");
        File::open(&path).unwrap().set_modified(modified).unwrap();
        let (mut previous, mut snapshot) = (None, None);
        let mut budget = Budget::new(32);
        let parse = |file, version, budget: &mut Budget| read_small(file, version, 16, budget);
        read_cached(&path, &mut previous, &mut snapshot, &mut budget, parse);
        assert_eq!(snapshot.as_deref(), Some(b"false".as_slice()));

        let changed = || {
            let metadata = path.metadata().unwrap();
            (metadata.ctime(), metadata.ctime_nsec())
        };
        let before = changed();
        f.write("limits.json", "true ");
        let file = File::open(&path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            file.set_modified(modified).unwrap();
            if changed() != before {
                break;
            }
            assert!(Instant::now() < deadline, "inode change time did not advance");
            std::thread::sleep(Duration::from_millis(1));
        }
        read_cached(&path, &mut previous, &mut snapshot, &mut budget, parse);
        assert_eq!(snapshot.as_deref(), Some(b"true ".as_slice()));
    }

    #[cfg(unix)]
    #[test]
    fn the_walk_budget_is_spent_in_the_newest_year_month_and_day_first() {
        let f = Fixture::folder();
        let newest = "2026/11/19/rollout-new.jsonl";
        for path in [
            "2024/12/26/rollout-old-year.jsonl",
            "2026/10/26/rollout-old-month.jsonl",
            "2026/11/18/rollout-old-day.jsonl",
            newest,
        ] {
            f.write(path, "{}\n");
        }
        let sessions = SessionRoot(fs_ops::open_dir(&f.root).unwrap()).scan(Tree::Codex, 4).unwrap();
        assert!(sessions.partial);
        assert_eq!(sessions.files.len(), 1);
        assert_eq!(sessions.files[0].path, Path::new(newest));
    }

    #[cfg(unix)]
    #[test]
    fn compressed_and_irrelevant_paths_do_not_spend_the_walk_budget() {
        let f = Fixture::folder();
        let plain = "2026/11/19/rollout-plain.jsonl";
        for i in 0..12 {
            f.write(&format!("2026/11/19/rollout-{i}.jsonl.zst"), "not read");
            f.write(&format!("unrelated-{i}"), "not read");
        }
        f.write(plain, "{}\n");
        let sessions = SessionRoot(fs_ops::open_dir(&f.root).unwrap()).scan(Tree::Codex, 4).unwrap();
        assert!(!sessions.partial);
        assert_eq!(sessions.files.len(), 1);
        assert_eq!(sessions.files[0].path, Path::new(plain));
    }
}

#[cfg(windows)]
pub(super) fn probe_list(root: &File) -> String {
    windows::probe_list(root)
}

#[cfg(windows)]
pub(super) fn probe_open(root: &File) -> String {
    windows::probe_open(root)
}
