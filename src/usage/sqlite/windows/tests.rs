//! The Windows query against benign databases made by Windows' own SQLite in a scratch folder. The
//! database's files and folder must read the same after every query as before it.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use super::super::{MAX_OUTPUT, executable, query as checked_query};
use super::*;
use crate::fixture::Fixture;

const WAIT: Duration = Duration::from_secs(30);
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

/// A database in WAL mode, in a folder whose name needs escaping, with its writer still open and
/// never checkpointing on its own.
fn busy(f: &Fixture) -> (PathBuf, Writer) {
    let folder = f.root.join("Agent data é #1");
    fs::create_dir(&folder).unwrap();
    let path = folder.join("agent.db");
    let writer = Writer::open(&path);
    writer.exec(
        r#"PRAGMA journal_mode=WAL;
           PRAGMA wal_autocheckpoint=0;
           CREATE TABLE t(n INTEGER, x REAL, s TEXT, z, w REAL, b BLOB);
           INSERT INTO t VALUES (1, 0.5, 'a"b\c' || char(10) || 'd é', NULL, 5.0, x'414243');"#,
    );
    (path, writer)
}

fn rows(count: u32, size: u32) -> String {
    format!(
        "INSERT INTO t(n, b) WITH RECURSIVE s(v) AS (SELECT 1 UNION ALL SELECT v + 1 FROM s WHERE v < {count})
         SELECT v, randomblob({size}) FROM s;"
    )
}

/// Every name in the folder with its bytes, the writer's shared-memory file included.
fn state(folder: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(folder)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_string_lossy().into_owned(), fs::read(e.path()).expect("cannot read a fixture file"))
        })
        .collect();
    files.sort();
    files
}

fn count(path: &Path) -> String {
    let bytes = checked_query(executable(), path, "SELECT count(*) AS c FROM t", WAIT).expect("no answer");
    String::from_utf8(bytes).unwrap()
}

fn later() -> Instant {
    Instant::now() + WAIT
}

fn direct(path: &Path, sql: &str, max: usize) -> Option<Vec<u8>> {
    query(path, sql, Instant::now() + WAIT, max)
}

fn linked_here(made: std::io::Result<()>) -> bool {
    match made {
        Ok(()) => true,
        Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => false,
        Err(e) => panic!("cannot create a link: {e}"),
    }
}

#[test]
fn a_log_being_written_is_read_in_the_sqlite_shell_json_form_and_left_as_it_was() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    let folder = path.parent().unwrap();
    let before = state(folder);
    let names: Vec<_> = before.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["agent.db", "agent.db-shm", "agent.db-wal"]);
    let sql = "PRAGMA query_only=ON;\nPRAGMA trusted_schema=OFF;\nSELECT * FROM t; -- done\n";
    let bytes = checked_query(executable(), &path, sql, WAIT).unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "[{\"n\":1,\"x\":0.5,\"s\":\"a\\\"b\\\\c\\u000ad é\",\"z\":null,\"w\":5.0,\"b\":\"ABC\"}]\n"
    );
    assert!(crate::json::Value::parse(&checked_query(executable(), &path, sql, WAIT).unwrap()).is_some());
    assert_eq!(checked_query(executable(), &path, "SELECT n FROM t WHERE n > 9", WAIT).unwrap(), b"");
    assert!(checked_query(executable(), &path, "INSERT INTO t(n) VALUES (3)", WAIT).is_none());
    assert!(checked_query(executable(), &path, "SELECT * FROM missing", WAIT).is_none());
    assert!(state(folder) == before, "a query changed the database's folder");

    writer.exec("INSERT INTO t(n) VALUES (2)");
    let before = state(folder);
    assert_eq!(count(&path), "[{\"c\":2}]\n");
    assert!(state(folder) == before, "a query changed the database's folder");
}

