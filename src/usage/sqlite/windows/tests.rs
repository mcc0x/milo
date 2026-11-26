//! The Windows query against benign databases made by Windows' own SQLite in a scratch folder: a
//! log that is being written, checkpoints and restarts, a missing sidecar, links, and limits. The
//! database's files and folder must read the same after every query as before it.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use super::super::{MAX_OUTPUT, executable, query as checked_query};
use super::*;
use crate::fixture::Fixture;

const WAIT: Duration = Duration::from_secs(30);
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

/// A database in WAL mode, in a folder whose name needs escaping, with its writer still open.
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

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Every name in the folder with its bytes. The writer's shared-memory file is read like the rest.
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
    let insert = |rows: u32| {
        writer.exec(&format!(
            "INSERT INTO t(n, b) WITH RECURSIVE s(value) AS (SELECT 1 UNION ALL SELECT value + 1 FROM s WHERE value < {rows})
             SELECT value, randomblob(3000) FROM s;
             UPDATE t SET n = n + 1 WHERE rowid = 1;"
        ));
    };
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

#[test]
fn a_torn_frame_ends_the_log_there() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    for _ in 0..4 {
        writer.exec("INSERT INTO t(n, b) WITH RECURSIVE s(value) AS (SELECT 1 UNION ALL SELECT value + 1 FROM s WHERE value < 10)
             SELECT value, randomblob(100) FROM s;");
    }
    let (db, wal) = (fs::read(&path).unwrap(), fs::read(sidecar(&path, "-wal")).unwrap());
    let whole = fold(db.clone(), &wal).unwrap();
    assert!(whole.len() > db.len());
    let page = usize::try_from(be32(&wal, 8)).unwrap();
    let mut torn = wal.clone();
    let last = WAL_HEADER + (wal.len() - WAL_HEADER) / (FRAME_HEADER + page) * (FRAME_HEADER + page);
    torn[last - 1] ^= 1;
    assert!(fold(db.clone(), &torn).unwrap() != whole);
    let mut foreign = wal.clone();
    foreign[16] ^= 1;
    assert_eq!(fold(db.clone(), &foreign).unwrap()[100..], db[100..]);
}

#[test]
fn a_log_without_its_shared_memory_file_or_a_database_closed_cleanly_is_read_and_nothing_is_made() {
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

    writer.exec("INSERT INTO t(b) VALUES (randomblob(100000));");
    assert!(snapshot(&path, 50_000, Instant::now() + WAIT).is_none());
    assert!(snapshot(&path, MAX_SNAPSHOT, Instant::now() + WAIT).is_some());
}
