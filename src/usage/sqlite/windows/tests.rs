//! The Windows query against databases made by Windows' own SQLite in a scratch folder: a log that
//! is being written, the sqlite3 shell's JSON form, links, a missing sidecar, time and size limits.

use std::fs;
use std::time::Duration;

use super::super::{MAX_OUTPUT, executable, query as checked_query, read_only_uri};
use super::*;
use crate::fixture::Fixture;

const WAIT: Duration = Duration::from_secs(30);
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

/// A database in WAL mode, in a folder whose name needs escaping, with its writer still open.
fn busy(f: &Fixture) -> (std::path::PathBuf, Writer) {
    let folder = f.root.join("Agent data é #1");
    fs::create_dir(&folder).unwrap();
    let path = folder.join("agent.db");
    let writer = Writer::open(&path);
    writer.exec(
        r#"PRAGMA journal_mode=WAL;
           CREATE TABLE t(n INTEGER, x REAL, s TEXT, z, w REAL, b BLOB);
           INSERT INTO t VALUES (1, 0.5, 'a"b\c' || char(10) || 'd é', NULL, 5.0, x'414243');"#,
    );
    (path, writer)
}

fn files(folder: &std::path::Path) -> Vec<String> {
    let mut names: Vec<_> =
        fs::read_dir(folder).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn direct(path: &std::path::Path, sql: &str, wait: Duration, max: usize) -> Option<Vec<u8>> {
    query(path, &read_only_uri(path), sql, Instant::now() + wait, max)
}

fn linked_here(made: std::io::Result<()>) -> bool {
    match made {
        Ok(()) => true,
        Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => false,
        Err(e) => panic!("cannot create a link: {e}"),
    }
}

#[test]
fn a_log_being_written_is_read_in_the_sqlite_shell_json_form() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    let before = files(path.parent().unwrap());
    assert_eq!(before, ["agent.db", "agent.db-shm", "agent.db-wal"]);
    let sql = "PRAGMA query_only=ON;\nPRAGMA trusted_schema=OFF;\nSELECT * FROM t; -- done\n";
    let bytes = checked_query(executable(), &path, sql, WAIT).unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "[{\"n\":1,\"x\":0.5,\"s\":\"a\\\"b\\\\c\\u000ad é\",\"z\":null,\"w\":5.0,\"b\":\"ABC\"}]\n"
    );
    assert!(crate::json::Value::parse(&checked_query(executable(), &path, sql, WAIT).unwrap()).is_some());

    writer.exec("INSERT INTO t(n) VALUES (2)");
    let count = checked_query(executable(), &path, "SELECT count(*) AS c FROM t", WAIT).unwrap();
    assert_eq!(count, b"[{\"c\":2}]\n");
    assert_eq!(checked_query(executable(), &path, "SELECT n FROM t WHERE n > 9", WAIT).unwrap(), b"");
    assert!(checked_query(executable(), &path, "INSERT INTO t(n) VALUES (3)", WAIT).is_none());
    assert!(checked_query(executable(), &path, "SELECT * FROM missing", WAIT).is_none());
    assert_eq!(files(path.parent().unwrap()), before);
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
    assert!(direct(&link, "SELECT 1", WAIT, MAX_OUTPUT).is_none());
    assert!(!f.root.join("link.db-wal").exists() && !f.root.join("link.db-shm").exists());

    let copy = f.root.join("copy.db");
    fs::write(&copy, fs::read(&path).unwrap()).unwrap();
    fs::write(f.root.join("copy.db-shm"), fs::read(path.with_extension("db-shm")).unwrap()).unwrap();
    std::os::windows::fs::symlink_file(path.with_extension("db-wal"), f.root.join("copy.db-wal")).unwrap();
    assert!(direct(&copy, "SELECT 1", WAIT, MAX_OUTPUT).is_none());
}

#[test]
fn a_log_without_its_shared_memory_file_is_not_given_one() {
    let f = Fixture::folder();
    let (path, _writer) = busy(&f);
    let copy = f.root.join("copy.db");
    fs::write(&copy, fs::read(&path).unwrap()).unwrap();
    fs::write(f.root.join("copy.db-wal"), fs::read(path.with_extension("db-wal")).unwrap()).unwrap();
    assert!(checked_query(executable(), &copy, "SELECT 1", WAIT).is_none());
    let _ = direct(&copy, "SELECT 1", WAIT, MAX_OUTPUT);
    assert!(!f.root.join("copy.db-shm").exists(), "a read-only query made a shared-memory file");
}

#[test]
fn a_query_past_its_deadline_is_interrupted() {
    let f = Fixture::folder();
    let (path, _writer) = busy(&f);
    let started = Instant::now();
    let endless = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT max(i) FROM n";
    assert!(checked_query(executable(), &path, endless, Duration::from_millis(200)).is_none());
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
}

#[test]
fn output_past_the_limit_is_refused() {
    let f = Fixture::folder();
    let (path, _writer) = busy(&f);
    let many = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n LIMIT 1000) SELECT i FROM n";
    assert!(direct(&path, many, WAIT, 100).is_none());
    assert_eq!(direct(&path, many, WAIT, MAX_OUTPUT).unwrap().iter().filter(|&&b| b == b'{').count(), 1000);
}
