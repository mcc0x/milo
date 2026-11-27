//! Checks OpenCode totals against the tables its checked releases create.
//!
//! The schemas under `opencode/` come from databases the release binaries
//! made. Rows are synthetic, shaped like the ones those binaries wrote.

use super::super::opencode::{self, Activity, Cost, Report, Store, Tokens};
use super::*;

pub(super) type Release = (&'static str, &'static str, Store);

/// The first and newest checked release of each generation of session tables,
/// then the newest after it upgraded a 1.x database in place.
pub(super) const RELEASES: [Release; 5] = [
    ("1.2.0", include_str!("opencode/v1.2.0.sql"), Store::V1),
    ("1.19.6", include_str!("opencode/v1.19.6.sql"), Store::V1),
    ("2.2.0", include_str!("opencode/v2.2.0.sql"), Store::V2 { legacy: false }),
    ("2.3.2", include_str!("opencode/v2.3.2.sql"), Store::V2 { legacy: false }),
    ("2.3.2", include_str!("opencode/v2.3.2-from-1.19.6.sql"), Store::V2 { legacy: true }),
];
const NEWEST: [Release; 3] = [RELEASES[1], RELEASES[3], RELEASES[4]];
const UPGRADED: Release = RELEASES[4];
const REPLY: &str = "SYNTHETIC-REPLY";

/// One recorded step: input, output, reasoning, cache reads and cache writes, then its cost.
struct Step {
    tokens: [u64; 5],
    cost: Option<&'static str>,
}

const FIRST: Step = Step { tokens: [1000, 30, 4, 200, 0], cost: Some("0.00357") };
const SECOND: Step = Step { tokens: [50, 20, 0, 9_000_007, 400], cost: Some("0.4143") };

/// Which binary wrote a row of an upgraded database. Other databases have one writer.
#[derive(Clone, Copy, PartialEq)]
enum Writer {
    /// 1.x before the upgrade: the row is in both generations of tables.
    Migrated,
    /// 2.x: the row is only in the new tables.
    New,
    /// 1.x after the upgrade: the row is only in the old tables.
    Old,
}

pub(super) struct Database {
    pub path: PathBuf,
    store: Store,
    writer: Writer,
    next: u32,
}

impl Database {
    pub(super) fn new(f: &Fixture, schema: &str, store: Store) -> Self {
        let path = f.root.join("opencode.db");
        // sqlite3 would take the schema's leading comment for an option, so a statement goes first.
        let project =
            "INSERT INTO project (id,worktree,time_created,time_updated,sandboxes) VALUES ('global','/',0,0,'[]');";
        exec(&path, &format!("BEGIN;\n{schema}\n{project}\nCOMMIT;"));
        Self { path, store, writer: Writer::Migrated, next: 0 }
    }

    fn in_old_tables(&self) -> bool {
        match self.store {
            Store::V1 => true,
            Store::V2 { legacy } => legacy && self.writer != Writer::New,
        }
    }

    fn in_new_tables(&self) -> bool {
        self.store != Store::V1 && self.writer != Writer::Old
    }

    pub(super) fn session(&self, id: &str, version: &str, created: i64, fork_of: Option<&str>) {
        let created = created * 1000;
        let mut sql = String::new();
        if self.in_old_tables() {
            sql = format!(
                "INSERT INTO session (id,project_id,slug,directory,title,version,time_created,time_updated)
                 VALUES ('{id}','global','{id}','/work','Synthetic','{version}',{created},{created});"
            );
        }
        if self.in_new_tables() {
            sql += &format!(
                "INSERT INTO session_v2 (id,project_id,fork_session_id,slug,directory,version,time_created,time_updated)
                 VALUES ('{id}','global',{},'{id}','/work','{version}',{created},{created});",
                fork_of.map_or("NULL".into(), |source| format!("'{source}'"))
            );
        }
        self.exec(&sql);
    }

