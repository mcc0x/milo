//! Windows form of the read-only query. Windows has no sqlite3 program, and the SQLite that ships
//! with it, System32\winsqlite3.dll, ignores `readonly_shm`: opened in place, it would write its
//! shared-memory file and make missing sidecars. So the database is never opened in place. Under
//! SQLite's own shared lock, its file and write-ahead log are read through handles that only read,
//! the committed frames are applied to a private copy in memory, and that copy is queried. Nothing
//! beside the database is opened for writing, made or changed. A copy that the files show was
//! disturbed is taken again, and after a few tries the database is left unread.
//!
//! The answer is written as the sqlite3 shell's `-json` output, so the readers parse one format.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::fs::File;
use std::io;
use std::os::windows::fs::FileExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;
use std::time::Instant;

use crate::fs_ops;

type Db = c_void;
type Stmt = c_void;
type Progress = unsafe extern "system" fn(*mut c_void) -> c_int;

const OK: c_int = 0;
const ROW: c_int = 100;
const DONE: c_int = 101;
const INTEGER: c_int = 1;
const FLOAT: c_int = 2;
const TEXT: c_int = 3;
const BLOB: c_int = 4;
const OPEN_READWRITE: c_int = 0x2;
const OPEN_CREATE: c_int = 0x4;
const DESERIALIZE_READONLY: c_uint = 4;
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
/// Virtual machine steps between looks at the clock.
const STEPS: c_int = 1000;
/// The most a database and its log may hold together before it is left unread.
pub(super) const MAX_SNAPSHOT: u64 = 64 << 20;
/// A writer that is busy during every copy leaves the database unread.
const ATTEMPTS: usize = 3;
const RETRY: std::time::Duration = std::time::Duration::from_millis(10);
const WAL_HEADER: usize = 32;
const FRAME_HEADER: usize = 24;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> Option<unsafe extern "system" fn() -> isize>;
    fn GetFileInformationByHandleEx(file: *mut c_void, class: c_int, info: *mut c_void, size: u32) -> i32;
    fn LockFileEx(
        file: *mut c_void,
        flags: u32,
        reserved: u32,
        low: u32,
        high: u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn UnlockFileEx(file: *mut c_void, reserved: u32, low: u32, high: u32, overlapped: *mut Overlapped) -> i32;
}

type Open = unsafe extern "system" fn(*const c_char, *mut *mut Db, c_int, *const c_char) -> c_int;
type Close = unsafe extern "system" fn(*mut Db) -> c_int;
type Deserialize = unsafe extern "system" fn(*mut Db, *const c_char, *mut u8, i64, i64, c_uint) -> c_int;
type ProgressHandler = unsafe extern "system" fn(*mut Db, c_int, Option<Progress>, *mut c_void);
type Prepare = unsafe extern "system" fn(*mut Db, *const c_char, c_int, *mut *mut Stmt, *mut *const c_char) -> c_int;
type Step = unsafe extern "system" fn(*mut Stmt) -> c_int;
type Count = unsafe extern "system" fn(*mut Stmt) -> c_int;
type Name = unsafe extern "system" fn(*mut Stmt, c_int) -> *const c_char;
type Kind = unsafe extern "system" fn(*mut Stmt, c_int) -> c_int;
type Integer = unsafe extern "system" fn(*mut Stmt, c_int) -> i64;
type Float = unsafe extern "system" fn(*mut Stmt, c_int) -> f64;
type Blob = unsafe extern "system" fn(*mut Stmt, c_int) -> *const c_void;

/// The parts of SQLite's C interface a query uses.
struct Sqlite {
    open: Open,
    close: Close,
    deserialize: Deserialize,
    progress_handler: ProgressHandler,
    prepare: Prepare,
    step: Step,
    finalize: Step,
    column_count: Count,
    column_name: Name,
    column_type: Kind,
    column_int64: Integer,
    column_double: Float,
    column_text: Blob,
    column_blob: Blob,
    column_bytes: Kind,
}

/// Only System32 is searched, so a winsqlite3.dll left beside milo.exe or in the working folder
/// is never loaded. A Windows without it reads no databases rather than failing to start.
fn sqlite() -> Option<&'static Sqlite> {
    static SQLITE: OnceLock<Option<Sqlite>> = OnceLock::new();
    SQLITE.get_or_init(load).as_ref()
}

