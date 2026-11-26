//! The Windows query against benign databases made by Windows' own SQLite in a scratch folder. A
//! database is read only while no connection uses it through a write-ahead log, and its files and
//! folder must read the same after every query as before it.

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
           CREATE TABLE t(n INTEGER, x REAL, s TEXT, z, w REAL, b BLOB);
           INSERT INTO t VALUES (1, 0.5, 'a"b\c' || char(10) || 'd é', NULL, 5.0, x'414243');"#,
    );
    (path, writer)
}

/// The same database once its writer has closed it, which checks the log in and removes it.
fn closed(f: &Fixture) -> PathBuf {
    let (path, writer) = busy(f);
    drop(writer);
    assert_eq!(state(path.parent().unwrap()).len(), 1, "the writer left its sidecars");
    path
}

/// Every name in the folder with its bytes.
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

fn count(path: &Path) -> Option<String> {
    let bytes = checked_query(executable(), path, "SELECT count(*) AS c FROM t", WAIT)?;
    Some(String::from_utf8(bytes).unwrap())
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
fn a_closed_database_is_read_in_the_sqlite_shell_json_form_and_left_as_it_was() {
    let f = Fixture::folder();
    let path = closed(&f);
    let folder = path.parent().unwrap();
    let before = state(folder);
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
}

#[test]
fn a_database_in_use_through_its_log_is_left_unread_and_untouched() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    let folder = path.parent().unwrap();
    let before = state(folder);
    assert_eq!(before.len(), 3);
    assert_eq!(count(&path), None);
    assert!(state(folder) == before, "a query changed a database in use");

    drop(writer);
    assert_eq!(count(&path).as_deref(), Some("[{\"c\":1}]\n"));
}

#[test]
fn a_log_or_an_index_left_beside_a_database_leaves_it_unread() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    writer.exec("INSERT INTO t(n) VALUES (2)");
    for (name, sidecars) in [("log", &["-wal"][..]), ("index", &["-shm"][..]), ("both", &["-wal", "-shm"][..])] {
        let copied = f.root.join(name);
        fs::create_dir(&copied).unwrap();
        let copy = copied.join("copy.db");
        fs::write(&copy, fs::read(&path).unwrap()).unwrap();
        for suffix in sidecars {
            fs::write(sidecar(&copy, suffix), fs::read(sidecar(&path, suffix)).unwrap()).unwrap();
        }
        let before = state(&copied);
        assert_eq!(count(&copy), None, "a database beside a {name} was read");
        assert!(state(&copied) == before, "a query changed the files beside a {name}");
    }
}

#[test]
fn a_writer_that_closes_during_a_copy_leaves_its_log_and_the_database_unread() {
    let f = Fixture::folder();
    let (path, writer) = busy(&f);
    let file = fs_ops::open_read(&path).unwrap();
    let held = read_lock(&file).unwrap();
    drop(writer);
    assert!(sidecar(&path, "-wal").exists(), "the closing writer checked in and deleted its log");
    drop(held);
    assert_eq!(count(&path), None);
}

#[test]
fn a_writer_that_opens_under_the_read_lock_leaves_the_database_unread() {
    let f = Fixture::folder();
    let path = closed(&f);
    let file = fs_ops::open_read(&path).unwrap();
    let held = read_lock(&file).unwrap();
    let writer = Writer::open(&path);
    writer.exec("INSERT INTO t(n) VALUES (2)");
    assert!(matches!(copy(&path, MAX_SNAPSHOT, Instant::now() + WAIT), Attempt::Unreadable));
    drop(writer);
    assert!(sidecar(&path, "-wal").exists(), "the writer removed its log under the read lock");
    drop(held);
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
    assert_eq!(count(&path).as_deref(), Some("[{\"c\":2}]\n"), "an idle rollback connection keeps nothing unread");

    writer.exec("BEGIN EXCLUSIVE;");
    assert!(snapshot(&path, MAX_SNAPSHOT, Instant::now() + WAIT).is_none());
    writer.exec("COMMIT;");
    assert_eq!(count(&path).as_deref(), Some("[{\"c\":2}]\n"));

    fs::write(sidecar(&path, "-journal"), [0xd9, 0xd5, 0x05, 0xf9]).unwrap();
    assert!(snapshot(&path, MAX_SNAPSHOT, Instant::now() + WAIT).is_none(), "a hot journal was ignored");
    fs::write(sidecar(&path, "-journal"), [0; 28]).unwrap();
    assert_eq!(count(&path).as_deref(), Some("[{\"c\":2}]\n"));
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
fn a_header_sqlite_would_not_open_is_never_queried() {
    let f = Fixture::folder();
    let path = closed(&f);
    let good = fs::read(&path).unwrap();
    assert!(normalized(good.clone()).is_some_and(|copy| copy[18] == 1 && copy[19] == 1 && copy[20..] == good[20..]));
    for (at, value) in [(0, b'X'), (16, 0), (17, 3), (18, 3), (19, 3)] {
        let mut bad = good.clone();
        bad[at] = value;
        assert!(normalized(bad).is_none(), "byte {at} set to {value} was accepted");
    }
    assert!(normalized(good[..99].to_vec()).is_none());
}

#[test]
fn a_link_to_a_database_is_refused_and_nothing_is_made_beside_it() {
    let f = Fixture::folder();
    let path = closed(&f);
    let link = f.root.join("link.db");
    if !linked_here(std::os::windows::fs::symlink_file(&path, &link)) {
        return;
    }
    assert_eq!(count(&link), None);
    assert!(!sidecar(&link, "-wal").exists() && !sidecar(&link, "-shm").exists());
    assert_eq!(count(&path).as_deref(), Some("[{\"c\":1}]\n"));
}

#[test]
fn time_output_and_size_are_bounded() {
    let f = Fixture::folder();
    let path = closed(&f);
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

    let size = fs::metadata(&path).unwrap().len();
    assert!(snapshot(&path, size - 1, Instant::now() + WAIT).is_none());
    assert!(snapshot(&path, size, Instant::now() + WAIT).is_some());
    assert!(snapshot(&path, MAX_SNAPSHOT, Instant::now()).is_none(), "a passed deadline still copied");
    assert!(!too_large(&path));

    let large = f.root.join("large.db");
    fs::write(&large, fs::read(&path).unwrap()).unwrap();
    fs::OpenOptions::new().write(true).open(&large).unwrap().set_len(MAX_SNAPSHOT + 1).unwrap();
    assert!(too_large(&large));
    assert!(direct(&large, "SELECT 1", MAX_OUTPUT).is_none());
}
