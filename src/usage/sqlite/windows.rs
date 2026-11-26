//! Windows form of the read-only query, for databases no connection is using through a write-ahead
//! log. Windows has no sqlite3 program, and the SQLite that ships with it, System32\winsqlite3.dll,
//! ignores `readonly_shm`: opened in place, it would write the WAL index and make missing sidecars.
//! So the database is never opened in place. Under SQLite's own shared lock, its file is read through
//! a handle that only reads, and a private copy in memory is queried. A database in use through a log
//! is left unread: what its writer has published cannot be learned from outside without a mapping or
//! a lock SQLite would notice. Nothing beside the database is opened for writing, made or changed.
//!
//! The answer is written as the sqlite3 shell's `-json` output, so the readers parse one format.

use std::ffi::{CStr, CString, OsString, c_char, c_int, c_uint, c_void};
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
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
/// The most a database may hold before it is left unread.
pub(super) const MAX_SNAPSHOT: u64 = 64 << 20;
/// A writer that is busy during every copy leaves the database unread.
const ATTEMPTS: usize = 3;
const RETRY: std::time::Duration = std::time::Duration::from_millis(10);

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
    fn GetFinalPathNameByHandleW(file: *mut c_void, path: *mut u16, len: u32, flags: u32) -> u32;
    fn GetShortPathNameW(long: *const u16, short: *mut u16, len: u32) -> u32;
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

#[repr(C)]
#[derive(Default)]
struct FileStandardInfo {
    allocation_size: i64,
    end_of_file: i64,
    number_of_links: u32,
    delete_pending: u8,
    directory: u8,
}

/// SQLite's lock bytes on a database on Windows, past the end of any database Milo reads.
const PENDING_BYTE: u32 = 0x4000_0000;
const SHARED_FIRST: u32 = PENDING_BYTE + 2;
const SHARED_SIZE: u32 = 510;
const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
const FILE_ID_INFO: c_int = 0x12;
const FILE_STANDARD_INFO: c_int = 0x1;
const FILE_NAME_NORMALIZED: u32 = 0;
/// The longest path Windows names, in UTF-16 units.
const MAX_PATH_UNITS: usize = 32_768;

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
/// into the database and delete the log and its index on close; it leaves them instead.
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

/// Whether the open file has one name, so no other name can have a log or a journal beside it.
fn one_name(file: &File) -> bool {
    let mut info = FileStandardInfo::default();
    let Ok(size) = u32::try_from(size_of::<FileStandardInfo>()) else { return false };
    // SAFETY: The handle is open, and the output is a writable FILE_STANDARD_INFO of the size given.
    let found =
        unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), FILE_STANDARD_INFO, (&raw mut info).cast(), size) };
    found != 0 && info.number_of_links == 1 && info.delete_pending == 0 && info.directory == 0
}

/// A path's UTF-16 form, NUL-terminated.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

/// The text a Windows call wrote into `buffer`, if it fitted.
fn written(buffer: &[u16], len: u32) -> Option<PathBuf> {
    let len = usize::try_from(len).ok().filter(|&len| len > 0 && len < buffer.len())?;
    Some(PathBuf::from(OsString::from_wide(&buffer[..len])))
}

/// The open file's own long name, as Windows records it.
fn final_name(file: &File) -> Option<OsString> {
    let mut buffer = vec![0u16; MAX_PATH_UNITS];
    let len = u32::try_from(buffer.len()).ok()?;
    // SAFETY: The handle is open, and the buffer holds `len` units.
    let got =
        unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), buffer.as_mut_ptr(), len, FILE_NAME_NORMALIZED) };
    written(&buffer, got)?.file_name().map(ToOwned::to_owned)
}

/// The short (8.3) spelling of a path, when the volume keeps one that differs.
fn short_spelling(path: &Path) -> Option<PathBuf> {
    let long = wide(path);
    let mut buffer = vec![0u16; MAX_PATH_UNITS];
    let len = u32::try_from(buffer.len()).ok()?;
    // SAFETY: The name is NUL-terminated, and the buffer holds `len` units.
    let got = unsafe { GetShortPathNameW(long.as_ptr(), buffer.as_mut_ptr(), len) };
    let short = written(&buffer, got)?;
    let name = short.file_name()?;
    (Some(name) != path.file_name()).then(|| path.with_file_name(name))
}