fn load() -> Option<Sqlite> {
    let name: Vec<u16> = "winsqlite3.dll".encode_utf16().chain([0]).collect();
    // SAFETY: The name is NUL-terminated, and no file handle is passed.
    let module = unsafe { LoadLibraryExW(name.as_ptr(), ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
    if module.is_null() {
        return None;
    }
    macro_rules! symbol {
        ($name:literal as $kind:ty) => {{
            // SAFETY: The module is never freed, and the name is NUL-terminated.
            let found = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr().cast()) }?;
            // SAFETY: The export is SQLite's C function of that name, whose documented signature $kind declares.
            unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $kind>(found) }
        }};
    }
    Some(Sqlite {
        open: symbol!("sqlite3_open_v2" as Open),
        close: symbol!("sqlite3_close" as Close),
        deserialize: symbol!("sqlite3_deserialize" as Deserialize),
        progress_handler: symbol!("sqlite3_progress_handler" as ProgressHandler),
        prepare: symbol!("sqlite3_prepare_v2" as Prepare),
        step: symbol!("sqlite3_step" as Step),
        finalize: symbol!("sqlite3_finalize" as Step),
        column_count: symbol!("sqlite3_column_count" as Count),
        column_name: symbol!("sqlite3_column_name" as Name),
        column_type: symbol!("sqlite3_column_type" as Kind),
        column_int64: symbol!("sqlite3_column_int64" as Integer),
        column_double: symbol!("sqlite3_column_double" as Float),
        column_text: symbol!("sqlite3_column_text" as Blob),
        column_blob: symbol!("sqlite3_column_blob" as Blob),
        column_bytes: symbol!("sqlite3_column_bytes" as Kind),
    })
}

/// One open database, closed when dropped.
struct Connection {
    sqlite: &'static Sqlite,
    db: *mut Db,
}

impl Connection {
    fn open(name: &CStr, flags: c_int) -> Option<Self> {
        let sqlite = sqlite()?;
        let mut db = ptr::null_mut();
        // SAFETY: The name is NUL-terminated and the handle output is writable; the default VFS is used.
        let opened = unsafe { (sqlite.open)(name.as_ptr(), &raw mut db, flags, ptr::null()) };
        if db.is_null() {
            return None;
        }
        // SQLite hands back a handle to close even when the open fails.
        let connection = Self { sqlite, db };
        (opened == OK).then_some(connection)
    }

    /// Runs every statement in `sql`, writing the rows of each that returns any as one JSON array.
    fn run(&self, sql: &CStr, max: usize) -> Option<Vec<u8>> {
        let mut out = Json { out: Vec::new(), max };
        let mut rest = sql.as_ptr();
        loop {
            let (mut stmt, mut tail) = (ptr::null_mut(), ptr::null());
            // SAFETY: `rest` points into the NUL-terminated `sql`, and both outputs are writable.
            if unsafe { (self.sqlite.prepare)(self.db, rest, -1, &raw mut stmt, &raw mut tail) } != OK {
                return None;
            }
            if stmt.is_null() {
                // Only blanks or comments were left.
                return Some(out.out);
            }
            Statement { sqlite: self.sqlite, stmt }.rows(&mut out)?;
            rest = tail;
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: Every statement was finalized before its connection is dropped.
        unsafe { (self.sqlite.close)(self.db) };
    }
}

/// One prepared statement, finalized when dropped.
struct Statement {
    sqlite: &'static Sqlite,
    stmt: *mut Stmt,
}

/// Output that never grows past its limit: every append is checked before it is made.
struct Json {
    out: Vec<u8>,
    max: usize,
}

impl Json {
    fn put(&mut self, bytes: &[u8]) -> Option<()> {
        (self.out.len().checked_add(bytes.len())? <= self.max).then(|| self.out.extend_from_slice(bytes))
    }