#[test]
fn committed_transactions_are_read_through_checkpoints_restarts_and_an_open_transaction() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    let wal = sidecar(&path, "-wal");
    let insert = |count: u32| writer.exec(&(rows(count, 3000) + "UPDATE t SET n = n + 1 WHERE rowid = 1;"));
    let expect = |rows: u32| assert_eq!(count(&path), format!("[{{\"c\":{rows}}}]\n"));

    insert(40);
    expect(41);
    writer.exec("PRAGMA wal_checkpoint(PASSIVE);");
    insert(40);
    expect(81);
    writer.exec("PRAGMA wal_checkpoint(RESTART);");
    insert(5);
    expect(86);
    writer.exec("PRAGMA wal_checkpoint(TRUNCATE);");
    assert_eq!(fs::metadata(&wal).unwrap().len(), 0);
    expect(86);

    let grown = fs::metadata(&wal).unwrap().len();
    writer.exec("PRAGMA cache_size=1; BEGIN;");
    insert(60);
    assert!(fs::metadata(&wal).unwrap().len() > grown, "the open transaction did not reach the log");
    expect(86);
    writer.exec("COMMIT;");
    expect(146);
}

/// The copy's boundary: while Milo holds its locks, a writer can still commit to the log, but no
/// checkpoint writes into the database file and the log cannot be emptied, so the copy either sees
/// the database as it was or starts again.
#[test]
fn a_checkpoint_cannot_write_the_database_while_a_copy_holds_its_locks() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    writer.exec("PRAGMA wal_checkpoint(TRUNCATE);");
    let wal = sidecar(&path, "-wal");
    assert_eq!(fs::metadata(&wal).unwrap().len(), 0);
    let (db_file, shm) = (fs_ops::open_read(&path).unwrap(), fs_ops::open_read(&sidecar(&path, "-shm")).unwrap());
    let database = fs::read(&path).unwrap();
    let locks = (read_lock(&db_file).unwrap(), hold(&shm, READ_LOCK_0, 1).unwrap());

    writer.exec(&(rows(20, 3000) + "UPDATE t SET n = 99 WHERE rowid = 1;"));
    writer.exec("PRAGMA wal_checkpoint(TRUNCATE);");
    writer.exec("PRAGMA wal_checkpoint(PASSIVE);");
    assert!(fs::read(&path).unwrap() == database, "a checkpoint wrote the database under the copy's locks");
    assert!(fs::metadata(&wal).unwrap().len() > 0, "the log was emptied under the copy's locks");
    assert!(matches!(copy(&path, MAX_SNAPSHOT, Instant::now() + WAIT), Attempt::Copied { .. }));

    drop(locks);
    writer.exec("PRAGMA wal_checkpoint(TRUNCATE);");
    assert_eq!(fs::metadata(&wal).unwrap().len(), 0);
    assert!(fs::read(&path).unwrap() != database);
    assert_eq!(count(&path), "[{\"c\":21}]\n");
}

#[test]
fn a_torn_frame_or_a_frame_from_another_log_ends_the_log_at_the_commit_before_it() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    let (db0, wal0) = (fs::read(&path).unwrap(), fs::read(sidecar(&path, "-wal")).unwrap());
    for _ in 0..4 {
        writer.exec(&rows(10, 100));
    }
    let (db, wal) = (fs::read(&path).unwrap(), fs::read(sidecar(&path, "-wal")).unwrap());
    assert_eq!(db, db0, "the fixture checkpointed");
    let page = usize::try_from(be32(&wal, 8)).unwrap();
    let frame = FRAME_HEADER + page;
    let earlier = fold(db.clone(), &wal0, MAX_SNAPSHOT, later()).unwrap();
    let whole = fold(db.clone(), &wal, MAX_SNAPSHOT, later()).unwrap();
    assert!(whole != earlier);

    let first_new = WAL_HEADER + (wal0.len() - WAL_HEADER) / frame * frame;
    let mut torn = wal.clone();
    torn[first_new + FRAME_HEADER] ^= 1;
    assert_eq!(fold(db.clone(), &torn, MAX_SNAPSHOT, later()).unwrap(), earlier);
    let mut foreign = wal.clone();
    foreign[first_new + 8] ^= 1;
    assert_eq!(fold(db.clone(), &foreign, MAX_SNAPSHOT, later()).unwrap(), earlier);
    assert!(fold(db.clone(), &wal, whole.len() as u64 - 1, later()).is_none());
    let mut rollback = db.clone();
    rollback[18] = 1;
    rollback[19] = 1;
    assert_eq!(fold(rollback.clone(), &wal, MAX_SNAPSHOT, later()).unwrap(), rollback);
    let mut version = wal.clone();
    version[4..8].copy_from_slice(&3_007_001_u32.to_be_bytes());
    let sum = checksum((0, 0), &version[..24], false);
    version[24..28].copy_from_slice(&sum.0.to_be_bytes());
    version[28..32].copy_from_slice(&sum.1.to_be_bytes());
    assert!(fold(db.clone(), &version, MAX_SNAPSHOT, later()).is_none(), "an unknown log version was skipped");
}

