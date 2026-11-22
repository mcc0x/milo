//! Offline, release-mode usage timing fixtures. The clock advances without
//! sleeping; fixture setup and writes are outside the measured intervals.
//!
//! `cargo test --release --locked --offline --bin milo usage_profile -- --ignored --nocapture --test-threads=1`

use std::fmt::Write as _;

use super::idle::{dated, history, history_sql, steps_sql};
use super::opencode_activity::{Database, RELEASES};
use super::unchanged::Writer;
use super::*;

pub(super) fn ready(fd: RawFd) {
    let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    loop {
        // SAFETY: `poll` is writable and the caller's job owns the descriptor.
        match unsafe { libc::poll(&raw mut poll, 1, 30_000) } {
            1 => return,
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            result => panic!("job did not signal completion: {result}"),
        }
    }
}

pub(super) fn micros(times: &mut [Duration]) -> String {
    if times.is_empty() {
        return "-\t-".into();
    }
    times.sort_unstable();
    let n = times.len();
    format!("{}\t{}", times[(n - 1) / 2].as_micros(), times[(n * 95).div_ceil(100) - 1].as_micros())
}

const TRANSCRIPT_LOOKS: u32 = 32;
const SHARED: u32 = 8;
const OLD_ROLLOUTS: u32 = 128;