    /// Bytes as a JSON string. Like the sqlite3 shell, bytes above ASCII pass through as they are.
    fn string(&mut self, bytes: &[u8]) -> Option<()> {
        // Escapes only lengthen a string, so one that cannot fit as it is is refused before copying.
        if self.out.len().checked_add(bytes.len())?.checked_add(2)? > self.max {
            return None;
        }
        self.put(b"\"")?;
        for &b in bytes {
            match b {
                b'"' => self.put(b"\\\"")?,
                b'\\' => self.put(b"\\\\")?,
                0..0x20 => self.put(format!("\\u{b:04x}").as_bytes())?,
                _ => self.put(&[b])?,
            }
        }
        self.put(b"\"")
    }
}

impl Statement {
    fn rows(&self, out: &mut Json) -> Option<()> {
        // SAFETY: The statement is live.
        let columns = unsafe { (self.sqlite.column_count)(self.stmt) };
        let mut first = true;
        loop {
            // SAFETY: The statement is live.
            match unsafe { (self.sqlite.step)(self.stmt) } {
                ROW => {}
                DONE => break,
                _ => return None,
            }
            out.put(if first { b"[{" } else { b",\n{" })?;
            first = false;
            for column in 0..columns {
                if column > 0 {
                    out.put(b",")?;
                }
                // SAFETY: The column is in range, and the name stays valid until the next step.
                let name = unsafe { (self.sqlite.column_name)(self.stmt, column) };
                if name.is_null() {
                    return None;
                }
                // SAFETY: SQLite returned a NUL-terminated name.
                out.string(unsafe { CStr::from_ptr(name) }.to_bytes())?;
                out.put(b":")?;
                self.value(out, column)?;
            }
            out.put(b"}")?;
        }
        if !first {
            out.put(b"]\n")?;
        }
        Some(())
    }

    /// One column of the current row, as the sqlite3 shell writes it in JSON mode: text as UTF-8
    /// whatever the database's encoding, and a blob's bytes as they are.
    fn value(&self, out: &mut Json, column: c_int) -> Option<()> {
        // SAFETY: The statement has a current row, and the column is in range.
        match unsafe { (self.sqlite.column_type)(self.stmt, column) } {
            // SAFETY: As above.
            INTEGER => out.put(unsafe { (self.sqlite.column_int64)(self.stmt, column) }.to_string().as_bytes()),
            FLOAT => {
                // SAFETY: As above.
                let value = unsafe { (self.sqlite.column_double)(self.stmt, column) };
                if value.is_finite() { out.put(format!("{value:?}").as_bytes()) } else { out.put(b"null") }
            }
            kind @ (TEXT | BLOB) => {
                let bytes = if kind == TEXT {
                    // SAFETY: As above. The text is converted to UTF-8 before its length is read, as SQLite asks.
                    unsafe { (self.sqlite.column_text)(self.stmt, column) }.cast::<u8>()
                } else {
                    // SAFETY: As above.
                    unsafe { (self.sqlite.column_blob)(self.stmt, column) }.cast::<u8>()
                };
                // SAFETY: As above, after the pointer was read.
                let len = usize::try_from(unsafe { (self.sqlite.column_bytes)(self.stmt, column) }).ok()?;
                if bytes.is_null() || len == 0 {
                    return out.string(b"");
                }
                // SAFETY: SQLite keeps `len` bytes at `bytes` until the next step.
                out.string(unsafe { std::slice::from_raw_parts(bytes, len) })
            }
            _ => out.put(b"null"),
        }
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        // SAFETY: The statement is live and is not used again.
        unsafe { (self.sqlite.finalize)(self.stmt) };
    }
}

/// Interrupts a statement once the deadline it was handed has passed.
unsafe extern "system" fn past(deadline: *mut c_void) -> c_int {
    // SAFETY: The handler is only ever registered with the query's deadline, which outlives the connection.
    let deadline = unsafe { &*deadline.cast::<Instant>() };
    c_int::from(Instant::now() >= *deadline)
}

/// A whole file no longer than `limit`, read before the deadline; None otherwise or on any error.
fn read_all(file: &File, limit: u64, deadline: Instant) -> Option<Vec<u8>> {
    if file.metadata().ok()?.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        match file.seek_read(&mut buffer, bytes.len() as u64) {
            Ok(0) => return Some(bytes),
            Ok(n) if (bytes.len() + n) as u64 <= limit => bytes.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            _ => return None,
        }
    }
}