/// A valid log in little-endian checksum order with the given frames: page number, the database's
/// size in pages if the frame commits, and the byte the page is filled with.
fn log(page: usize, frames: &[(u32, u32, u8)]) -> Vec<u8> {
    let mut wal = Vec::new();
    for word in [0x377f_0682_u32, 3_007_000, u32::try_from(page).unwrap(), 0, 7, 9] {
        wal.extend_from_slice(&word.to_be_bytes());
    }
    let mut sum = checksum((0, 0), &wal, false);
    wal.extend_from_slice(&sum.0.to_be_bytes());
    wal.extend_from_slice(&sum.1.to_be_bytes());
    for &(number, commit, fill) in frames {
        let mut frame = Vec::new();
        for word in [number, commit, 7, 9] {
            frame.extend_from_slice(&word.to_be_bytes());
        }
        let data = vec![fill; page];
        sum = checksum(checksum(sum, &frame[..8], false), &data, false);
        frame.extend_from_slice(&sum.0.to_be_bytes());
        frame.extend_from_slice(&sum.1.to_be_bytes());
        wal.extend(frame);
        wal.extend(data);
    }
    wal
}

/// Three pages of 512 bytes in WAL mode, page 1 starting with a database header.
fn small_db() -> Vec<u8> {
    let mut db = vec![0; 3 * 512];
    db[..16].copy_from_slice(b"SQLite format 3\0");
    db[16..18].copy_from_slice(&512_u16.to_be_bytes());
    db[18] = 2;
    db[19] = 2;
    db
}

#[test]
fn a_crafted_log_is_folded_only_to_its_end_and_never_past_its_bounds() {
    let db = small_db();
    let three = log(512, &[(2, 0, 0xaa), (3, 3, 0xbb), (2, 3, 0xcc)]);
    let all = fold(db.clone(), &three, MAX_SNAPSHOT, later()).unwrap();
    assert!(all[512..1024].iter().all(|&b| b == 0xcc) && all[1024..].iter().all(|&b| b == 0xbb));
    assert_eq!((all[18], all[19]), (1, 1));
    let uncommitted = log(512, &[(2, 3, 0xaa), (3, 0, 0xbb)]);
    let folded = fold(db.clone(), &uncommitted, MAX_SNAPSHOT, later()).unwrap();
    assert!(folded[512..1024].iter().all(|&b| b == 0xaa) && folded[1024..].iter().all(|&b| b == 0));

    let shrunk = fold(db.clone(), &log(512, &[(3, 2, 0xbb)]), MAX_SNAPSHOT, later()).unwrap();
    assert_eq!(shrunk.len(), 2 * 512, "a page past the committed size is dropped");
    assert!(fold(db.clone(), &log(512, &[(0, 1, 0xbb)]), MAX_SNAPSHOT, later()).is_none());
    assert!(fold(db.clone(), &log(512, &[(2, u32::MAX, 0xbb)]), MAX_SNAPSHOT, later()).is_none());
    assert!(fold(db.clone(), &log(1024, &[(2, 3, 0xbb)]), MAX_SNAPSHOT, later()).is_none());
    assert_eq!(fold(db.clone(), &log(512, &[])[..20], MAX_SNAPSHOT, later()).unwrap()[100..], db[100..]);
    assert!(fold(db.clone(), &log(512, &[(1, 3, 0x11)]), MAX_SNAPSHOT, later()).is_none());
    assert!(fold(db[..99].to_vec(), &[], MAX_SNAPSHOT, later()).is_none());
    assert!(fold(db.clone(), &three, MAX_SNAPSHOT, Instant::now()).is_none(), "a passed deadline still folded");

    let mut write_only = db.clone();
    write_only[19] = 1;
    assert_eq!(fold(write_only, &three, MAX_SNAPSHOT, later()).unwrap()[100..], db[100..]);
    let mut unknown = db.clone();
    unknown[19] = 3;
    assert!(fold(unknown, &three, MAX_SNAPSHOT, later()).is_none());
    let mut sixty_four = vec![0; 65536];
    sixty_four[..100].copy_from_slice(&db[..100]);
    sixty_four[16..18].copy_from_slice(&1_u16.to_be_bytes());
    let grown = fold(sixty_four, &log(65536, &[(2, 2, 0x5a)]), MAX_SNAPSHOT, later()).unwrap();
    assert!(grown.len() == 2 * 65536 && grown[65536..].iter().all(|&b| b == 0x5a));
}

