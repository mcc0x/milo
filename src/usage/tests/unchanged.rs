//! Checks when the pane may leave a database unread because its files are
//! as the last read left them, and every way that must stop being true.

use std::process::{Child, Command, Stdio};

use super::idle::{history, last_written, steps};
use super::omp_limits::{report, sqlite, wait_for, window};
use super::opencode_activity::{Database, RELEASES, busy_day};
use super::sqlite::Poll;
use super::*;

/// The pane's looks at one database, one read interval apart.
struct Looks<T> {
    poll: Poll<T>,
    clock: Instant,
}

impl<T: Send + 'static> Looks<T> {
    fn new(poll: Poll<T>) -> Self {
        Self { poll, clock: Instant::now() }
    }

    /// What the next look at `now` read, or None when it asked SQLite nothing.
    fn look(&mut self, now: i64) -> Option<Option<T>> {
        self.clock += Duration::from_secs(20);
        self.poll.start(now, self.clock);
        self.finish()
    }

    fn finish(&mut self) -> Option<Option<T>> {
        self.poll.fd()?;
        let mut read = None;
        wait_for(|| {
            read = self.poll.collect();
            read.is_some()
        });
        read
    }
}

/// OMP's database with one saved five-hour figure, last written a minute before `NOW`.
fn omp_at(f: &Fixture, fraction: &str) -> Option<(PathBuf, Looks<omp::Reports>)> {
    let path = sqlite(f)?;
    history(&path, 1, (NOW - 60) * 1000, fraction);
    last_written(&path, NOW - 60);
    Some((path.clone(), Looks::new(omp::poll(Some(path), TEST_WAIT))))
}

fn percent(read: Option<Option<omp::Reports>>) -> Option<f64> {
    read??.claude?.windows.base[0].map(|window| window.percent)
}

#[test]
fn an_idle_database_is_read_once_and_then_only_at_the_backstop() {
    let f = Fixture::folder();
    let Some((_, mut looks)) = omp_at(&f, "0.5") else { return };
    assert_eq!(percent(looks.look(NOW)), Some(50.0));
    // Five minutes of looks, twenty seconds apart, after the one that read.
    for after in 1..15 {
        assert!(looks.look(NOW + after * 20).is_none(), "look {after} read an unchanged database");
    }
    assert_eq!(percent(looks.look(NOW + 300)), Some(50.0), "the backstop reads whatever the files say");
    assert!(looks.look(NOW + 320).is_none());
}

#[test]
fn a_write_is_read_at_the_next_look_and_again_until_the_file_has_been_left_alone() {
    let f = Fixture::folder();
    let Some((path, mut looks)) = omp_at(&f, "0.5") else { return };
    assert_eq!(percent(looks.look(NOW)), Some(50.0));
    assert!(looks.look(NOW + 20).is_none());

    history(&path, 1, (NOW + 30) * 1000, "0.77");
    last_written(&path, NOW + 39);
    assert_eq!(percent(looks.look(NOW + 40)), Some(77.0));
    // Written a second before that look: another write could carry the same time.
    assert_eq!(percent(looks.look(NOW + 60)), Some(77.0));
    assert!(looks.look(NOW + 80).is_none());
}

#[test]
fn a_figure_dated_ahead_of_the_clock_is_read_when_it_comes_due_without_a_write() {
    let f = Fixture::folder();
    let Some((path, mut looks)) = omp_at(&f, "0.5") else { return };
    history(&path, 1, (NOW + CLOCK_SKEW + 100) * 1000, "0.9");
    last_written(&path, NOW - 60);
    assert_eq!(percent(looks.look(NOW)), Some(50.0));
    assert!(looks.look(NOW + 99).is_none());
    assert_eq!(percent(looks.look(NOW + 100)), Some(90.0));
}

#[test]
fn a_report_dated_ahead_of_the_clock_is_read_when_it_comes_due_without_a_write() {
    let f = Fixture::folder();
    let Some((path, mut looks)) = omp_at(&f, "0.5") else { return };
    let due = NOW + CLOCK_SKEW + 100;
    let five = window("anthropic:5h", "0.9", (NOW + DAY) * 1000, 300, "null");
    report(&path, "bench", "anthropic", due * 1000, &format!("[{five}]"));
    last_written(&path, NOW - 60);
    assert_eq!(percent(looks.look(NOW)), Some(50.0));
    assert!(looks.look(NOW + 99).is_none());
    assert_eq!(percent(looks.look(NOW + 100)), Some(90.0));
}

