//! Windows form of the read-only query. Windows has no sqlite3 program, so the SQLite that ships
//! with it, System32\winsqlite3.dll, runs the query on the reading thread. Its answer is written
//! as the sqlite3 shell's `-json` output, so the readers parse one format on every system.
//!
//! That build also ignores `readonly_shm`: opening a log whose sidecars are gone makes new ones.
//! Only a database whose sidecars were both found is opened, so that happens only if the writer
//! removes them between that look and the open.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::Write as _;
use std::os::windows::fs::MetadataExt as _;
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;
use std::time::Instant;

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
const OPEN_READONLY: c_int = 0x1;
const OPEN_URI: c_int = 0x40;
/// Windows' SQLite accepts this flag but still follows a link, so links are refused before opening.
const OPEN_NOFOLLOW: c_int = 0x0100_0000;
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// Virtual machine steps between looks at the clock.
const STEPS: c_int = 1000;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> Option<unsafe extern "system" fn() -> isize>;
}

type Open = unsafe extern "system" fn(*const c_char, *mut *mut Db, c_int, *const c_char) -> c_int;
type Close = unsafe extern "system" fn(*mut Db) -> c_int;
type BusyTimeout = unsafe extern "system" fn(*mut Db, c_int) -> c_int;
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
    busy_timeout: BusyTimeout,
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
        busy_timeout: symbol!("sqlite3_busy_timeout" as BusyTimeout),
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

/// Whether the file at `path` with `suffix` added is a link, junction or other reparse point.
/// A name that cannot be looked at counts as one.
fn linked(path: &Path, suffix: &str) -> bool {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    match std::fs::symlink_metadata(name) {
        Ok(metadata) => metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Bound output and wall time; a busy database waits only until the deadline.
pub(super) fn query(path: &Path, uri: &str, sql: &str, deadline: Instant, max: usize) -> Option<Vec<u8>> {
    // SQLite would follow a link and keep its own sidecars beside it.
    if ["", "-wal", "-shm"].iter().any(|suffix| linked(path, suffix)) {
        return None;
    }
    let (uri, sql) = (CString::new(uri).ok()?, CString::new(sql).ok()?);
    let connection = Connection::open(&uri, OPEN_READONLY | OPEN_URI | OPEN_NOFOLLOW)?;
    let wait = c_int::try_from(deadline.saturating_duration_since(Instant::now()).as_millis()).ok()?;
    // SAFETY: The connection is live.
    unsafe { (connection.sqlite.busy_timeout)(connection.db, wait) };
    let handed = (&raw const deadline).cast_mut().cast();
    // SAFETY: The connection is live, and the deadline outlives it.
    unsafe { (connection.sqlite.progress_handler)(connection.db, STEPS, Some(past), handed) };
    let out = connection.run(&sql, max)?;
    (Instant::now() < deadline).then_some(out)
}

/// A connection that may write, for building fixture databases.
#[cfg(test)]
pub(in crate::usage) struct Writer(Connection);

#[cfg(test)]
impl Writer {
    const OPEN_READWRITE_CREATE: c_int = 0x2 | 0x4;

    pub(in crate::usage) fn open(path: &Path) -> Self {
        let name = CString::new(path.to_str().unwrap()).unwrap();
        Self(Connection::open(&name, Self::OPEN_READWRITE_CREATE).expect("cannot open a fixture database"))
    }

    pub(in crate::usage) fn exec(&self, sql: &str) {
        assert!(self.0.run(&CString::new(sql).unwrap(), usize::MAX).is_some(), "failed: {sql}");
    }
}

#[cfg(test)]
mod tests;