/// Every name SQLite could give this database's log, index and journal: the name it is read by,
/// which must be its own long name, and that name's short spelling. A database known by any other
/// name, through a second link, could have a writer whose files are beside that name instead.
fn names(path: &Path, db: &File) -> Option<Vec<PathBuf>> {
    let name = path.file_name()?;
    if !one_name(db) || final_name(db)?.to_str()?.to_lowercase() != name.to_str()?.to_lowercase() {
        return None;
    }
    Some([Some(path.to_owned()), short_spelling(path)].into_iter().flatten().collect())
}

/// Whether a log or an index is beside a database, as one is while any connection uses it through a
/// write-ahead log. Only names are looked at.
pub(in crate::usage) fn in_use(path: &Path) -> bool {
    ["-wal", "-shm"].into_iter().any(|suffix| std::fs::symlink_metadata(sidecar(path, suffix)).is_ok())
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
    Copied(Vec<u8>),
    /// A writer was busy; a later attempt may find the files settled.
    Again,
    /// The database is in use through a write-ahead log, or cannot be read.
    Unreadable,
}

/// One copy of a database no connection is using through a write-ahead log. Windows' SQLite cannot
/// read such a database in place without writing to its WAL index, and what a live writer has
/// published there cannot be learned from outside without a mapping or a lock SQLite would notice,
/// so a database with a log or an index beside it is left unread. With neither, the database file
/// holds every committed transaction. SQLite's shared lock, held for the copy, keeps rollback writers
/// out of the file and keeps a connection that opens meanwhile from removing the log and index it
/// makes; finding neither after the copy, and the same file at the same size, means none did. That
/// holds only for the names the files are looked for under, so a database with a second link, or
/// read by a name other than its own, is left unread. The sidecars are looked for before the lock
/// too, so a database already in use when a look starts is not locked.
pub(super) fn copy(path: &Path, limit: u64, deadline: Instant) -> Attempt {
    use Attempt::{Again, Unreadable};
    let Ok(Some(db)) = open(path) else { return Unreadable };
    if db.metadata().map_or(true, |m| m.len() > limit) {
        return Unreadable;
    }
    let Some(names) = names(path, &db) else { return Unreadable };
    let quiet = || {
        names
            .iter()
            .all(|name| now_at(&sidecar(name, "-wal")) == Ok(None) && now_at(&sidecar(name, "-shm")) == Ok(None))
    };
    if !quiet() {
        return Unreadable;
    }
    let Some(_lock) = read_lock(&db) else { return Again };
    if !quiet() {
        return Unreadable;
    }
    // A journal that starts with anything but zero belongs to a transaction only its writer can finish or undo.
    for name in &names {
        match open(&sidecar(name, "-journal")) {
            Ok(None) => {}
            Ok(Some(journal)) if start(&journal, 1).is_some_and(|b| b.first().is_none_or(|&b| b == 0)) => {}
            Ok(Some(_)) => return Again,
            Err(()) => return Unreadable,
        }
    }
    let (Some(id), Ok(size)) = (identity(&db), db.metadata().map(|m| m.len())) else { return Unreadable };
    let Some(bytes) = read_all(&db, limit, deadline) else { return Unreadable };
    let settled =
        quiet() && one_name(&db) && now_at(path) == Ok(Some(id)) && db.metadata().is_ok_and(|m| m.len() == size);
    if !settled {
        return Again;
    }
    Attempt::Copied(bytes)
}

/// A copy SQLite can open on its own: the header is checked and marked as having no log, which a
/// database without one does not need.
fn normalized(mut db: Vec<u8>) -> Option<Vec<u8>> {
    if !header_valid(&db) {
        return None;
    }
    db[18] = 1;
    db[19] = 1;
    Some(db)
}

/// A coherent copy, or None once the deadline passes, the database is in use through a log, or a
/// writer keeps every attempt from settling.
pub(super) fn snapshot(path: &Path, limit: u64, deadline: Instant) -> Option<Vec<u8>> {
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(RETRY);
        }
        if Instant::now() >= deadline {
            return None;
        }
        match copy(path, limit, deadline) {
            Attempt::Copied(db) => return normalized(db),
            Attempt::Again => {}
            Attempt::Unreadable => return None,
        }
    }
    None
}

/// Whether a database holds more than a query on Windows copies.
pub(in crate::usage) fn too_large(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.len() > MAX_SNAPSHOT)
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