#[test]
fn a_clock_set_back_or_a_manual_reload_reads_a_settled_database() {
    let f = Fixture::folder();
    let Some((_, mut looks)) = omp_at(&f, "0.5") else { return };
    assert_eq!(percent(looks.look(NOW)), Some(50.0));
    assert!(looks.look(NOW + 20).is_none());
    assert_eq!(percent(looks.look(NOW - 30)), Some(50.0));
    assert!(looks.look(NOW - 10).is_none());
    looks.poll.reload(NOW - 9, looks.clock);
    assert_eq!(percent(looks.finish()), Some(50.0));
}

#[test]
fn a_replaced_or_removed_database_is_read_and_a_failed_read_settles_nothing() {
    let f = Fixture::folder();
    let Some((path, mut looks)) = omp_at(&f, "0.5") else { return };
    assert_eq!(percent(looks.look(NOW)), Some(50.0));
    assert!(looks.look(NOW + 20).is_none());

    // The same bytes, size and time under the same name, in another file.
    let other = f.root.join("other.db");
    fs::copy(&path, &other).unwrap();
    last_written(&other, NOW - 60);
    fs::rename(&other, &path).unwrap();
    assert_eq!(percent(looks.look(NOW + 40)), Some(50.0), "a file put in the database's place is read");
    assert!(looks.look(NOW + 60).is_none());

    // A write that leaves the file its size and, as far as its times say, untouched.
    let size = fs::metadata(&path).unwrap().len();
    exec(&path, "UPDATE usage_history SET used_fraction=0.2;");
    last_written(&path, NOW - 60);
    assert_eq!(fs::metadata(&path).unwrap().len(), size);
    assert_eq!(percent(looks.look(NOW + 64)), Some(20.0), "the database's own change counter moved");
    assert!(looks.look(NOW + 68).is_none());

    exec(&path, "ALTER TABLE usage_history RENAME COLUMN used_fraction TO fraction;");
    last_written(&path, NOW - 60);
    for now in [NOW + 80, NOW + 100] {
        assert!(matches!(looks.look(now), Some(None)), "a database that cannot be read is tried at every look");
    }

    fs::remove_file(&path).unwrap();
    for now in [NOW + 120, NOW + 140] {
        let cleared = looks.look(now).flatten().expect("a missing database clears the reports");
        assert!(cleared.claude.is_none());
    }
}

/// A tool that keeps its database open in write-ahead mode and saves a
/// figure each time it is told to.
struct Writer {
    child: Child,
    folder: PathBuf,
    saved: u32,
}

impl Writer {
    fn open(f: &Fixture, path: &Path) -> Self {
        let child = Command::new(sqlite::executable())
            .args(["-batch", "-bail", "-init", "/dev/null"])
            .current_dir(&f.root)
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut writer = Self { child, folder: f.root.clone(), saved: 0 };
        writer.run("PRAGMA journal_mode=WAL;");
        writer
    }

    /// Runs a statement and waits until the tool has committed it.
    fn run(&mut self, sql: &str) {
        self.saved += 1;
        let done = format!("saved-{}", self.saved);
        writeln!(self.child.stdin.as_mut().unwrap(), "{sql}\n.once {done}\nSELECT 1;").unwrap();
        wait_for(|| self.folder.join(&done).exists());
    }

    fn save(&mut self, at: i64, fraction: &str) {
        self.run(&format!(
            "INSERT INTO usage_history(provider, account_key, recorded_at, limit_id, used_fraction, window_label)
             VALUES ('anthropic','oauth|account:bench',{},'anthropic:5h',{fraction},'5 Hours');",
            at * 1000
        ));
    }

