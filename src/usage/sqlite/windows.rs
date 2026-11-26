//! Windows form of the read-only query. Windows has no sqlite3 program, and the SQLite that ships
//! with it, System32\winsqlite3.dll, ignores `readonly_shm`: opened in place, it would write its
//! shared-memory file and make missing sidecars. So the database is never opened in place. Its file
//! and write-ahead log are read through handles that only read, the committed frames are applied to
//! a private copy in memory, and that copy is queried. Nothing beside the database is opened, made
//! or written, and the tool that owns it never waits on Milo.
//!
//! The answer is written as the sqlite3 shell's `-json` output, so the readers parse one format.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::fs::File;
use std::io::{self, Write as _};
use std::os::windows::fs::FileExt as _;
use std::path::Path;
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
/// A writer that restarts its log during every copy leaves the database unread.
const ATTEMPTS: usize = 3;
const WAL_HEADER: usize = 32;
const FRAME_HEADER: usize = 24;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> Option<unsafe extern "system" fn() -> isize>;
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
        let mut out = Vec::new();
        let mut rest = sql.as_ptr();
        loop {
            let (mut stmt, mut tail) = (ptr::null_mut(), ptr::null());
            // SAFETY: `rest` points into the NUL-terminated `sql`, and both outputs are writable.
            if unsafe { (self.sqlite.prepare)(self.db, rest, -1, &raw mut stmt, &raw mut tail) } != OK {
                return None;
            }
            if stmt.is_null() {
                // Only blanks or comments were left.
                return Some(out);
            }
            Statement { sqlite: self.sqlite, stmt }.rows(&mut out, max)?;
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

impl Statement {
    fn rows(&self, out: &mut Vec<u8>, max: usize) -> Option<()> {
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
            out.extend_from_slice(if first { b"[{" } else { b",\n{" });
            first = false;
            for column in 0..columns {
                if column > 0 {
                    out.push(b',');
                }
                // SAFETY: The column is in range, and the name stays valid until the next step.
                let name = unsafe { (self.sqlite.column_name)(self.stmt, column) };
                if name.is_null() {
                    return None;
                }
                // SAFETY: SQLite returned a NUL-terminated name.
                string(out, unsafe { CStr::from_ptr(name) }.to_bytes());
                out.push(b':');
                self.value(out, column);
            }
            out.push(b'}');
            if out.len() > max {
                return None;
            }
        }
        if !first {
            out.extend_from_slice(b"]\n");
        }
        Some(())
    }

    /// One column of the current row, as the sqlite3 shell writes it in JSON mode.
    fn value(&self, out: &mut Vec<u8>, column: c_int) {
        // SAFETY: The statement has a current row, and the column is in range.
        match unsafe { (self.sqlite.column_type)(self.stmt, column) } {
            // SAFETY: As above.
            INTEGER => write!(out, "{}", unsafe { (self.sqlite.column_int64)(self.stmt, column) }).unwrap_or(()),
            FLOAT => {
                // SAFETY: As above.
                let value = unsafe { (self.sqlite.column_double)(self.stmt, column) };
                if value.is_finite() {
                    write!(out, "{value:?}").unwrap_or(());
                } else {
                    out.extend_from_slice(b"null");
                }
            }
            TEXT | BLOB => {
                // SAFETY: As above. The pointer is read before the length, as SQLite asks.
                let bytes = unsafe { (self.sqlite.column_blob)(self.stmt, column) };
                // SAFETY: As above.
                let len = usize::try_from(unsafe { (self.sqlite.column_bytes)(self.stmt, column) }).unwrap_or(0);
                if bytes.is_null() || len == 0 {
                    string(out, b"");
                } else {
                    // SAFETY: SQLite keeps `len` bytes at `bytes` until the next step.
                    string(out, unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), len) });
                }
            }
            _ => out.extend_from_slice(b"null"),
        }
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        // SAFETY: The statement is live and is not used again.
        unsafe { (self.sqlite.finalize)(self.stmt) };
    }
}

/// Bytes as a JSON string. Like the sqlite3 shell, bytes above ASCII pass through as they are.
fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'"');
    for &b in bytes {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0..0x20 => write!(out, "\\u{b:04x}").unwrap_or(()),
            _ => out.push(b),
        }
    }
    out.push(b'"');
}

/// Interrupts a statement once the deadline it was handed has passed.
unsafe extern "system" fn past(deadline: *mut c_void) -> c_int {
    // SAFETY: The handler is only ever registered with the query's deadline, which outlives the connection.
    let deadline = unsafe { &*deadline.cast::<Instant>() };
    c_int::from(Instant::now() >= *deadline)
}