/// Up to `len` bytes from the start of a file.
fn start(file: &File, len: usize) -> Option<Vec<u8>> {
    let mut bytes = vec![0; len];
    let mut read = 0;
    while read < len {
        match file.seek_read(&mut bytes[read..], read as u64) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    bytes.truncate(read);
    Some(bytes)
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// SQLite's running checksum over 8-byte steps, in the byte order the log's magic number names.
fn checksum(mut sum: (u32, u32), bytes: &[u8], big_endian: bool) -> (u32, u32) {
    let word = |raw: [u8; 4]| if big_endian { u32::from_be_bytes(raw) } else { u32::from_le_bytes(raw) };
    for &[a, b, c, d, e, f, g, h] in bytes.as_chunks::<8>().0 {
        sum.0 = sum.0.wrapping_add(word([a, b, c, d])).wrapping_add(sum.1);
        sum.1 = sum.1.wrapping_add(word([e, f, g, h])).wrapping_add(sum.0);
    }
    sum
}

/// The page size a database header names, if it is one SQLite accepts.
fn page_size(db: &[u8]) -> Option<usize> {
    let size = match u16::from_be_bytes([db[16], db[17]]) {
        1 => 65536,
        size => usize::from(size),
    };
    (size.is_power_of_two() && size >= 512).then_some(size)
}

/// A first page SQLite would open: its header, a page size it accepts and format versions 1 or 2.
fn header_valid(db: &[u8]) -> bool {
    db.len() >= 100
        && &db[..16] == b"SQLite format 3\0"
        && page_size(db).is_some()
        && matches!((db[18], db[19]), (1 | 2, 1 | 2))
}

/// The database as of the last commit in its log, with the log folded in and the header marked as
/// having none, so SQLite reads the copy on its own. This is SQLite's own recovery: every frame that
/// carries the log's salts and continues its checksum counts, up to the last that commits. A live
/// writer publishes a commit only after writing it, so the copy can be ahead of a live reader by a
/// commit whose publication is moments away; it never mixes two states. As SQLite does, a log is used
/// only for a database whose read version is 2; one with an unknown version or a different page size
/// leaves the database unread, and one with a foreign magic number or header checksum counts as
/// empty. Nothing grows past `limit`, and work stops at the deadline.
pub(super) fn fold(mut db: Vec<u8>, wal: &[u8], limit: u64, deadline: Instant) -> Option<Vec<u8>> {
    if !header_valid(&db) {
        return None;
    }
    let page = page_size(&db)?;
    let magic = if wal.len() >= WAL_HEADER { be32(wal, 0) } else { 0 };
    let big_endian = magic & 1 == 1;
    let log = db[19] == 2
        && magic & !1 == 0x377f_0682
        && usize::try_from(be32(wal, 8)).is_ok_and(|size| size.is_power_of_two() && (512..=65536).contains(&size))
        && checksum((0, 0), &wal[..24], big_endian) == (be32(wal, 24), be32(wal, 28));
    if !log {
        return Some(normalized(db));
    }
    if be32(wal, 4) != 3_007_000 || usize::try_from(be32(wal, 8)).ok()? != page {
        return None;
    }
    let mut sum = (be32(wal, 24), be32(wal, 28));
    let (mut frames, mut committed) = (Vec::new(), 0);
    for frame in wal[WAL_HEADER..].chunks_exact(FRAME_HEADER + page) {
        if frame[8..16] != wal[16..24] {
            break;
        }
        if frames.len() % 64 == 0 && Instant::now() >= deadline {
            return None;
        }
        sum = checksum(checksum(sum, &frame[..8], big_endian), &frame[FRAME_HEADER..], big_endian);
        if sum != (be32(frame, 16), be32(frame, 20)) {
            break;
        }
        frames.push(frame);
        if be32(frame, 4) != 0 {
            committed = frames.len();
        }
    }
    if committed > 0 {
        let size = u64::from(be32(frames[committed - 1], 4)).checked_mul(page as u64).filter(|&size| size <= limit)?;
        db.resize(usize::try_from(size).ok()?, 0);
        for frame in &frames[..committed] {
            let at = usize::try_from(be32(frame, 0)).ok()?.checked_sub(1)?.checked_mul(page)?;
            if let Some(target) = db.get_mut(at..at.checked_add(page)?) {
                target.copy_from_slice(&frame[FRAME_HEADER..]);
            }
        }
    }
    (header_valid(&db) && page_size(&db) == Some(page)).then(|| normalized(db))
}

/// Marks a copy as having no log, so SQLite reads it on its own.
fn normalized(mut db: Vec<u8>) -> Vec<u8> {
    db[18] = 1;
    db[19] = 1;
    db
}

#[repr(C)]
#[derive(Default)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: usize,
}

#[repr(C)]
struct FileIdInfo {
    volume_serial: u64,
    id: [u8; 16],
}

/// SQLite's lock bytes on Windows: the database's, past the end of any database Milo reads, and the
/// WAL index's read-lock slot 0, which a checkpoint must hold exclusively to write into the database.
const PENDING_BYTE: u32 = 0x4000_0000;
const SHARED_FIRST: u32 = PENDING_BYTE + 2;
const SHARED_SIZE: u32 = 510;
const READ_LOCK_0: u32 = 120 + 3;
const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
const FILE_ID_INFO: c_int = 0x12;

/// A shared lock on a byte range, released when dropped. Locks change no bytes.
struct Held<'a> {
    file: &'a File,
    offset: u32,
    len: u32,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let mut overlapped = Overlapped { offset: self.offset, ..Overlapped::default() };
        // SAFETY: The range was locked through this open handle; the OVERLAPPED only names it.
        unsafe { UnlockFileEx(self.file.as_raw_handle(), 0, self.len, 0, &raw mut overlapped) };
    }
}