fn transcript_fixture(f: &Fixture, now: i64, sessions: u32, records: u32) -> (Vec<String>, Vec<String>, u64) {
    let chatter = format!(
        "{{\"timestamp\":\"{}Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"content\":[{{\"text\":\"{}\"}}]}}}}\n",
        iso(now - HOUR),
        "fictional response from a working session. ".repeat(12)
    );
    for n in 0..OLD_ROLLOUTS {
        write(f, &format!("codex/2026/10/01/rollout-old-{n}.jsonl"), &chatter, now - DAY);
    }
    let rollouts = (0..4)
        .map(|n| {
            let path = format!("codex/2026/11/19/rollout-bench-{n}.jsonl");
            let data = codex(now - HOUR, 25, now + DAY, "null") + &chatter.repeat(512);
            write(f, &path, &data, now - 60);
            path
        })
        .collect();

    let claude = (0..records).map(|i| assistant(now - HOUR + i64::from(i), 3, 5, 7)).collect::<Vec<_>>();
    let chunk = format!(r#", "content":[{{"type":"text","text":"{}"}}]"#, "fictional patch summary. ".repeat(8));
    let transcripts = (0..sessions)
        .map(|session| {
            let path = if session % 2 == 0 {
                format!("claude/project-{session}/session.jsonl")
            } else {
                format!("claude/project-{session}/session/subagents/agent-bench.jsonl")
            };
            let mut data = String::with_capacity(440 * claude.len());
            for (i, line) in claude.iter().enumerate() {
                let id = if i < SHARED as usize { format!("shared-{i}") } else { format!("msg-{session}-{i}") };
                let line = identified(line, &id);
                writeln!(data, "{}{chunk}}}}}", line.strip_suffix("}}\n").unwrap()).unwrap();
            }
            write(f, &path, &data, now - 1);
            path
        })
        .collect();
    (rollouts, transcripts, u64::from(sessions * (records - SHARED) + SHARED))
}

fn append_at(f: &Fixture, path: &str, line: &str, now: i64) -> u64 {
    let mut file = OpenOptions::new().append(true).open(f.root.join(path)).unwrap();
    file.write_all(line.as_bytes()).unwrap();
    file.set_modified(mtime(now)).unwrap();
    u64::try_from(line.len()).unwrap()
}

#[test]
#[ignore = "a benchmark; run it alone on a release build"]
fn usage_profile_transcripts() {
    println!("# two seconds between looks; cold setup excluded; activity sum timed separately from refresh");
    println!(
        "size\tmode\tactive_claude_files\tarchive_rollouts\tlooks\twatch_notifications\tclaude_ids\tappend_bytes\tread_bytes\trefresh_p50_us\trefresh_p95_us\tactivity_p50_us\tactivity_p95_us"
    );
    for (name, sessions, records) in [("typical", 4, 256), ("busy", 32, 2000)] {
        let (f, now, clock) = (Fixture::folder(), local_noon(), Instant::now());
        let (rollouts, transcripts, mut ids) = transcript_fixture(&f, now, sessions, records);
        let mut usage = unopened(&f, now);
        assert!(usage.refresh(now, clock, false));
        assert_eq!(usage.claude.activity(usage.today.since, now).today, ids * 15);
        for (phase, (mode, appending, notifying)) in
            [("idle", false, false), ("append-known", true, false), ("append-notified", true, true)]
                .into_iter()
                .enumerate()
        {
            let (mut read, mut written) = (0, 0);
            let (mut refresh, mut activity) = (Vec::new(), Vec::new());
            for look in 1..=TRANSCRIPT_LOOKS {
                let tick = u32::try_from(phase).unwrap() * TRANSCRIPT_LOOKS + look;
                let (at, clock) = (now + 2 * i64::from(tick), clock + Duration::from_secs(2) * tick);
                if appending {
                    let msg = identified(&assistant(at, 3, 5, 7), &format!("new-{tick}"));
                    written += append_at(&f, &transcripts[tick as usize % transcripts.len()], &msg, at);
                    let percent = 25 + look;
                    let report = codex(at, percent, now + DAY, "null");
                    written += append_at(&f, &rollouts[tick as usize % rollouts.len()], &report, at);
                    ids += 1;
                }
                if notifying {
                    usage.filesystem_changed();
                }
                let started = Instant::now();
                let settled = usage.refresh(at, clock, false);
                refresh.push(started.elapsed());
                read += usage.bytes_read;
                let started = Instant::now();
                let tokens = usage.claude.activity(usage.today.since, at);
                activity.push(started.elapsed());
                assert!(settled && !tokens.partial);
                assert_eq!(tokens.today, ids * 15);
                if appending {
                    let report = usage.codex.reported(at).unwrap().0;
                    assert_eq!(report.at.seconds, at);
                    assert_eq!(report.windows.base[0].map(|window| window.percent), Some(f64::from(25 + look)));
                }
            }
            if !appending {
                assert_eq!(read, 0);
            }
            let events = u32::from(notifying) * TRANSCRIPT_LOOKS;
            println!(
                "{name}\t{mode}\t{sessions}\t{OLD_ROLLOUTS}\t{TRANSCRIPT_LOOKS}\t{events}\t{ids}\t{written}\t{read}\t{}\t{}",
                micros(&mut refresh),
                micros(&mut activity)
            );
        }
    }
}

const DB_LOOKS: u32 = 12;
const HISTORY_ROWS: u32 = 200_000;
const OPEN_CODE_ROWS: u32 = 50_000;

fn opencode_rows(path: &Path, first: u32, rows: u32, newest: i64) {
    let ms = newest * 1000;
    let text = "fictional response with a tool result and a code change. ".repeat(5);
    exec(
        path,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i+1<{rows})
             INSERT INTO session_message(id,session_id,type,seq,time_created,time_updated,data)
             SELECT 'seed-'||({first}+i),'session-'||(i%8),CASE WHEN i%4=0 THEN 'user' ELSE 'assistant' END,
                    {first}+i,{ms}-i*500,{ms}-i*500,
                    '{{\"agent\":\"build\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}],\"cost\":0.001,
                      \"tokens\":{{\"input\":10,\"output\":5,\"reasoning\":0,\"cache\":{{\"read\":7,\"write\":1}}}}}}'
             FROM n;"
        ),
    );
}

fn database_looks(
    profile: &str,
    path: &Path,
    mut usage: Usage,
    now: i64,
    rows: (u32, u32),
    mut commit: impl FnMut(u32, i64),
    mut verify: impl FnMut(&Usage, u32),
) {
    let bytes = fs::metadata(path).unwrap().len();
    let (mut inline, mut fetched, mut jobs) = (Vec::new(), Vec::new(), 0);
    let clock = Instant::now();
    for look in 0..=DB_LOOKS {
        let seconds = Duration::from_secs(20) * look;
        let at = now + i64::try_from(seconds.as_secs()).unwrap();
        commit(look, at);
        let start = Instant::now();
        usage.refresh(at, clock + seconds, false);
        let foreground = start.elapsed();
        let fd = usage.external_fd().expect("the committed change must start a new read");
        ready(fd);
        usage.check_external();
        let complete = start.elapsed();
        assert!(usage.external_fd().is_none());
        verify(&usage, look);
        if look > 0 {
            jobs += 1;
            inline.push(foreground);
            fetched.push(complete);
        }
    }
    let slow = fetched.iter().filter(|duration| **duration > sqlite::QUERY_WAIT).count();
    let (seed_rows, day_rows) = rows;
    println!(
        "{profile}\t{bytes}\t{seed_rows}\t{day_rows}\t{DB_LOOKS}\t{jobs}\t{slow}\t{}\t{}",
        micros(&mut inline),
        micros(&mut fetched)
    );
}