    fn step(&mut self, session: &str, at: i64, step: &Step) {
        let [input, output, reasoning, read, write] = step.tokens;
        let tokens = format!(
            r#""tokens":{{"input":{input},"output":{output},"reasoning":{reasoning},"cache":{{"read":{read},"write":{write}}}}}"#
        );
        let cost = step.cost.map_or(String::new(), |cost| format!(r#""cost":{cost},"#));
        self.assistant(session, at, &cost, &tokens);
    }

    fn assistant(&mut self, session: &str, at: i64, cost: &str, tokens: &str) {
        let ms = at * 1000 + 494;
        let old = format!(
            r#"{{"parentID":"msg_user","role":"assistant","mode":"build","agent":"build","path":{{"cwd":"/work","root":"/"}},{cost}{tokens},"modelID":"fake-1","providerID":"fake","time":{{"created":{ms},"completed":{ms}}},"finish":"stop"}}"#
        );
        let new = format!(
            r#"{{"time":{{"created":{ms},"completed":{ms}}},"agent":"build","model":{{"id":"fake-1","providerID":"fake"}},"content":[{{"type":"text","text":"{REPLY}"}}],"finish":"stop",{cost}{tokens}}}"#
        );
        self.row(session, ms, "assistant", &old, &new);
    }

    /// Rows that carry no usage: a prompt in either generation, and the idle marker 2.x adds.
    fn prompt(&mut self, session: &str, at: i64) {
        let ms = at * 1000;
        let old = format!(
            r#"{{"role":"user","time":{{"created":{ms}}},"agent":"build","model":{{"providerID":"fake","modelID":"fake-1"}}}}"#
        );
        let new = format!(r#"{{"time":{{"created":{ms}}},"text":"say hi","files":[]}}"#);
        self.row(session, ms, "user", &old, &new);
        let idle = format!(r#"{{"time":{{"created":{ms}}},"outcome":"succeeded"}}"#);
        self.row(session, ms, "idle", "", &idle);
    }

    fn row(&mut self, session: &str, ms: i64, kind: &str, old: &str, new: &str) {
        self.next += 1;
        let n = self.next;
        let mut sql = String::new();
        if self.in_old_tables() && !old.is_empty() {
            sql = format!(
                "INSERT INTO message (id,session_id,time_created,time_updated,data)
                 VALUES ('msg_{n}','{session}',{ms},{ms},'{old}');"
            );
        }
        if self.in_new_tables() {
            sql += &format!(
                "INSERT INTO session_message (id,session_id,type,seq,time_created,time_updated,data)
                 VALUES ('msg_{n}','{session}','{kind}',{n},{ms},{ms},'{new}');"
            );
        }
        self.exec(&sql);
    }

    /// A row for a generation this database lacks is nothing to run.
    fn exec(&self, sql: &str) {
        if !sql.is_empty() {
            exec(&self.path, sql);
        }
    }

    fn read(&self, now: i64) -> Report {
        opencode::read(&self.path, now, TEST_WAIT)
    }
}

fn midnight(now: i64) -> i64 {
    Today::at(now, when::local_day(now)).since
}

/// Two steps today in one session, after a step yesterday and before one dated
/// ahead of the clock. In an upgraded database the upgrade came between the two.
pub(super) fn busy_day(f: &Fixture, release: Release, now: i64) -> Database {
    let (version, schema, store) = release;
    let mut db = Database::new(f, schema, store);
    let since = midnight(now);
    db.session("ses_a", version, since - HOUR, None);
    db.step("ses_a", since - HOUR, &Step { tokens: [70_000, 7, 7, 7, 7], cost: Some("7") });
    db.prompt("ses_a", since + HOUR);
    db.step("ses_a", since + HOUR, &FIRST);
    db.writer = Writer::New;
    db.step("ses_a", now - MINUTE, &SECOND);
    db.step("ses_a", now + HOUR, &Step { tokens: [90_000, 9, 9, 9, 9], cost: Some("9") });
    db
}

fn busy_totals(store: Store, now: i64) -> Activity {
    Activity {
        since: midnight(now),
        store,
        steps: 2,
        tokens: Tokens { input: 1050, output: 50, reasoning: 4, cache_read: 9_000_207, cache_write: 400 },
        cost: Some(Cost { usd: 0.417_87, complete: true }),
        last: Some(now - MINUTE),
    }
}

fn today(report: Report) -> Activity {
    match report {
        Report::Today(activity) => activity,
        other => panic!("expected totals, found {other:?}"),
    }
}

#[test]
fn every_checked_release_yields_todays_steps_once_with_each_token_kind_apart() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    for release in RELEASES {
        let f = Fixture::folder();
        let db = busy_day(&f, release, now);
        let found = today(db.read(now));
        let expected = busy_totals(release.2, now);
        let cost = found.cost.unwrap();
        assert!((cost.usd - 0.417_87).abs() < 1e-9 && cost.complete, "{}: {cost:?}", release.0);
        assert_eq!(Activity { cost: expected.cost, ..found }, expected, "OpenCode {}", release.0);
        assert_eq!(found.tokens.fresh(), 1504, "cache reads are not new work");
    }
}

#[test]
fn history_copied_into_a_fork_is_counted_once() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    for release in NEWEST {
        let f = Fixture::folder();
        let mut db = busy_day(&f, release, now);
        // A fork made a minute ago repeats the source's steps under their original times.
        db.session("ses_fork", release.0, now - MINUTE + 1, Some("ses_a"));
        db.step("ses_fork", midnight(now) + HOUR, &FIRST);
        db.step("ses_fork", now - MINUTE, &SECOND);
        db.step("ses_fork", now - 30, &FIRST);
        let found = today(db.read(now));
        assert_eq!((found.steps, found.tokens.input), (3, 2050), "OpenCode {}", release.0);
        assert_eq!(found.last, Some(now - 30));
    }

    // An upgrade does not record that a 1.x session was a fork. Its copies still count once.
    let f = Fixture::folder();
    let mut db = busy_day(&f, UPGRADED, now);
    db.writer = Writer::Migrated;
    db.session("ses_fork", "1.19.6", now - MINUTE + 1, None);
    db.step("ses_fork", midnight(now) + HOUR, &FIRST);
    db.step("ses_fork", now - 30, &FIRST);
    assert_eq!(today(db.read(now)).steps, 3);
}

#[test]
fn a_cost_no_step_recorded_stays_unknown_and_a_partial_one_is_marked() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let unpriced = Step { tokens: [10, 1, 0, 0, 0], cost: None };
    for release in NEWEST {
        let f = Fixture::folder();
        let mut db = Database::new(&f, release.1, release.2);
        db.session("ses_a", release.0, now - HOUR, None);
        db.step("ses_a", now - 50, &unpriced);
        let found = today(db.read(now));
        assert_eq!((found.steps, found.cost), (1, None), "OpenCode {}", release.0);

        db.step("ses_a", now - 40, &Step { tokens: [10, 1, 0, 0, 0], cost: Some("0") });
        let zero = today(db.read(now)).cost.unwrap();
        assert_eq!((zero.usd, zero.complete, zero.text()), (0.0, false, None), "an unpriced step is not free");

        db.step("ses_a", now - 30, &FIRST);
        assert_eq!(today(db.read(now)).cost.unwrap().text().as_deref(), Some("<$0.01+"));
    }
}

/// Something a real database could hold that the checked releases never wrote.
type Spoil<'a> = (&'a str, &'a dyn Fn(&mut Database));

#[test]
fn unchecked_writers_tables_and_rows_stay_unknown() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let f = Fixture::folder();
    let path = f.root.join("opencode.db");
    assert_eq!(opencode::read(&path, now, TEST_WAIT), Report::Missing);
    // An empty or foreign file under the name is not a database OpenCode saved.
    for leftover in ["", "not a database"] {
        f.write("opencode.db", leftover);
        assert_eq!(opencode::read(&path, now, TEST_WAIT), Report::Missing, "{leftover:?}");
    }
    // A database is OpenCode's even when it cannot be read: here its journal files are gone.
    fs::remove_file(&path).unwrap();
    exec(&path, "CREATE TABLE session(id);");
    exec(&path, "PRAGMA journal_mode=WAL;");
    assert!(!f.root.join("opencode.db-wal").exists());
    assert_eq!(opencode::read(&path, now, TEST_WAIT), Report::Unreadable);
    fs::remove_file(&path).unwrap();