/// Takes a shared lock on the range without waiting, as SQLite takes its shared locks on Windows.
fn hold(file: &File, offset: u32, len: u32) -> Option<Held<'_>> {
    let mut overlapped = Overlapped { offset, ..Overlapped::default() };
    // SAFETY: The handle is open, the OVERLAPPED only names the range, and the call never waits.
    let locked =
        unsafe { LockFileEx(file.as_raw_handle(), LOCKFILE_FAIL_IMMEDIATELY, 0, len, 0, &raw mut overlapped) } != 0;
    locked.then(|| Held { file, offset, len })
}

/// SQLite's shared lock on a database, taken as SQLite takes it: through a brief shared pending lock,
/// so a writer already waiting for its exclusive lock goes first. While it is held, no connection can
/// take the exclusive lock it needs to write a rollback-journal database in place, or to check its log
/// into the database and delete it on close; it leaves the log, and its WAL index, instead.
fn read_lock(db: &File) -> Option<Held<'_>> {
    let pending = hold(db, PENDING_BYTE, 1)?;
    let shared = hold(db, SHARED_FIRST, SHARED_SIZE);
    drop(pending);
    shared
}

/// The 128-bit identity of an open file.
fn identity(file: &File) -> Option<(u64, [u8; 16])> {
    let mut info = FileIdInfo { volume_serial: 0, id: [0; 16] };
    let size = u32::try_from(size_of::<FileIdInfo>()).ok()?;
    // SAFETY: The handle is open, and the output is a writable FILE_ID_INFO of the size given.
    let found =
        unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), FILE_ID_INFO, (&raw mut info).cast(), size) };
    (found != 0).then_some((info.volume_serial, info.id))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// An open file at `path`, None if there is none, opened without following a link.
fn open(path: &Path) -> Result<Option<File>, ()> {
    match fs_ops::open_read(path) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// The identity of the file now at `path`, or None if there is none.
fn now_at(path: &Path) -> Result<Option<(u64, [u8; 16])>, ()> {
    match open(path)? {
        Some(file) => identity(&file).map(Some).ok_or(()),
        None => Ok(None),
    }
}

/// What one copy found.
pub(super) enum Attempt {
    Copied {
        db: Vec<u8>,
        wal: Vec<u8>,
    },
    /// A writer was busy; a later attempt may find the files settled.
    Again,
    Unreadable,
}

/// One copy of the database and its log, under SQLite's shared lock on the database and, when the
/// database has a WAL index, its read-lock slot 0. Together they stop every write into the database
/// file for the copy: a rollback writer needs the exclusive database lock and a checkpoint needs slot 0
/// exclusively. The log can still grow, or be emptied or restarted once fully checkpointed, so the copy
/// stands only if its header is unchanged, every file at its name is the one read, and no WAL index or
/// log appeared. The WAL index itself is never read: it is SQLite's shared memory, and neither its
/// freshness nor whether a live process still owns it can be learned from outside without a mapping
/// or a lock that SQLite would notice.
pub(super) fn copy(path: &Path, limit: u64, deadline: Instant) -> Attempt {
    use Attempt::{Again, Unreadable};
    let Ok(Some(db)) = open(path) else { return Unreadable };
    if db.metadata().map_or(true, |m| m.len() > limit) {
        return Unreadable;
    }
    let Some(_db_lock) = read_lock(&db) else { return Again };
    // A journal that starts with anything but zero belongs to a transaction only its writer can finish or undo.
    match open(&sidecar(path, "-journal")) {
        Ok(None) => {}
        Ok(Some(journal)) if start(&journal, 1).is_some_and(|b| b.first().is_none_or(|&b| b == 0)) => {}
        Ok(Some(_)) => return Again,
        Err(()) => return Unreadable,
    }
    let (Ok(shm), Ok(wal)) = (open(&sidecar(path, "-shm")), open(&sidecar(path, "-wal"))) else { return Unreadable };
    let _slot = match &shm {
        Some(shm) => match hold(shm, READ_LOCK_0, 1) {
            Some(held) => Some(held),
            None => return Again,
        },
        None => None,
    };
    let identities = |file: &Option<File>| match file {
        Some(file) => identity(file).map(Some).ok_or(()),
        None => Ok(None),
    };
    let (Some(db_id), Ok(shm_id), Ok(wal_id)) = (identity(&db), identities(&shm), identities(&wal)) else {
        return Unreadable;
    };
    let Ok(size) = db.metadata().map(|m| m.len()) else { return Unreadable };
    let before = match &wal {
        Some(file) => match start(file, WAL_HEADER) {
            Some(header) => header,
            None => return Unreadable,
        },
        None => Vec::new(),
    };
    let Some(bytes) = read_all(&db, limit, deadline) else { return Unreadable };
    let frames = match &wal {
        Some(file) => match read_all(file, limit - bytes.len() as u64, deadline) {
            Some(frames) => frames,
            None => return Unreadable,
        },
        None => Vec::new(),
    };
    let after = wal.as_ref().map_or(Some(Vec::new()), |file| start(file, WAL_HEADER));
    let settled = after.as_ref() == Some(&before)
        && frames[..WAL_HEADER.min(frames.len())] == before[..]
        && db.metadata().is_ok_and(|m| m.len() == size)
        && now_at(path) == Ok(Some(db_id))
        && now_at(&sidecar(path, "-shm")) == Ok(shm_id)
        && now_at(&sidecar(path, "-wal")) == Ok(wal_id);
    if !settled {
        return Again;
    }
    Attempt::Copied { db: bytes, wal: frames }
}

/// A coherent copy, or None once the deadline passes or a writer keeps every attempt from settling.
/// The locks are released before the copy is folded.
pub(super) fn snapshot(path: &Path, limit: u64, deadline: Instant) -> Option<Vec<u8>> {
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(RETRY);
        }
        if Instant::now() >= deadline {
            return None;
        }
        match copy(path, limit, deadline) {
            Attempt::Copied { db, wal } => return fold(db, &wal, limit, deadline),
            Attempt::Again => {}
            Attempt::Unreadable => return None,
        }
    }
    None
}