#[test]
fn the_read_lock_is_the_one_sqlite_honours_and_a_hot_journal_is_never_copied() {
    let f = Fixture::folder();
    let path = f.root.join("journal.db");
    let writer = Writer::open(&path);
    writer.exec("CREATE TABLE t(n); INSERT INTO t VALUES (1);");
    let file = fs_ops::open_read(&path).unwrap();
    let held = read_lock(&file).unwrap();
    assert!(!writer.try_exec("INSERT INTO t VALUES (2)"), "a writer changed the database under the read lock");
    drop(held);
    writer.exec("INSERT INTO t VALUES (2)");

    writer.exec("BEGIN EXCLUSIVE;");
    assert!(snapshot(&path, MAX_SNAPSHOT, Instant::now() + WAIT).is_none());
    writer.exec("COMMIT;");
    assert_eq!(count(&path), "[{\"c\":2}]\n");

    fs::write(sidecar(&path, "-journal"), [0xd9, 0xd5, 0x05, 0xf9]).unwrap();
    assert!(snapshot(&path, MAX_SNAPSHOT, Instant::now() + WAIT).is_none(), "a hot journal was ignored");
    fs::write(sidecar(&path, "-journal"), [0; 28]).unwrap();
    assert_eq!(count(&path), "[{\"c\":2}]\n");
}

#[test]
fn a_writer_that_closes_during_a_copy_leaves_its_log() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    let file = fs_ops::open_read(&path).unwrap();
    let held = read_lock(&file).unwrap();
    drop(writer);
    assert!(sidecar(&path, "-wal").exists(), "the closing writer checked in and deleted its log");
    drop(held);
    assert_eq!(count(&path), "[{\"c\":1}]\n");
}

#[test]
fn a_log_without_its_index_or_a_database_closed_cleanly_is_read_and_nothing_is_made() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    writer.exec("INSERT INTO t(n) VALUES (2)");
    let copied = f.root.join("copied");
    fs::create_dir(&copied).unwrap();
    let copy = copied.join("copy.db");
    fs::write(&copy, fs::read(&path).unwrap()).unwrap();
    fs::write(sidecar(&copy, "-wal"), fs::read(sidecar(&path, "-wal")).unwrap()).unwrap();
    let before = state(&copied);
    assert_eq!(count(&copy), "[{\"c\":2}]\n");
    assert!(state(&copied) == before, "a query made a file beside the copy");

    drop(writer);
    let folder = path.parent().unwrap();
    let before = state(folder);
    assert_eq!(before.len(), 1, "the writer left its sidecars");
    assert_eq!(count(&path), "[{\"c\":2}]\n");
    assert!(state(folder) == before, "a query made a file beside a closed database");
}

