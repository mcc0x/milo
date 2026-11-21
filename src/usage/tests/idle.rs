//! Counts what an idle Usage pane asks of the local databases, and how soon
//! it shows a figure a tool has just saved.
//!
//! The pane is given OMP's and OpenCode's databases, made up here at two
//! sizes, and its timer is run twenty times, one read interval apart, with
//! nothing written in between. Then each database gets a new row and the
//! timer runs until the pane shows it.
//!
//! ```sh
//! cargo test --release --locked --bin milo usage::tests::idle -- --ignored --nocapture
//! ```

use super::opencode_activity::{Database, RELEASES};
use super::*;

const TICKS: u32 = 20;
/// One background read interval, as `sqlite.rs` keeps it.
const TICK: Duration = Duration::from_secs(20);
/// Each OpenCode read asks SQLite twice: for the tables, then the totals.
const OPENCODE_QUERIES: u32 = 2;

/// How many rows each database starts with.
struct Size {
    name: &'static str,
    history: u32,
    steps: u32,
}

const SIZES: [Size; 2] =
    [Size { name: "small", history: 4, steps: 4 }, Size { name: "large", history: 200_000, steps: 50_000 }];

/// Dates a file as a tool would have by writing it at `at`. The fixtures
/// are made as the test runs, long after the time the pane is told it is.
pub(super) fn last_written(path: &Path, at: i64) {
    File::open(path).unwrap().set_modified(mtime(at)).unwrap();
}

pub(super) fn history(path: &Path, rows: u32, newest_ms: i64, fraction: &str) {
    let reset = (NOW + DAY) * 1000;
    exec(
        path,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i+1<{rows})
             INSERT INTO usage_history(provider, account_key, recorded_at, limit_id, used_fraction, resets_at, window_label)
             SELECT 'anthropic','oauth|account:bench',{newest_ms}-i*1000,'anthropic:5h',{fraction},{reset},'5 Hours' FROM n;"
        ),
    );
}

pub(super) fn steps(path: &Path, first: u32, rows: u32, newest_ms: i64) {
    exec(
        path,
        &format!(
            r#"WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i+1<{rows})
               INSERT INTO session_message (id,session_id,type,seq,time_created,time_updated,data)
               SELECT 'msg_'||({first}+i),'bench','assistant',{first}+i,{newest_ms}-i,{newest_ms}-i,
                      '{{"cost":0.001,"tokens":{{"input":10,"output":5,"reasoning":0,"cache":{{"read":7,"write":1}}}}}}'
               FROM n;"#
        ),
    );
}

/// Lets every read the timer started finish, and returns how long that took.
fn finish(usage: &mut Usage) -> Duration {
    let began = Instant::now();
    while usage.external_fd().is_some() {
        usage.check_external();
        std::thread::sleep(Duration::from_micros(200));
    }
    began.elapsed()
}

/// Runs the timer once and returns how many `sqlite3` processes it started
/// and how long they took.
fn tick(usage: &mut Usage, tick: u32, clock: Instant) -> (u32, Duration) {
    usage.refresh(NOW + i64::from(tick) * 20, clock + TICK * tick, false);
    let started =
        u32::from(usage.omp.fd().is_some()) + OPENCODE_QUERIES * u32::from(usage.opencode.poll.fd().is_some());
    (started, finish(usage))
}

fn shown(usage: &Usage) -> (Option<f64>, Option<u64>) {
    let percent = usage.claude.omp.and_then(|reading| reading.windows.base[0]).map(|window| window.percent);
    let steps = match usage.opencode.report {
        opencode::Report::Today(activity) => Some(activity.steps),
        _ => None,
    };
    (percent, steps)
}

#[test]
#[ignore = "a benchmark; run it alone on a release build"]
fn idle_ticks_and_freshness() {
    if !sqlite_installed() {
        return;
    }
    println!("# milo usage idle bench: {TICKS} timer ticks {TICK:?} apart with nothing written");
    println!("size\tdatabase_bytes\tsqlite3_started\tread_ms\tticks_to_fresh");
    for size in SIZES {
        let f = Fixture::folder();
        let omp = omp_limits::sqlite(&f).unwrap();
        history(&omp, size.history, (NOW - 60) * 1000, "0.5");
        let (_, schema, store) = RELEASES[3];
        let db = Database::new(&f, schema, store);
        db.session("bench", "2.3.2", NOW - 3600, None);
        steps(&db.path, 0, size.steps, (NOW - 60) * 1000);
        let bytes = fs::metadata(&omp).unwrap().len() + fs::metadata(&db.path).unwrap().len();
        last_written(&omp, NOW - 60);
        last_written(&db.path, NOW - 60);

        let mut usage = unopened(&f, NOW);
        usage.omp = omp::poll(Some(omp.clone()), TEST_WAIT);
        usage.opencode = OpenCode::new(Some(db.path.clone()), TEST_WAIT);
        let clock = Instant::now();
        // The first read is the pane opening, not an idle tick.
        tick(&mut usage, 0, clock);
        assert_eq!(shown(&usage), (Some(50.0), Some(u64::from(size.steps))));
        let (mut started, mut reading) = (0, Duration::ZERO);
        for at in 1..=TICKS {
            let (processes, took) = tick(&mut usage, at, clock);
            started += processes;
            reading += took;
        }

        let written = NOW + i64::from(TICKS) * 20;
        history(&omp, 1, written * 1000, "0.77");
        steps(&db.path, size.steps, 1, written * 1000);
        last_written(&omp, written);
        last_written(&db.path, written);
        let fresh = (1..=TICKS).find(|after| {
            tick(&mut usage, TICKS + after, clock);
            shown(&usage) == (Some(77.0), Some(u64::from(size.steps) + 1))
        });
        let fresh = fresh.map_or("never".into(), |ticks| ticks.to_string());
        println!("{}\t{bytes}\t{started}\t{}\t{fresh}", size.name, reading.as_millis());
    }
}