/// Whether a database and its log hold more than a query on Windows copies.
pub(in crate::usage) fn too_large(path: &Path) -> bool {
    let size = |path: &Path| std::fs::symlink_metadata(path).map_or(0, |metadata| metadata.len());
    size(path).saturating_add(size(&sidecar(path, "-wal"))) > MAX_SNAPSHOT
}

/// Bound memory, output and wall time; the database's own files are only ever read.
pub(super) fn query(path: &Path, sql: &str, deadline: Instant, max: usize) -> Option<Vec<u8>> {
    let sql = CString::new(sql).ok()?;
    let mut image = snapshot(path, MAX_SNAPSHOT, deadline)?;
    if Instant::now() >= deadline {
        return None;
    }
    let size = i64::try_from(image.len()).ok()?;
    let connection = Connection::open(c":memory:", OPEN_READWRITE | OPEN_CREATE)?;
    // SAFETY: The image outlives the connection, which only reads it; SQLite is not asked to free it.
    let loaded = unsafe {
        (connection.sqlite.deserialize)(
            connection.db,
            c"main".as_ptr(),
            image.as_mut_ptr(),
            size,
            size,
            DESERIALIZE_READONLY,
        )
    };
    if loaded != OK {
        return None;
    }
    let handed = (&raw const deadline).cast_mut().cast();
    // SAFETY: The connection is live, and the deadline outlives it.
    unsafe { (connection.sqlite.progress_handler)(connection.db, STEPS, Some(past), handed) };
    connection.run(c"PRAGMA temp_store=MEMORY;", 0)?;
    let out = connection.run(&sql, max)?;
    drop(connection);
    drop(image);
    (Instant::now() < deadline).then_some(out)
}

/// A connection that may write, for building fixture databases.
#[cfg(test)]
pub(in crate::usage) struct Writer(Connection);

#[cfg(test)]
impl Writer {
    pub(in crate::usage) fn open(path: &Path) -> Self {
        let name = CString::new(path.to_str().unwrap()).unwrap();
        Self(Connection::open(&name, OPEN_READWRITE | OPEN_CREATE).expect("cannot open a fixture database"))
    }

    pub(in crate::usage) fn exec(&self, sql: &str) {
        assert!(self.try_exec(sql), "failed: {sql}");
    }

    fn try_exec(&self, sql: &str) -> bool {
        self.0.run(&CString::new(sql).unwrap(), usize::MAX).is_some()
    }
}

#[cfg(test)]
mod tests;