/// An index left behind by a process that stopped describes an older state than its log; the
/// copy follows the log, as SQLite's recovery does, and never trusts the index.
#[test]
fn a_stale_index_beside_a_later_log_is_not_trusted() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    let stale = fs::read(sidecar(&path, "-shm")).unwrap();
    writer.exec(&rows(30, 500));
    let copied = f.root.join("stale");
    fs::create_dir(&copied).unwrap();
    let copy = copied.join("copy.db");
    fs::write(&copy, fs::read(&path).unwrap()).unwrap();
    fs::write(sidecar(&copy, "-wal"), fs::read(sidecar(&path, "-wal")).unwrap()).unwrap();
    fs::write(sidecar(&copy, "-shm"), &stale).unwrap();
    let before = state(&copied);
    assert_eq!(count(&copy), "[{\"c\":31}]\n");
    assert!(state(&copied) == before, "a query changed the copied files");
}

#[test]
fn text_in_a_utf16_database_is_written_as_utf8() {
    let f = Fixture::folder();
    let path = f.root.join("utf16.db");
    let writer = Writer::open(&path);
    writer.exec("PRAGMA encoding='UTF-16le'; CREATE TABLE p(provider TEXT); INSERT INTO p VALUES ('anthropic é');");
    drop(writer);
    let bytes = checked_query(executable(), &path, "SELECT provider FROM p", WAIT).unwrap();
    assert_eq!(String::from_utf8(bytes).unwrap(), "[{\"provider\":\"anthropic é\"}]\n");
}

#[test]
fn a_link_to_a_database_or_its_log_is_refused_and_nothing_is_made_beside_it() {
    let f = Fixture::folder();
    let (path, _writer) = busy(&f);
    let link = f.root.join("link.db");
    if !linked_here(std::os::windows::fs::symlink_file(&path, &link)) {
        return;
    }
    assert!(checked_query(executable(), &link, "SELECT 1", WAIT).is_none());
    assert!(!sidecar(&link, "-wal").exists() && !sidecar(&link, "-shm").exists());

    let copy = f.root.join("copy.db");
    fs::write(&copy, fs::read(&path).unwrap()).unwrap();
    std::os::windows::fs::symlink_file(sidecar(&path, "-wal"), sidecar(&copy, "-wal")).unwrap();
    assert!(direct(&copy, "SELECT 1", MAX_OUTPUT).is_none());
}

#[test]
fn time_output_and_size_are_bounded() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    let started = Instant::now();
    let endless = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT max(i) FROM n";
    assert!(checked_query(executable(), &path, endless, Duration::from_millis(200)).is_none());
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());

    let many = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n LIMIT 1000) SELECT i FROM n";
    assert!(direct(&path, many, 100).is_none());
    assert_eq!(direct(&path, many, MAX_OUTPUT).unwrap().iter().filter(|&&b| b == b'{').count(), 1000);
    let controls = "SELECT replace(hex(zeroblob(6000)), '0', char(1)) AS v";
    assert!(direct(&path, controls, MAX_OUTPUT).is_none(), "72 kB of escapes passed a 64 KiB limit");
    let exact = direct(&path, "SELECT 'x' AS v", MAX_OUTPUT).unwrap();
    assert_eq!(direct(&path, "SELECT 'x' AS v", exact.len()).unwrap(), exact);
    assert!(direct(&path, "SELECT 'x' AS v", exact.len() - 1).is_none());

    writer.exec("INSERT INTO t(b) VALUES (randomblob(100000));");
    writer.exec("PRAGMA wal_checkpoint(TRUNCATE);");
    assert!(snapshot(&path, 50_000, Instant::now() + WAIT).is_none());
    assert!(snapshot(&path, MAX_SNAPSHOT, Instant::now() + WAIT).is_some());
    assert!(snapshot(&path, MAX_SNAPSHOT, Instant::now()).is_none(), "a passed deadline still copied");
    assert!(!too_large(&path));

    let large = f.root.join("large.db");
    fs::write(&large, fs::read(&path).unwrap()).unwrap();
    fs::OpenOptions::new().write(true).open(&large).unwrap().set_len(MAX_SNAPSHOT + 1).unwrap();
    assert!(too_large(&large));
    assert!(direct(&large, "SELECT 1", MAX_OUTPUT).is_none());
}
