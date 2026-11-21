//! Tells whether a database another tool keeps has been written since Milo
//! last read it, from its files alone, so that an idle database is not
//! queried again only to give the same answer.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use crate::fs_ops;

/// A file must have been left alone this many seconds before a read of it
/// counts as settled. A write that lands in the same tick of the file
/// system's clock as the one before it leaves the times as they were.
const QUIET: i64 = 2;
/// A database's header holds its change counter and size; a write-ahead
/// log's holds the salts that change each time the log starts over. A
/// database in write-ahead mode leaves its counter alone, and a checkpoint
/// can leave both files their sizes, so there the times carry the weight.
const HEAD: usize = 100;

/// What a write to a file changes, without reading the file through.
#[derive(PartialEq, Eq)]
struct Mark {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    head: [u8; HEAD],
}

impl Mark {
    /// Reads through the open file, so that the times and the bytes are one file's.
    fn of(mut file: File) -> Option<Self> {
        let info = file.metadata().ok().filter(|info| info.is_file())?;
        let mut head = [0; HEAD];
        let mut filled = 0;
        while filled < HEAD {
            match file.read(&mut head[filled..]) {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
        Some(Self {
            device: info.dev(),
            inode: info.ino(),
            size: info.size(),
            modified: (info.mtime(), info.mtime_nsec()),
            head,
        })
    }
}

/// A database and its write-ahead log as a write, a checkpoint, a
/// replacement or a removal would change them.
#[derive(PartialEq, Eq)]
pub(super) struct Signature {
    database: Mark,
    /// The log, where SQLite keeps commits not yet moved into the database.
    log: Option<Mark>,
    /// Whether the log's index is there. Its contents change without a write.
    index: bool,
}

impl Signature {
    /// None when the files cannot say: a missing or unreadable database, or
    /// a log that is there but cannot be opened. Links are not followed.
    pub(super) fn of(path: &Path) -> Option<Self> {
        let beside = |suffix: &str| {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            name
        };
        let database = Mark::of(fs_ops::open_read(path).ok()?)?;
        let log = match fs_ops::open_read(Path::new(&beside("-wal"))) {
            Ok(file) => Some(Mark::of(file)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        let index = std::fs::symlink_metadata(beside("-shm")).is_ok_and(|info| info.is_file());
        Some(Self { database, log, index })
    }

    /// Whether every file was last written long enough before `now` that a
    /// later write must show in its times.
    pub(super) fn quiet(&self, now: i64) -> bool {
        [Some(&self.database), self.log.as_ref()]
            .into_iter()
            .flatten()
            .all(|mark| mark.modified.0.saturating_add(QUIET) <= now)
    }

    /// Whether the database keeps a write-ahead log. A commit there shows to
    /// readers through the log's index, which these files cannot vouch for.
    pub(super) fn write_ahead(&self) -> bool {
        self.log.is_some() || self.index || self.database.head[18] == 2 || self.database.head[19] == 2
    }
}