    /// The tool exits: SQLite moves the log into the database and removes it.
    fn close(mut self) {
        drop(self.child.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

#[test]
fn a_write_ahead_log_that_appears_grows_or_goes_is_read() {
    let f = Fixture::folder();
    let Some((path, mut looks)) = omp_at(&f, "0.5") else { return };
    let log = f.root.join("agent.db-wal");
    let left_alone = |at: i64| [&path, &log].into_iter().filter(|file| file.exists()).for_each(|f| last_written(f, at));
    assert_eq!(percent(looks.look(NOW)), Some(50.0));
    assert!(looks.look(NOW + 20).is_none());

    let mut tool = Writer::open(&f, &path);
    tool.save(NOW + 30, "0.6");
    assert!(log.exists());
    left_alone(NOW - 60);
    assert_eq!(percent(looks.look(NOW + 40)), Some(60.0), "a log appeared beside a database dated as before");
    assert!(looks.look(NOW + 60).is_none());

    tool.save(NOW + 70, "0.7");
    left_alone(NOW - 60);
    assert_eq!(percent(looks.look(NOW + 80)), Some(70.0), "the log grew");
    assert!(looks.look(NOW + 100).is_none());

    // Some builds of SQLite leave the log when the last tool closes; most remove it and its index.
    tool.close();
    for sidecar in [&log, &f.root.join("agent.db-shm")] {
        let _ = fs::remove_file(sidecar);
    }
    left_alone(NOW - 60);
    for now in [NOW + 120, NOW + 140] {
        assert!(matches!(looks.look(now), Some(None)), "a database missing its log is tried at every look");
    }
}

fn steps_today(read: Option<Option<opencode::Report>>) -> Option<u64> {
    match read?? {
        opencode::Report::Today(activity) => Some(activity.steps),
        _ => None,
    }
}

#[test]
fn opencode_totals_are_read_again_when_the_local_day_ends_or_a_step_dated_ahead_comes_due() {
    if !sqlite_installed() {
        return;
    }
    let f = Fixture::folder();
    let noon = local_noon();
    let (_, schema, store) = RELEASES[3];
    let db = Database::new(&f, schema, store);
    db.session("bench", "2.3.2", noon - 3600, None);
    steps(&db.path, 0, 1, (noon - 60) * 1000);
    steps(&db.path, 1, 1, (noon + 50) * 1000);
    last_written(&db.path, noon - 60);
    let mut looks = Looks::new(opencode::poll(Some(db.path.clone()), TEST_WAIT));

    assert_eq!(steps_today(looks.look(noon)), Some(1));
    assert!(looks.look(noon + 49).is_none());
    assert_eq!(steps_today(looks.look(noon + 50)), Some(2));
    assert!(looks.look(noon + 70).is_none());

    let tomorrow = Today::at(noon + DAY, when::local_day(noon + DAY)).since;
    assert!(looks.look(tomorrow - 1).is_none());
    assert_eq!(steps_today(looks.look(tomorrow)), Some(0), "yesterday's steps are not today's");
}

#[test]
fn every_generation_of_opencode_tables_says_when_a_step_dated_ahead_comes_due() {
    if !sqlite_installed() {
        return;
    }
    let noon = local_noon();
    for release in RELEASES {
        let f = Fixture::folder();
        let db = busy_day(&f, release, noon);
        last_written(&db.path, noon - 60);
        let mut looks = Looks::new(opencode::poll(Some(db.path.clone()), TEST_WAIT));
        assert_eq!(steps_today(looks.look(noon)), Some(2), "OpenCode {}", release.0);
        assert!(looks.look(noon + HOUR - 1).is_none(), "OpenCode {}", release.0);
        assert_eq!(steps_today(looks.look(noon + HOUR)), Some(3), "OpenCode {}", release.0);
    }
}

#[test]
fn totals_read_for_another_local_day_are_read_again_though_nothing_was_written() {
    if !sqlite_installed() {
        return;
    }
    let f = Fixture::folder();
    let noon = local_noon();
    let db = busy_day(&f, RELEASES[3], noon);
    last_written(&db.path, noon - 60);
    let mut usage = unopened(&f, noon);
    usage.opencode = OpenCode::new(Some(db.path.clone()), TEST_WAIT);
    let clock = Instant::now();
    let look = |usage: &mut Usage, after: u32| {
        usage.refresh(noon, clock + Duration::from_secs(20) * after, false);
        let read = usage.opencode.poll.fd().is_some();
        wait_for(|| {
            usage.check_external();
            usage.external_fd().is_none()
        });
        read
    };
    assert!(look(&mut usage, 0));
    assert!(!look(&mut usage, 1));
    // The pane's day moved under totals that were read for the day before, as a time zone can move it.
    let opencode::Report::Today(read) = &mut usage.opencode.report else { panic!("no totals") };
    read.since -= DAY;
    assert!(look(&mut usage, 2));
}