#[test]
#[ignore = "a benchmark; run it alone on a release build"]
fn usage_profile_active_sqlite() {
    if !sqlite_installed() {
        return;
    }
    println!("# WAL writers stay open; one change per 20s look; byte counts are base files; one warm-up omitted");
    println!("# Job completion includes process startup. A longer fixture limit exposes reads over one second.");
    println!(
        "profile\tdatabase_bytes\tseed_rows\tday_assistant_rows\tcommits\tjobs\tover_1s\tinline_p50_us\tinline_p95_us\tresult_p50_us\tresult_p95_us"
    );
    let now = local_noon();
    for (profile, cached) in [("omp-uncached", false), ("omp-cached", true)] {
        let f = Fixture::folder();
        let path = omp_limits::sqlite(&f).unwrap();
        history(&path, HISTORY_ROWS, (now - 60) * 1000, "0.25");
        let mut writer = Writer::open(&f, &path);
        let mut usage = unopened(&f, now);
        usage.omp = omp::poll(Some(path.clone()), TEST_WAIT);
        database_looks(
            profile,
            &path,
            usage,
            now,
            (HISTORY_ROWS, 0),
            |look, at| {
                if cached {
                    let observed = (at - 4) * 1000;
                    let reset = (at + HOUR) * 1000;
                    let base = omp_limits::window("anthropic:5h", "0.1", reset, 300, "null");
                    writer.run(&format!(
                        "INSERT OR REPLACE INTO cache VALUES ('usage_cache:report:anthropic:default:oauth|account:bench',
                         '{{\"value\":{{\"provider\":\"anthropic\",\"fetchedAt\":{observed},\"limits\":[{base}]}}}}',0);"
                    ));
                }
                let fraction = if look % 2 == 0 { "0.25" } else { "0.75" };
                writer.run(&history_sql(1, (at - 1) * 1000, fraction));
                dated(&path, at - 1);
            },
            |usage, look| {
                let reading = usage.claude.omp.unwrap().windows.base[0].unwrap();
                assert_eq!(reading.percent, if look % 2 == 0 { 25.0 } else { 75.0 });
            },
        );
        writer.close();
    }
    for (profile, day_rows) in [("opencode-mostly-history", 2_000), ("opencode-dense-day", 40_000)] {
        let f = Fixture::folder();
        let (version, schema, store) = RELEASES[3];
        let db = Database::new(&f, schema, store);
        for i in 0..8 {
            db.session(&format!("session-{i}"), version, now - 2 * DAY, None);
        }
        db.session("bench", version, now - HOUR, None);
        opencode_rows(&db.path, 0, OPEN_CODE_ROWS - day_rows, now - DAY);
        opencode_rows(&db.path, OPEN_CODE_ROWS - day_rows, day_rows, now - 60);
        let mut writer = Writer::open(&f, &db.path);
        let mut usage = unopened(&f, now);
        usage.opencode = OpenCode::new(Some(db.path.clone()), TEST_WAIT);
        let assistant_rows = day_rows * 3 / 4;
        database_looks(
            profile,
            &db.path,
            usage,
            now,
            (OPEN_CODE_ROWS, assistant_rows),
            |look, at| {
                writer.run(&steps_sql(look, 1, (at - 1) * 1000));
                dated(&db.path, at - 1);
            },
            |usage, look| {
                let opencode::Report::Today(totals) = usage.opencode.report else {
                    panic!("expected OpenCode totals, got {:?}", usage.opencode.report);
                };
                assert_eq!(totals.steps, u64::from(assistant_rows + look + 1));
            },
        );
        writer.close();
    }
}