    let spoil: [Spoil<'_>; 13] = [
        ("a newer major release", &|db| {
            db.session("ses_new", "3.0.0", now - HOUR, None);
            db.step("ses_new", now - 20, &FIRST);
        }),
        ("a preview build", &|db| {
            db.session("ses_dev", "0.0.0-dev-202611170722", now - HOUR, None);
            db.step("ses_dev", now - 20, &FIRST);
        }),
        ("a row that is not JSON", &|db| {
            let table = if db.store == Store::V1 { "message" } else { "session_message" };
            exec(&db.path, &format!("UPDATE {table} SET data='{{' WHERE time_created>={}", (now - MINUTE) * 1000));
        }),
        ("a JSON scalar instead of a message", &|db| {
            let table = if db.store == Store::V1 { "message" } else { "session_message" };
            exec(&db.path, &format!("UPDATE {table} SET data='42' WHERE time_created>={}", (now - MINUTE) * 1000));
        }),
        // Each of these would vanish into a sum that still looks plausible.
        ("a negative count", &|db| db.assistant("ses_a", now - 20, "", r#""tokens":{"input":-10}"#)),
        ("a fractional count", &|db| db.assistant("ses_a", now - 20, "", r#""tokens":{"cache":{"read":0.5}}"#)),
        ("a count in words", &|db| db.assistant("ses_a", now - 20, "", r#""tokens":{"output":"12abc"}"#)),
        ("counts that are one number", &|db| db.assistant("ses_a", now - 20, "", r#""tokens":5"#)),
        ("an object instead of a count", &|db| db.assistant("ses_a", now - 20, "", r#""tokens":{"output":{}}"#)),
        ("a scalar cache count", &|db| db.assistant("ses_a", now - 20, "", r#""tokens":{"cache":10}"#)),
        ("a negative cost", &|db| db.assistant("ses_a", now - 20, r#""cost":-0.25,"#, r#""tokens":{"input":1}"#)),
        ("a cost in words", &|db| db.assistant("ses_a", now - 20, r#""cost":"free","#, r#""tokens":{"input":1}"#)),
        ("a renamed column", &|db| {
            let (table, column) =
                if db.store == Store::V1 { ("message", "time_created") } else { ("session_v2", "fork_session_id") };
            exec(&db.path, &format!("ALTER TABLE {table} RENAME COLUMN {column} TO renamed;"));
        }),
    ];
    for release in NEWEST {
        for (what, change) in spoil {
            let f = Fixture::folder();
            let mut db = busy_day(&f, release, now);
            assert!(matches!(db.read(now), Report::Today(_)));
            change(&mut db);
            assert_eq!(db.read(now), Report::Unsupported, "OpenCode {} with {what}", release.0);
        }
    }

    let f = Fixture::folder();
    exec(&f.root.join("opencode.db"), "CREATE TABLE unrelated(id);");
    assert_eq!(opencode::read(&f.root.join("opencode.db"), now, TEST_WAIT), Report::Unsupported);
}

#[test]
fn a_1x_binary_still_writing_to_an_upgraded_database_makes_the_day_unknown() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let f = Fixture::folder();
    let mut db = busy_day(&f, UPGRADED, now);
    // Yesterday's leftovers in the old tables are no reason to doubt today.
    db.writer = Writer::Old;
    db.session("ses_then", "1.19.6", midnight(now) - HOUR, None);
    db.step("ses_then", midnight(now) - HOUR, &FIRST);
    assert_eq!(today(db.read(now)).steps, 2);

    db.session("ses_old", "1.19.6", now - HOUR, None);
    db.step("ses_old", now - 20, &FIRST);
    assert_eq!(db.read(now), Report::Unsupported);
}

#[test]
fn a_future_2x_step_does_not_yet_cover_legacy_activity_in_its_session() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let f = Fixture::folder();
    let mut db = busy_day(&f, UPGRADED, now);
    db.writer = Writer::Old;
    db.session("ses_pending", "1.19.6", now - HOUR, None);
    db.step("ses_pending", now - MINUTE, &FIRST);
    db.writer = Writer::New;
    db.session("ses_pending", "1.19.6", now - HOUR, None);
    let due = now + MINUTE;
    db.step("ses_pending", due, &SECOND);

    assert_eq!(db.read(now), Report::Unsupported);
    assert_eq!(db.read(due - 1), Report::Unsupported);
    assert_eq!(today(db.read(due)).steps, 3);
}

#[test]
fn the_queries_return_counters_and_never_message_text() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let f = Fixture::folder();
    let db = busy_day(&f, RELEASES[3], now);
    let stored = Command::new(sqlite::executable())
        .args(["-batch", "-init", "/dev/null"])
        .arg(&db.path)
        .arg("SELECT data FROM session_message")
        .output();
    assert!(String::from_utf8(stored.unwrap().stdout).unwrap().contains(REPLY), "the fixture holds reply text");
    let totals = Store::V2 { legacy: false }.totals(midnight(now), now);
    let bytes = sqlite::query(sqlite::executable(), &db.path, &totals, TEST_WAIT).unwrap();
    let output = String::from_utf8(bytes).unwrap();
    assert!(output.contains("\"steps\":2") && !output.contains(REPLY), "{output}");
}

#[test]
fn the_database_path_follows_opencodes_own_overrides() {
    let home = Path::new("/home/user");
    let path = |data: Option<&str>, named: Option<&str>| {
        opencode::database_in(Some(home), data.map(Path::new), named.map(Path::new))
    };
    assert_eq!(path(None, None).unwrap(), home.join(".local/share/opencode/opencode.db"));
    assert_eq!(path(Some("/data"), None).unwrap(), Path::new("/data/opencode/opencode.db"));
    assert_eq!(path(Some("/data"), Some("custom.db")).unwrap(), Path::new("/data/opencode/custom.db"));
    assert_eq!(path(Some("relative"), Some("/abs/custom.db")).unwrap(), Path::new("/abs/custom.db"));
    assert_eq!(path(Some("relative"), None), None, "OpenCode's working directory is unknown");
    assert_eq!(path(None, Some(":memory:")), None);
    assert_eq!(opencode::database_in(None, None, None), None);
}

/// Waits for background reads the way the pane does, through its external descriptor.
pub(super) fn settle(usage: &mut Usage) {
    while let Some(fd) = usage.external_fd() {
        let mut ready = crate::sys::pollfd { fd, events: crate::sys::POLLIN, revents: 0 };
        let timeout = i32::try_from(TEST_WAIT.as_millis()).unwrap();
        // SAFETY: The single pollfd is live, and usage still owns its descriptor.
        let result = unsafe { libc::poll(&raw mut ready, 1, timeout) };
        if result == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        assert_eq!(result, 1, "a background read did not wake the pane");
        usage.check_external();
    }
}

fn pane(f: &Fixture, db: &Database, now: i64) -> Usage {
    let mut usage = unopened(f, now);
    usage.opencode = OpenCode::new(Some(db.path.clone()), TEST_WAIT);
    usage.check();
    settle(&mut usage);
    usage
}

fn opencode_rows(usage: &mut Usage, width: usize, height: usize) -> Vec<String> {
    let rows = frame(usage, width, height);
    rows.into_iter().filter(|row| row.contains("OpenCode")).map(|row| row.trim().to_owned()).collect()
}

#[test]
fn the_pane_gives_opencode_a_row_of_its_own_when_there_is_room_for_it() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let f = Fixture::folder();
    let db = busy_day(&f, RELEASES[3], now);
    let mut usage = pane(&f, &db, now);

    let wide = frame(&mut usage, 100, 9);
    assert_eq!(wide[7].trim_end(), " OpenCode · 1.5k tokens today · $0.42 est.", "in line with the key hints");
    assert!(wide[8].starts_with(" r reload"), "{wide:?}");
    assert_eq!(opencode_rows(&mut usage, 100, 8), [""; 0], "the panels keep their rows on a shorter pane");
    // On a rail it has a row as soon as each tool above it has one, in the wording that fits.
    assert_eq!(opencode_rows(&mut usage, 40, 16), ["OpenCode · 1.5k tokens today"]);
    assert_eq!(opencode_rows(&mut usage, 40, 3), ["OpenCode · 1.5k tokens today"]);
    assert_eq!(opencode_rows(&mut usage, 40, 2), [""; 0]);
    assert_eq!(opencode_rows(&mut usage, 24, 16), ["OpenCode 1.5k tokens"]);
    assert_eq!(opencode_rows(&mut usage, 20, 16), ["OpenCode 1.5k tokens"]);
    assert_eq!(opencode_rows(&mut usage, 14, 16), ["OpenCode 1.5k"]);

    // Yesterday's totals are not today's: they go until the next read replaces them.
    usage.fixed_now = Some(now + DAY);
    assert_eq!(opencode_rows(&mut usage, 100, 9), [""; 0]);
}

#[test]
fn the_opencode_row_does_not_take_claudes_third_window_or_its_tokens_today() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let f = Fixture::folder();
    write(&f, CLAUDE, &assistant(now - 10, 1000, 200, 0), now);
    let db = busy_day(&f, RELEASES[3], now);
    let mut usage = pane(&f, &db, now);
    let window = |minutes| Window { minutes, percent: 27.0, resets: Some(now + HOUR), observed: now };
    let model_week = ModelWeek::new("fable", window(10_080));
    let windows = Limits { base: [Some(window(300)), Some(window(10_080))], model_week };
    usage.claude.omp = Some(Reading { at: Timestamp::seconds(now), windows });

    let rows = frame(&mut usage, 100, 9);
    let claude =
        |text: &str| rows[..7].iter().any(|row| row.split_once('│').is_some_and(|(_, right)| right.contains(text)));
    assert!(claude("Fable week") && claude("1.2k tokens today"), "{rows:?}");
    assert!(rows[7].contains("OpenCode · 1.5k tokens today"), "{rows:?}");
}

#[test]
fn the_pane_shows_nothing_for_opencode_until_a_checked_reading_has_tokens() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let f = Fixture::folder();
    let (version, schema, store) = RELEASES[3];
    let mut db = Database::new(&f, schema, store);
    db.session("ses_a", version, now - HOUR, None);
    db.prompt("ses_a", now - 50);
    let mut usage = pane(&f, &db, now);
    let quiet = Activity { since: midnight(now), store, steps: 0, tokens: Tokens::default(), cost: None, last: None };
    assert_eq!(usage.opencode.report, Report::Today(quiet));
    assert_eq!(opencode_rows(&mut usage, 100, 9), [""; 0], "no steps today is not zero tokens");

    db.session("ses_new", "3.0.0", now - HOUR, None);
    db.step("ses_new", now - 20, &FIRST);
    usage.key(Key::Char('r'));
    settle(&mut usage);
    assert_eq!(usage.opencode.report, Report::Unsupported);
    assert_eq!(opencode_rows(&mut usage, 100, 9), [""; 0]);
}

#[test]
fn doctor_keeps_activity_cost_and_limits_apart_and_says_which_are_unknown() {
    let now = local_noon();
    let path = Path::new("/data/opencode/opencode.db");
    let lines = Report::Today(busy_totals(Store::V2 { legacy: false }, now)).describe(Some(path), now);
    assert_eq!(
        lines,
        [
            "OpenCode activity: 1504 tokens and 9000207 cache reads in 2 steps today, newest 1 m ago \
             (OpenCode 2 session tables in \"/data/opencode/opencode.db\")",
            "OpenCode cost: $0.42 today, as OpenCode estimated it",
            "OpenCode limits: unknown; OpenCode saves none locally",
        ]
    );
    let unknown = Report::Unsupported.describe(Some(path), now);
    assert!(unknown[0].starts_with("OpenCode activity: unknown;") && unknown[1] == "OpenCode cost: unknown");
    assert_eq!(unknown[2], lines[2]);
    assert_eq!(Report::Missing.describe(Some(path), now), ["OpenCode activity: no local database"]);
}
