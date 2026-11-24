//! Checks when the pane may leave a database unread because its files are
//! as the last read left them, and every way that must stop being true.

use std::process::{Child, Command, Stdio};

use super::idle::{history, last_written, steps, steps_sql};
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
pub(super) struct Writer {
    child: Child,
    folder: PathBuf,
    saved: u32,
}

impl Writer {
    pub(super) fn open(f: &Fixture, path: &Path) -> Self {
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
    pub(super) fn run(&mut self, sql: &str) {
        self.saved += 1;
        let done = format!("saved-{}-{}", self.child.id(), self.saved);
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
    pub(super) fn close(mut self) {
        drop(self.child.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

#[test]
fn limits_in_a_database_with_a_write_ahead_log_are_read_at_every_look() {
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
    // However long its files have been left alone, nothing here vouches for what a reader sees.
    for now in [NOW + 40, NOW + 60, NOW + 80] {
        assert_eq!(percent(looks.look(now)), Some(60.0), "a limit was left unread beside a write-ahead log");
    }

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

#[test]
fn opencode_activity_beside_a_write_ahead_log_is_read_when_a_write_moves_its_times() {
    if !sqlite_installed() {
        return;
    }
    let f = Fixture::folder();
    let noon = local_noon();
    let (_, schema, store) = RELEASES[3];
    let db = Database::new(&f, schema, store);
    db.session("bench", "2.3.2", noon - 3600, None);
    steps(&db.path, 0, 1, (noon - 60) * 1000);
    let log = f.root.join("opencode.db-wal");
    // The fixture's writes happen as the test runs; date each as the pane's clock would have seen it.
    let written = |at: i64| [&db.path, &log].into_iter().filter(|file| file.exists()).for_each(|f| last_written(f, at));
    written(noon - 60);
    let mut looks = Looks::new(opencode::poll(Some(db.path.clone()), TEST_WAIT));
    assert_eq!(steps_today(looks.look(noon)), Some(1));
    assert!(looks.look(noon + 20).is_none());

    // OpenCode keeps the database open in write-ahead mode and writes its steps
    // itself. There the database's change counter stands still, and where a write
    // leaves both files their sizes only their times say that it happened.
    let mut tool = Writer::open(&f, &db.path);
    for (step, at) in [(1, noon + 30), (2, noon + 70)] {
        tool.run(&steps_sql(step, 1, (noon - 50) * 1000));
        written(at);
        assert_eq!(steps_today(looks.look(at + 10)), Some(u64::from(step) + 1), "a step written at {at} was not read");
        assert!(looks.look(at + 30).is_none());
    }
    tool.close();
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
fn opencode_reads_the_earliest_legacy_message_dated_ahead_with_or_without_a_time_index() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let alternate_indexes = [
        None,
        Some(""),
        Some("CREATE INDEX message_session_idx ON message(session_id);"),
        Some("CREATE INDEX message_time_idx ON message(session_id COLLATE NOCASE, time_created);"),
        Some("CREATE INDEX message_time_idx ON message(session_id, time_created) WHERE time_created > 0;"),
    ];
    for release in [RELEASES[1], RELEASES[4]] {
        for alternate in alternate_indexes {
            let f = Fixture::folder();
            let db = busy_day(&f, release, now);
            if let Some(create) = alternate {
                exec(&db.path, &format!("DROP INDEX message_session_time_created_id_idx; {create}"));
            } else {
                // An orphan cannot affect totals, even though it comes due first.
                exec(
                    &db.path,
                    &format!(
                        "PRAGMA foreign_keys=OFF;
                         INSERT INTO message(id,session_id,time_created,time_updated,data)
                         VALUES ('orphan','no-session',{at},{at},'{{\"role\":\"assistant\"}}');",
                        at = (now + 25) * 1000,
                    ),
                );
            }
            // The first row is in a different session from the next message due.
            // Neither session has been migrated: this makes 2.x totals unknown only once it is due.
            exec(
                &db.path,
                &format!(
                    "INSERT INTO session(id,project_id,slug,directory,title,version,time_created,time_updated)
                     VALUES ('old-a','global','old-a','/work','Synthetic','1.19.6',{created},{created}),
                            ('old-b','global','old-b','/work','Synthetic','1.19.6',{created},{created});
                     INSERT INTO message(id,session_id,time_created,time_updated,data)
                     VALUES ('old-a','old-a',{later},{later},'{{\"role\":\"assistant\",\"tokens\":{{\"input\":99}}}}'),
                            ('old-b','old-b',{first},{first},'{{\"role\":\"assistant\",\"tokens\":{{\"input\":99}}}}');",
                    created = (now - MINUTE) * 1000,
                    later = (now + 80) * 1000,
                    first = (now + 50) * 1000 + 494,
                ),
            );
            last_written(&db.path, now - 60);
            let mut looks = Looks::new(opencode::poll(Some(db.path.clone()), TEST_WAIT));
            assert_eq!(steps_today(looks.look(now)), Some(2));
            assert!(looks.look(now + 49).is_none());
            let report = looks.look(now + 50).flatten().unwrap();
            match release.2 {
                opencode::Store::V1 => assert!(matches!(report, opencode::Report::Today(today) if today.steps == 3)),
                opencode::Store::V2 { .. } => assert_eq!(report, opencode::Report::Unsupported),
            }
        }
    }
}

#[test]
fn totals_are_read_again_when_the_time_zone_moves_midnight_within_the_same_date() {
    // The zone belongs to the whole process, so the check runs in one of its own.
    const CHILD: &str = "MILO_TEST_USAGE_ZONE";
    const TEST: &str =
        "usage::tests::unchanged::totals_are_read_again_when_the_time_zone_moves_midnight_within_the_same_date";
    if !sqlite_installed() {
        return;
    }
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && printed.contains("1 passed"), "{printed}");
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
    assert_eq!(usage.opencode.today(usage.today.since(noon)).map(|shown| shown.steps), Some(2));

    unsafe extern "C" {
        /// Has the C library read the zone from the environment again.
        fn tzset();
    }
    // Nine hours east it is the same date, and the day began nine hours earlier.
    // SAFETY: This process runs this one test, and no other thread reads the environment here.
    unsafe { std::env::set_var("TZ", "Asia/Tokyo") };
    // SAFETY: tzset takes no arguments and only reads the variable just set.
    unsafe { tzset() };
    assert_eq!(when::local_day(noon), when::local_day(noon - 12 * HOUR), "the date did not change");
    assert!(look(&mut usage, 2), "totals for the old zone's day were kept");
    let shown = usage.opencode.today(usage.today.since(noon)).map(|shown| shown.steps);
    assert_eq!(shown, Some(3), "the pane shows the new day's steps, one from before the old midnight");
}