/// Up to `limit` bytes of a file from its start; None past the limit or on any error.
fn read_all(file: &File, limit: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        match file.seek_read(&mut buffer, bytes.len() as u64) {
            Ok(0) => return Some(bytes),
            Ok(n) if (bytes.len() + n) as u64 <= limit => bytes.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            _ => return None,
        }
    }
}

fn header(file: &File) -> Option<Vec<u8>> {
    let mut header = vec![0; WAL_HEADER];
    let mut read = 0;
    while read < WAL_HEADER {
        match file.seek_read(&mut header[read..], read as u64) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    header.truncate(read);
    Some(header)
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

/// The database as of the last transaction committed in its log, with the log folded in and the
/// header marked as having none, so SQLite reads the copy on its own. A frame after the last commit,
/// a torn frame and any frame after one are left out, as SQLite itself leaves them out.
pub(super) fn fold(mut db: Vec<u8>, wal: &[u8]) -> Option<Vec<u8>> {
    if db.len() < 100 || &db[..16] != b"SQLite format 3\0" || !matches!((db[18], db[19]), (1 | 2, 1 | 2)) {
        return None;
    }
    if wal.len() >= WAL_HEADER {
        let magic = be32(wal, 0);
        let page = usize::try_from(be32(wal, 8)).ok()?;
        let header_sum = (be32(wal, 24), be32(wal, 28));
        let big_endian = magic & 1 == 1;
        let db_page = match u16::from_be_bytes([db[16], db[17]]) {
            1 => 65536,
            size => usize::from(size),
        };
        let valid = magic & !1 == 0x377f_0682
            && page == db_page
            && be32(wal, 4) == 3_007_000
            && page.is_power_of_two()
            && (512..=65536).contains(&page)
            && checksum((0, 0), &wal[..24], big_endian) == header_sum;
        if valid {
            let mut sum = header_sum;
            let (mut pages, mut committed) = (Vec::new(), None);
            for frame in wal[WAL_HEADER..].chunks_exact(FRAME_HEADER + page) {
                if frame[8..16] != wal[16..24] {
                    break;
                }
                sum = checksum(checksum(sum, &frame[..8], big_endian), &frame[FRAME_HEADER..], big_endian);
                if sum != (be32(frame, 16), be32(frame, 20)) {
                    break;
                }
                pages.push(frame);
                if be32(frame, 4) != 0 {
                    committed = Some(pages.len());
                }
            }
            if let Some(count) = committed {
                let size = usize::try_from(be32(pages[count - 1], 4)).ok()?.checked_mul(page)?;
                db.resize(size, 0);
                for frame in &pages[..count] {
                    let number = usize::try_from(be32(frame, 0)).ok()?.checked_sub(1)?;
                    let at = number.checked_mul(page)?;
                    if let Some(target) = db.get_mut(at..at + page) {
                        target.copy_from_slice(&frame[FRAME_HEADER..]);
                    }
                }
            }
        }
    }
    if db.len() < 100 || &db[..16] != b"SQLite format 3\0" {
        return None;
    }
    db[18] = 1;
    db[19] = 1;
    Some(db)
}

/// A coherent copy of the database and its log. A writer that restarts the log changes its header
/// first, so an unchanged header around the copy means every committed frame the copy needs is in it.
/// A checkpoint during the copy only moves frames the log still holds. The handles share reading,
/// writing and deletion, so the writer is never held back.
pub(super) fn snapshot(path: &Path, limit: u64, deadline: Instant) -> Option<Vec<u8>> {
    let mut log = path.as_os_str().to_owned();
    log.push("-wal");
    for _ in 0..ATTEMPTS {
        if Instant::now() >= deadline {
            return None;
        }
        let wal = match fs_ops::open_read(Path::new(&log)) {
            Ok(file) => Some(file),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        let before = match &wal {
            Some(file) => Some(header(file)?),
            None => None,
        };
        let db = read_all(&fs_ops::open_read(path).ok()?, limit)?;
        let frames = match &wal {
            Some(file) => read_all(file, limit.saturating_sub(db.len() as u64))?,
            None => Vec::new(),
        };
        let after = match &wal {
            Some(file) => Some(header(file)?),
            None => std::fs::symlink_metadata(&log).is_ok().then(Vec::new),
        };
        let start = &frames[..WAL_HEADER.min(frames.len())];
        if before == after && before.as_deref().unwrap_or_default() == start {
            return fold(db, &frames);
        }
    }
    None
}

/// Bound memory, output and wall time; the database's own files are only ever read.
pub(super) fn query(path: &Path, sql: &str, deadline: Instant, max: usize) -> Option<Vec<u8>> {
    let sql = CString::new(sql).ok()?;
    let mut image = snapshot(path, MAX_SNAPSHOT, deadline)?;
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
        assert!(self.0.run(&CString::new(sql).unwrap(), usize::MAX).is_some(), "failed: {sql}");
    }
}

#[cfg(test)]
mod tests;
