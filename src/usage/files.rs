//! Walks the session trees without following links and reads within a refresh budget.

use std::ffi::{CStr, CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::ops::ControlFlow;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt as _, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::Instant;

pub(super) const CODEX_FILES: usize = 4;
const CLAUDE_FILES: usize = 128;
const MAX_ENTRIES: usize = 32_768;
const MAX_LINE: usize = 2 * 1024 * 1024;
const BLOCK: usize = 32 * 1024;
const OPEN_FLAGS: libc::c_int = libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Mark {
    device: i128,
    inode: u128,
    modified: (i128, i128),
    pub len: u64,
}

impl Mark {
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

    pub(super) fn metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: i128::from(metadata.dev()),
            inode: u128::from(metadata.ino()),
            modified: (i128::from(metadata.mtime()), i128::from(metadata.mtime_nsec())),
            len: metadata.len(),
        }
    }

    pub(super) fn opened(file: &File) -> Option<Self> {
        let mut stat = MaybeUninit::uninit();
        // SAFETY: The descriptor is live and stat points to writable memory.
        (unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } == 0).then_some(())?;
        // SAFETY: A successful fstat initialized stat.
        Self::new(&unsafe { stat.assume_init() })
    }

    pub(super) fn same_file(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

pub(super) fn open_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(OPEN_FLAGS).open(path)
}

pub(super) fn open_directory(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(OPEN_FLAGS | libc::O_DIRECTORY).open(path)
}

/// Reads a regular file only when its size fits, including one byte to detect concurrent growth.
pub(super) fn read_small(file: File, mark: Mark, cap: u64, budget: &mut Budget) -> Option<Vec<u8>> {
    if mark.len > cap || budget.remaining <= mark.len {
        return None;
    }
    let mut bytes = Vec::new();
    let result = file.take((cap + 1).min(budget.remaining)).read_to_end(&mut bytes);
    budget.debit(bytes.len());
    result.ok()?;
    let len = u64::try_from(bytes.len()).ok()?;
    (len <= cap && len == mark.len).then_some(bytes)
}

/// Reads only changed regular files; a removed or bad file stops being a source.
pub(super) fn read_cached<T>(
    path: &Path,
    previous: &mut Option<Mark>,
    snapshot: &mut Option<T>,
    budget: &mut Budget,
    parse: impl FnOnce(File, Mark, &mut Budget) -> Option<T>,
) {
    let Some(mark) = std::fs::symlink_metadata(path).ok().filter(|m| m.is_file()) else {
        *previous = None;
        *snapshot = None;
        return;
    };
    if *previous == Some(Mark::metadata(&mark)) {
        return;
    }
    *snapshot = open_file(path).ok().and_then(|file| {
        let mark = Mark::opened(&file)?;
        *previous = Some(mark);
        parse(file, mark, budget)
    });
}

/// Bounds pipe capture even if its writer stops producing output without closing it.
pub(super) fn read_pipe(input: impl AsFd, max: usize, deadline: Instant) -> Option<Vec<u8>> {
    let fd = input.as_fd();
    let mut poll = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
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
        // `buffer` is writable for `cap` bytes and `fd` remains borrowed above.
        let read = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), cap) };
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

    fn includes(&self, mark: Mark) -> bool {
        match *self {
            Self::Codex => true,
            Self::Claude { since } => mark.modified.0 >= i128::from(since),
        }
    }

    fn limit(&self) -> usize {
        match self {
            Self::Codex => CODEX_FILES,
            Self::Claude { .. } => CLAUDE_FILES,
        }
    }
}

struct Dir(NonNull<libc::DIR>);

impl Dir {
    fn new(file: File) -> Option<Self> {
        let fd = file.as_raw_fd();
        // SAFETY: fd is a live directory descriptor; fdopendir owns it only on success.
        let dir = Self(NonNull::new(unsafe { libc::fdopendir(fd) })?);
        let _ = file.into_raw_fd();
        Some(dir)
    }

    fn fd(&self) -> RawFd {
        // SAFETY: The directory stream is valid for self's lifetime.
        unsafe { libc::dirfd(self.0.as_ptr()) }
    }

    fn next(&mut self) -> Option<&CStr> {
        // SAFETY: The stream is live and uniquely borrowed. The result lasts until the next read.
        let entry = NonNull::new(unsafe { libc::readdir(self.0.as_ptr()) })?;
        // SAFETY: entry points to a live directory entry. Do not form a reference to the
        // whole struct: the space actually used for the name varies by entry.
        let name = unsafe { &raw const (*entry.as_ptr()).d_name };
        // SAFETY: readdir supplies a nul-terminated name in the entry.
        Some(unsafe { CStr::from_ptr(name.cast()) })
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        // SAFETY: This stream is owned by self and is closed exactly once.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

fn open_at(dir: RawFd, path: &CStr, flags: libc::c_int) -> Option<File> {
    // SAFETY: dir is a live directory descriptor, path is nul-terminated and no mode is needed.
    let fd = unsafe { libc::openat(dir, path.as_ptr(), OPEN_FLAGS | libc::O_RDONLY | flags) };
    // SAFETY: A nonnegative result is a newly owned descriptor.
    (fd >= 0).then(|| unsafe { File::from_raw_fd(fd) })
}

fn mark_at(dir: RawFd, path: &CStr) -> Option<Mark> {
    let mut stat = MaybeUninit::uninit();
    // SAFETY: dir is live, path is nul-terminated and stat points to writable memory.
    (unsafe { libc::fstatat(dir, path.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } == 0).then_some(())?;
    // SAFETY: A successful fstatat initialized stat.
    Mark::new(&unsafe { stat.assume_init() })
}

pub(super) struct Candidate {
    pub path: PathBuf,
    pub mark: Mark,
}

struct Scan {
    remaining: usize,
    files: Vec<Candidate>,
    limited: bool,
}

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
        while let Some(name) = dir.next() {
            let bytes = name.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if tree.file(bytes, depth)
                && let Some(mark) = mark_at(fd, name)
                && tree.includes(mark)
            {
                if !self.admit() {
                    break;
                }
                self.keep(tree.limit(), path, bytes, mark);
            }
            if tree.directory(bytes, depth) && !self.descend(fd, name, tree, path, depth) {
                break;
            }
        }
    }

    fn descend(&mut self, fd: RawFd, name: &CStr, tree: &Tree, path: &mut PathBuf, depth: usize) -> bool {
        if let Some(child) = open_at(fd, name, libc::O_DIRECTORY).and_then(Dir::new) {
            if !self.admit() {
                return false;
            }
            path.push(OsStr::from_bytes(name.to_bytes()));
            self.directory(child, tree, path, depth + 1);
            path.pop();
        }
        true
    }

    fn admit(&mut self) -> bool {
        if self.remaining == 0 {
            self.limited = true;
            return false;
        }
        self.remaining -= 1;
        true
    }

    /// Keeps just the newest few files as the directory is walked.
    fn keep(&mut self, limit: usize, dir: &Path, name: &[u8], mark: Mark) {
        if self.files.len() == limit {
            self.limited = true;
            if self.files.last().is_some_and(|old| old.mark.modified > mark.modified) {
                return;
            }
        }
        let path = dir.join(OsStr::from_bytes(name));
        let at = self.files.partition_point(|old| {
            old.mark.modified > mark.modified || (old.mark.modified == mark.modified && old.path < path)
        });
        if at < limit {
            if self.files.len() == limit {
                self.files.pop();
            }
            self.files.insert(at, Candidate { path, mark });
        }
    }
}

/// Visits newer days first, so a full walk's budget runs out in older folders.
fn codex_folders(dir: &mut Dir, tree: &Tree, depth: usize) -> Vec<CString> {
    let mut names = Vec::new();
    while let Some(name) = dir.next() {
        if tree.directory(name.to_bytes(), depth) {
            names.push(name.to_owned());
        }
    }
    names.sort_unstable_by(|a, b| b.cmp(a));
    names
}

pub(super) struct Sessions {
    root: File,
    pub files: Vec<Candidate>,
    pub limited: bool,
}

impl Sessions {
    pub(super) fn scan(root: &Path, tree: Tree) -> io::Result<Self> {
        Self::scan_with_limit(root, tree, MAX_ENTRIES)
    }

    fn scan_with_limit(root: &Path, tree: Tree, remaining: usize) -> io::Result<Self> {
        let root = open_directory(root)?;
        // Open a separate stream: a duplicate descriptor would share the directory cursor.
        let dir = open_at(root.as_raw_fd(), c".", libc::O_DIRECTORY)
            .and_then(Dir::new)
            .ok_or_else(|| io::Error::other("cannot open session directory stream"))?;
        let mut scan = Scan { remaining, files: Vec::new(), limited: false };
        scan.directory(dir, &tree, &mut PathBuf::new(), 0);
        Ok(Self { root, files: scan.files, limited: scan.limited })
    }

    pub(super) fn open(&self, path: &Path) -> Option<(File, Mark)> {
        let mut parts = path.iter().peekable();
        let mut folder = None::<File>;
        while let Some(part) = parts.next() {
            let fd = folder.as_ref().unwrap_or(&self.root).as_raw_fd();
            let name = CString::new(part.as_bytes()).ok()?;
            if parts.peek().is_none() {
                let file = open_at(fd, &name, 0)?;
                let mark = Mark::opened(&file)?;
                return Some((file, mark));
            }
            folder = Some(open_at(fd, &name, libc::O_DIRECTORY)?);
        }
        None
    }
}

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

    fn read_at(&mut self, file: &File, data: &mut [u8], offset: u64) -> io::Result<usize> {
        let cap = data.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        loop {
            match file.read_at(&mut data[..cap], offset) {
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

/// Reads backwards in blocks, touching only enough of the tail to find a record.
pub(super) fn tail<T>(
    file: &File,
    len: u64,
    max: u64,
    budget: &mut Budget,
    find: impl Fn(&[u8]) -> Option<T>,
) -> io::Result<(Option<T>, bool)> {
    let stop = len.saturating_sub(max.min(budget.remaining));
    let mut reverse = Reverse::new(len);
    while reverse.offset > stop {
        match reverse.block(file, stop, budget, |line, _| {
            line.and_then(&find).map_or(ControlFlow::Continue(()), ControlFlow::Break)
        }) {
            Ok(ControlFlow::Break(found)) => return Ok((Some(found), false)),
            Ok(ControlFlow::Continue(())) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok((None, true)),
            Err(error) => return Err(error),
        }
    }
    Ok((None, stop != 0))
}

#[derive(Default)]
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

pub(super) struct Stream {
    pub offset: u64,
    line: Line,
}

impl Stream {
    pub(super) fn resume(offset: u64) -> Self {
        Self { offset, line: Line::default() }
    }

    pub(super) fn remember_tail(&mut self, line: Option<&[u8]>) {
        if let Some(line) = line {
            self.line.push(line.iter().copied());
        } else {
            self.line.skipping = true;
        }
    }

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

    pub(super) fn tail(&self) -> &[u8] {
        &self.line.bytes
    }
}

pub(super) fn has_marker(line: &[u8], marker: &[u8]) -> bool {
    line.split(|b| *b == b'"').skip(1).any(|part| part == marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

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
        let sessions = Sessions::scan_with_limit(&f.root, Tree::Codex, 4).unwrap();
        assert!(sessions.limited);
        assert_eq!(sessions.files.len(), 1);
        assert_eq!(sessions.files[0].path, Path::new(newest));
    }

    #[test]
    fn compressed_and_irrelevant_paths_do_not_spend_the_walk_budget() {
        let f = Fixture::folder();
        let plain = "2026/11/19/rollout-plain.jsonl";
        for i in 0..12 {
            f.write(&format!("2026/11/19/rollout-{i}.jsonl.zst"), "not read");
            f.write(&format!("unrelated-{i}"), "not read");
        }
        f.write(plain, "{}\n");
        let sessions = Sessions::scan_with_limit(&f.root, Tree::Codex, 4).unwrap();
        assert!(!sessions.limited);
        assert_eq!(sessions.files.len(), 1);
        assert_eq!(sessions.files[0].path, Path::new(plain));
    }
}
