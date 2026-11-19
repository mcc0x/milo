//! Checks usage frames and provider ingestion with fixtures instead of live data.

mod activity;
mod codex;
mod layout;
mod status_line;

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::os::unix::net::UnixStream;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fixture::Fixture;
use crate::fs_ops;
use crate::pane::{Pace, WATCHED};
use crate::when::HOUR;
use crate::width::str_width;

use super::*;

const NOW: i64 = 1_795_100_000;
const TEST_WAIT: Duration = Duration::from_secs(30);
const CODEX: &str = "codex/2026/11/18/rollout-a.jsonl";
const OTHER_CODEX: &str = "codex/2026/11/18/rollout-z.jsonl";
const CLAUDE: &str = "claude/project/session.jsonl";
const AGENT: &str = "claude/project/session/subagents/agent-good.jsonl";
const STATE: &str = "state";
const STATUS: &[u8] = br#"{
    "rate_limits": {
        "five_hour": {"used_percentage":23,"resets_at":1795110000},
        "seven_day": {"used_percentage":61,"resets_at":"2026-11-21T02:00:00+02:00"}
    },
    "model":{"display_name":"Claude Test"}
}"#;

fn iso(seconds: i64) -> String {
    let ts = libc::time_t::try_from(i128::from(seconds)).unwrap();
    // SAFETY: Zero is valid for the integer fields and the raw timezone pointer.
    let mut time: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: ts is readable and time is writable for gmtime_r.
    assert!(!unsafe { libc::gmtime_r(&ts, &mut time) }.is_null());
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        1900 + time.tm_year,
        1 + time.tm_mon,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    )
}

fn mtime(seconds: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).unwrap())
}

fn write(f: &Fixture, path: &str, data: &str, now: i64) {
    f.write(path, data);
    File::open(f.root.join(path)).unwrap().set_modified(mtime(now)).unwrap();
}

fn open(f: &Fixture, now: i64) -> Usage {
    let mut usage = unopened(f, now);
    usage.check();
    usage
}

fn unopened(f: &Fixture, now: i64) -> Usage {
    Usage::from_dirs(Some(f.root.join("codex")), Some(f.root.join("claude")), Some(f.root.join(STATE)), now)
}

fn frame(usage: &mut Usage, width: usize, height: usize) -> Vec<String> {
    let mut screen = Screen::sized(width, height);
    usage.draw(&mut screen);
    let rows = screen.text();
    assert_eq!(rows.len(), height);
    assert!(rows.iter().all(|row| str_width(row) <= width));
    rows
}

fn local_noon() -> i64 {
    Today::at(NOW, when::local_day(NOW)).since + 12 * HOUR
}

fn caption(usage: &mut Usage) -> String {
    frame(usage, 140, 4).pop().unwrap()
}

fn codex_snapshot(timestamp: &str, primary: &str, secondary: &str) -> String {
    format!(
        "{{\"timestamp\":\"{timestamp}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
        \"rate_limits\":{{\"primary\":{primary},\"secondary\":{secondary}}}}}}}\n"
    )
}

fn codex_window(percent: u32, minutes: u32, reset: i64) -> String {
    format!(r#"{{"used_percent":{percent},"window_minutes":{minutes},"resets_at":{reset}}}"#)
}

fn codex_at(timestamp: &str, percent: u32, reset: i64, secondary: &str) -> String {
    codex_snapshot(timestamp, &codex_window(percent, 10_080, reset), secondary)
}

fn codex(at: i64, percent: u32, reset: i64, secondary: &str) -> String {
    codex_at(&format!("{}Z", iso(at)), percent, reset, secondary)
}

fn assistant(at: i64, input: u64, output: u64, writes: u64) -> String {
    let timestamp = iso(at);
    format!(
        "{{\"timestamp\":\"{timestamp}Z\",\"type\":\"assistant\",\"message\":{{\"usage\":{{\
        \"input_tokens\":{input},\"output_tokens\":{output},\"cache_creation_input_tokens\":{writes},\
        \"cache_read_input_tokens\":9010011}},\"model\":\"Claude Test\"}}}}\n"
    )
}

fn identified(line: &str, id: &str) -> String {
    line.replacen("\"usage\":", &format!("\"id\":\"{id}\",\"usage\":"), 1)
}

fn activity(usage: &Usage) -> Activity {
    usage.claude.activity(usage.today.since, usage.now())
}

fn with_limits() -> Fixture {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 23, NOW + 3 * DAY, "null"), NOW);
    let path = f.root.join(STATE).join(SAVED_LIMITS);
    assert_eq!(status::run(STATUS, Vec::new(), Some(&path), NOW), 0);
    f
}

#[test]
fn fractional_reset_times_round_up_to_the_next_second() {
    let reset = NOW + HOUR;
    let values = [format!("{}", reset * 1000 + 1), format!("\"{}.000000001Z\"", iso(reset))];
    for input in &values {
        let decoded = time::reset(Value::parse(input.as_bytes()).unwrap(), NOW);
        assert_eq!(decoded, Some(reset + 1));
        let window = Window { minutes: 300, percent: 42.0, resets: decoded, observed: NOW };
        assert!(!window.is_reset(reset));
        assert!(window.is_reset(reset + 1));
    }
}

#[test]
fn compressed_and_unrelated_files_are_ignored() {
    let f = Fixture::folder();
    let data = codex(NOW, 23, NOW + DAY, "null");
    write(&f, &format!("{CODEX}.zst"), &data, NOW);
    write(&f, "codex/2026/11/18/not-a-rollout.jsonl", &data, NOW);
    write(&f, "codex/rollout-b.jsonl", &data, NOW);
    write(&f, "claude/project/subagents/session.jsonl", &assistant(NOW, 600, 900, 100), NOW);
    write(&f, CLAUDE, &assistant(NOW, 600, 900, 100), NOW - 2 * DAY);
    let rows = frame(&mut open(&f, NOW), 140, 4);
    assert!(rows[2].contains("no sessions found"));
    assert!(!rows.join("\n").contains("tokens today"));
}

#[test]
fn session_files_and_folders_are_not_followed_through_links() {
    let f = Fixture::folder();
    write(&f, "elsewhere/rollout-linked.jsonl", &codex(NOW, 98, NOW + DAY, "null"), NOW);
    write(&f, "elsewhere/claude.jsonl", &assistant(NOW, 3000, 400, 100), NOW);
    fs::create_dir_all(f.root.join("codex/2026/11")).unwrap();
    symlink(f.root.join("elsewhere"), f.root.join("codex/2026/11/18")).unwrap();
    fs::create_dir_all(f.root.join("codex/2026/11/19")).unwrap();
    symlink(f.root.join("elsewhere/rollout-linked.jsonl"), f.root.join("codex/2026/11/19/rollout-linked.jsonl"))
        .unwrap();
    fs::create_dir_all(f.root.join("claude/project")).unwrap();
    symlink(f.root.join("elsewhere"), f.root.join("claude/linked")).unwrap();
    symlink(f.root.join("elsewhere/claude.jsonl"), f.root.join(CLAUDE)).unwrap();
    let mut usage = open(&f, NOW);
    let rows = frame(&mut usage, 140, 4);
    assert!(rows[2].contains("no sessions found"));
    assert!(!rows.join("\n").contains("tokens today"));
    assert_eq!(usage.bytes_read, 0);
}

#[test]
fn the_first_usage_look_loads_data_and_missing_roots_use_the_two_second_pace() {
    let f = Fixture::folder();
    let now = local_noon();
    fs::create_dir_all(f.root.join("codex")).unwrap();
    fs::create_dir_all(f.root.join(STATE)).unwrap();
    write(&f, CLAUDE, &assistant(now, 2000, 0, 0), now);
    let mut usage = unopened(&f, now);
    let mut pace = Pace::for_pane(&mut usage, WATCHED, true);
    assert_eq!(activity(&usage).today, 2000);
    assert_eq!(pace.idle(), WATCHED);

    fs::remove_dir(f.root.join("codex")).unwrap();
    pace.look(&mut usage, true);
    assert_eq!(pace.idle(), Duration::from_secs(2));
    write(&f, CODEX, &codex(now, 61, now + HOUR, "null"), now);
    pace.look(&mut usage, true);
    assert!(frame(&mut usage, 100, 2)[0].contains("39%"));
    // A new root is not guaranteed to be covered by the original watcher.
    assert_eq!(pace.idle(), Duration::from_secs(2));
}

fn claude_report(path: &Path, limits: &str, now: i64) {
    let input = format!("{{\"rate_limits\":{limits}}}");
    assert_eq!(status::run(input.as_bytes(), Vec::new(), Some(path), now), 0);
}

#[test]
fn malformed_enormous_and_deeply_nested_lines_are_skipped() {
    let f = Fixture::folder();
    let padding = format!("{{\"ignored\":\"{}\",", "a".repeat(2 * 1024 * 1024));
    let long_codex = codex(NOW, 97, NOW + DAY, "null").replacen('{', &padding, 1);
    let long_claude = assistant(NOW, 31_000, 0, 0).replacen('{', &padding, 1);
    let deep = format!("{{\"usage\":\"token_count\",\"ignored\":{}0{}}}\n", "[".repeat(1000), "]".repeat(1000));
    let garbage = "{\"usage\":\"token_count\",\"broken\"}\n";
    let codex_data = [codex(NOW, 23, NOW + DAY, "null"), long_codex, deep.clone(), garbage.into()].concat();
    let claude_data = [long_claude, deep, garbage.into(), assistant(NOW, 200, 900, 100)].concat();
    write(&f, CODEX, &codex_data, NOW);
    write(&f, CLAUDE, &claude_data, NOW);
    let rows = frame(&mut open(&f, NOW), 140, 4);
    assert!(rows[1].contains("77%") && rows[3].contains("1.2k tokens today"));
}

mod omp_limits {
    use std::process::{Command, Stdio};

    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE cache(key TEXT PRIMARY KEY, value TEXT, expires_at INTEGER);
        CREATE TABLE usage_history(id INTEGER PRIMARY KEY, recorded_at INTEGER, provider TEXT,
            account_key TEXT, email TEXT, account_id TEXT, limit_id TEXT, label TEXT, window_label TEXT,
            used_fraction REAL, status TEXT, resets_at INTEGER);
    ";

    fn sqlite(f: &Fixture) -> Option<PathBuf> {
        if Command::new(omp::sqlite()).arg("-version").output().is_err() {
            eprintln!("skipping OMP fixture: sqlite3 is not installed");
            return None;
        }
        let path = f.root.join("agent.db");
        exec(&path, SCHEMA);
        Some(path)
    }

    fn exec(path: &Path, sql: &str) {
        let result =
            Command::new(omp::sqlite()).args(["-batch", "-init", "/dev/null"]).arg(path).arg(sql).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    }

    fn report(path: &Path, key: &str, provider: &str, ms: i64, limits: &str) {
        exec(
            path,
            &format!(
                r#"INSERT OR REPLACE INTO cache VALUES ('usage_cache:report:{key}',
                   '{{"value":{{"provider":"{provider}","fetchedAt":{ms},"limits":{limits}}}}}',0);"#
            ),
        );
    }

    fn window(id: &str, fraction: &str, reset_ms: i64, minutes: u64, tier: &str) -> String {
        let duration_ms = minutes * 60_000;
        format!(
            r#"{{"id":"{id}","amount":{{"usedFraction":{fraction}}},"scope":{{"tier":{tier}}},
                 "window":{{"durationMs":{duration_ms},"resetsAt":{reset_ms}}}}}"#
        )
    }

    fn wait_for(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + TEST_WAIT * 2;
        while !ready() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn omp_prefers_the_newest_report_per_provider_and_newer_history_per_limit() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let now = NOW * 1000;
        let reset = (NOW + DAY) * 1000 + 789;
        let five = window("anthropic:5h", "0.9", reset, 300, "null");
        let week = window("anthropic:7d", "0.2", reset, 10_080, "null");
        let fable = window("anthropic:7d:fable", "0.53", reset, 10_080, r#""fable""#);
        let other = window("anthropic:7d:opus", "0.35", reset, 10_080, r#""opus""#);
        let null = window("anthropic:5h", "null", reset, 300, "null");
        let codex = window("openai-codex:primary", "0.01", reset, 10_080, "null");
        report(&path, "older-z", "anthropic", now - 3000, &format!("[{five}]"));
        report(&path, "key-only-secret", "anthropic", now - 1000, &format!("[{week},{fable},{other},{null},\"junk\"]"));
        report(&path, "codex", "openai-codex", now - 2000, &format!("[{codex}]"));
        exec(
            &path,
            &format!(
                "INSERT INTO cache VALUES('usage_cache:report:bad','not JSON',0);
                 INSERT INTO usage_history(provider, recorded_at, limit_id, used_fraction, resets_at, window_label)
                 VALUES ('anthropic',{now}-2000,'anthropic:5h',0.92,{reset},'5 Hours'),
                        ('anthropic',{now}-950,'anthropic:7d',0.29,{reset},'7 Day'),
                        ('anthropic',{now}-900,'anthropic:7d',0.27,{reset},'7 Day'),
                        ('openai-codex',{now}-2000,'openai-codex:primary',0.8,{reset},'7 days');"
            ),
        );

        let bytes = omp::query(omp::sqlite(), &path, TEST_WAIT).unwrap();
        assert!(!omp::SQL.to_ascii_lowercase().contains("auth_credentials"));
        assert!(omp::SQL.trim_start().starts_with("PRAGMA query_only=ON;"));
        assert!(!String::from_utf8_lossy(&bytes).contains("key-only-secret"));
        let reading = omp::parse(&bytes, NOW).unwrap();
        let claude = reading.claude.unwrap();
        assert!(claude.at == Timestamp::millis(Value::parse((now - 900).to_string().as_bytes()).unwrap()).unwrap());
        assert!(claude.windows.base[0].is_none());
        let week = claude.windows.base[1].unwrap();
        assert!(week.percent == 27.0 && week.resets == Some(NOW + DAY) && week.observed == NOW - 1);
        let model = claude.windows.model_week.unwrap();
        assert_eq!(model.label(), "Fable");
        assert_eq!(model.window.percent, 53.0);
        let codex = reading.codex.unwrap();
        assert!(codex.windows.base[0].is_none());
        assert_eq!(codex.windows.base[1].unwrap().percent, 1.0);
    }

    #[test]
    fn omp_history_fills_newer_windows_and_providers_without_a_report() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let now = NOW * 1000;
        report(&path, "recent-empty", "anthropic", now - 10, "null");
        exec(
            &path,
            &format!(
                "INSERT INTO usage_history(provider, recorded_at, limit_id, used_fraction, resets_at, window_label)
                 VALUES ('anthropic',{now}-11,'anthropic:7d',0.27,NULL,'7 Day'),
                        ('anthropic',{now},'anthropic:7d:fable',0.53,NULL,'7 Day'),
                        ('openai-codex',{now},'openai-codex:primary',0.01,NULL,'7 days');"
            ),
        );
        let reading = omp::read(&path, NOW, TEST_WAIT).unwrap();
        let claude = reading.claude.unwrap().windows;
        assert!(claude.base.into_iter().all(|window| window.is_none()));
        assert_eq!(claude.model_week.unwrap().label(), "Fable");
        assert_eq!(reading.codex.unwrap().windows.base[1].unwrap().percent, 1.0);
    }

    #[test]
    fn omp_errors_are_quiet_and_oversized_output_is_cut_off() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        assert!(omp::read(&f.root.join("missing.db"), NOW, TEST_WAIT).is_none());
        assert!(omp::query(f.root.join("missing-sqlite").as_os_str(), &path, TEST_WAIT).is_none());
        let executable = f.root.join("sqlite-stub");
        let script = |lines: &str| write_executable(&executable, lines);
        script("printf '%s' 'not JSON'");
        let bytes = omp::query(executable.as_os_str(), &path, TEST_WAIT).unwrap();
        assert!(omp::parse(&bytes, NOW).is_none());
        script("printf '%s\\n' \"$$\" >\"$0.pid\"; printf '%70000s' x");
        assert!(omp::query(executable.as_os_str(), &path, TEST_WAIT).is_none());
        assert_reaped(&executable);
        exec(&path, "DROP TABLE usage_history;");
        assert!(omp::read(&path, NOW, TEST_WAIT).is_none());
    }

    /// Writes a stand-in program and waits until it can be run. A child that
    /// another test thread forks at this instant holds the file open for
    /// writing until it execs, and a file open for writing cannot be run.
    fn write_executable(path: &Path, lines: &str) {
        use std::os::unix::fs::PermissionsExt as _;

        fs::write(path, format!("#!/bin/sh\n[ \"$1\" = ready ] && exit 0\n{lines}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        let deadline = Instant::now() + TEST_WAIT;
        while Command::new(path).arg("ready").status().is_err() {
            assert!(Instant::now() < deadline, "the stand-in never became runnable");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn assert_reaped(executable: &Path) {
        let pid: libc::pid_t = fs::read_to_string(executable.with_extension("pid")).unwrap().trim().parse().unwrap();
        // SAFETY: WNOHANG does not block and a null status pointer is allowed.
        assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
    }

    #[test]
    fn omp_does_not_create_missing_wal_sidecars() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        exec(&path, "PRAGMA journal_mode=WAL;");
        for sidecar in ["agent.db-wal", "agent.db-shm"] {
            assert!(!f.root.join(sidecar).exists());
        }
        assert!(omp::read(&path, NOW, TEST_WAIT).is_none());
        for sidecar in ["agent.db-wal", "agent.db-shm"] {
            assert!(!f.root.join(sidecar).exists());
        }
        // Use a child that would otherwise succeed even on a database without sidecars.
        assert!(omp::query(std::ffi::OsStr::new("/usr/bin/true"), &path, TEST_WAIT).is_none());
    }

    #[test]
    fn omp_reads_committed_wal_limits_during_a_write_without_recreating_a_lost_sidecar() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let db = f.root.join("agent #?% café.db");
        fs::rename(path, &db).unwrap();
        let week = window("anthropic:7d", "0.27", (NOW + DAY) * 1000, 10_080, "null");
        let mut writer = Command::new(omp::sqlite())
            .args(["-batch", "-bail", "-init", "/dev/null"])
            .current_dir(&f.root)
            .arg(&db)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let sql = format!(
            "PRAGMA journal_mode=WAL;\n\
             INSERT INTO cache VALUES ('usage_cache:report:claude', '{{\"value\":{{\"provider\":\"anthropic\",
                 \"fetchedAt\":{},\"limits\":[{week}]}}}}', 0);\n\
             BEGIN IMMEDIATE; UPDATE cache SET value='uncommitted';\n\
             .once ready\n\
             SELECT 1;",
            NOW * 1000
        );
        writeln!(writer.stdin.as_mut().unwrap(), "{sql}").unwrap();
        let ready = f.root.join("ready");
        wait_for(|| ready.exists());
        let committed = omp::read(&db, NOW, TEST_WAIT);
        let copied = ["", "-wal", "-shm"].into_iter().try_for_each(|suffix| {
            fs::copy(f.root.join(format!("agent #?% café.db{suffix}")), f.root.join(format!("copy.db{suffix}")))
                .map(|_| ())
        });
        drop(writer.stdin.take());
        assert!(writer.wait().unwrap().success() && ready.exists());
        copied.unwrap();
        assert_eq!(committed.unwrap().claude.unwrap().windows.base[1].unwrap().percent, 27.0);

        // Simulate a sidecar disappearing after milo's preflight but before SQLite opens it.
        let stub = f.root.join("sqlite-stub");
        write_executable(&stub, "/bin/rm \"${0%/*}/copy.db-shm\"; exec /usr/bin/sqlite3 \"$@\"");
        let result = omp::query(stub.as_os_str(), &f.root.join("copy.db"), TEST_WAIT);
        assert!(!f.root.join("copy.db-shm").exists());
        assert!(result.is_none());
    }

    #[test]
    fn omp_competes_with_native_sources_by_full_observation_with_no_window_mixing() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let state = f.root.join(STATE).join(SAVED_LIMITS);
        let reset = NOW + DAY;
        let codex_week = window("openai-codex:primary", "0.01", reset * 1000, 10_080, "null");
        let claude_week = window("anthropic:7d", "0.27", reset * 1000, 10_080, "null");
        report(&path, "codex", "openai-codex", NOW * 1000 - 100, &format!("[{codex_week}]"));
        report(&path, "claude", "anthropic", NOW * 1000 - 100, &format!("[{claude_week}]"));
        let secondary = codex_window(9, 300, reset);
        write(&f, CODEX, &codex(NOW - 1, 45, reset, &secondary), NOW);
        status::run(STATUS, Vec::new(), Some(&state), NOW - 1);
        let mut usage = unopened(&f, NOW);
        usage.omp = omp::Poll::new(Some(path), TEST_WAIT);
        wait_for(|| usage.check());
        let codex = usage.codex.latest().unwrap().windows.base;
        let claude = usage.claude.latest().unwrap().windows.base;
        assert!(codex[0].is_none() && codex[1].unwrap().percent == 1.0);
        assert!(claude[0].is_none() && claude[1].unwrap().percent == 27.0);

        let rollout = codex_at(&format!("{}.950Z", iso(NOW - 1)), 45, reset, &secondary);
        write(&f, CODEX, &rollout, NOW + 1);
        claude_report(&state, r#"{"seven_day":{"used_percentage":62,"resets_at":1795219200}}"#, NOW);
        assert!(usage.check());
        let codex = usage.codex.latest().unwrap().windows.base;
        let claude = usage.claude.latest().unwrap().windows.base;
        assert!(codex.into_iter().flatten().any(|window| window.percent == 45.0));
        assert!(codex.into_iter().flatten().any(|window| window.percent == 9.0));
        assert!(claude[0].unwrap().percent == 23.0 && claude[1].unwrap().percent == 62.0);
    }

    #[test]
    fn claude_native_cache_is_bounded_and_uses_its_observation_and_rfc3339_reset() {
        let f = Fixture::folder();
        let config = f.root.join(".claude.json");
        let reset = NOW + DAY;
        let stamp = iso(reset + HOUR);
        let content = |account: &str| {
            format!(
                r#"{{"oauthAccount":{{"accountUuid":"fixture-current"}},
                     "cachedUsageUtilization":{{"fetchedAtMs":{},"accountUuid":"{account}",
                      "utilization":{{"five_hour":null,"seven_day":{{"utilization":18,"resets_at":"{stamp}.987+01:00"}}}}}}}}"#,
                NOW * 1000 + 250
            )
        };
        let state = f.root.join(STATE).join(SAVED_LIMITS);
        status::run(STATUS, Vec::new(), Some(&state), NOW);
        let mut usage = unopened(&f, NOW);
        usage.claude_config = Some(config.clone());
        write(&f, ".claude.json", &content("fixture-current"), NOW);
        assert!(usage.check());
        let windows = usage.claude.latest().unwrap().windows.base;
        assert!(windows[0].is_none() && windows[1].unwrap().percent == 18.0);
        assert_eq!(windows[1].unwrap().resets, Some(reset));
        write(&f, ".claude.json", &content("fixture-other"), NOW + 1);
        assert!(usage.check());
        assert_eq!(usage.claude.latest().unwrap().windows.base[1].unwrap().percent, 61.0);
        write(&f, ".claude.json", &content("fixture-current"), NOW + 2);
        OpenOptions::new().write(true).open(config).unwrap().set_len(6 * 1024 * 1024).unwrap();
        assert!(usage.check());
        assert_eq!(usage.bytes_read, 0);
        assert_eq!(usage.claude.latest().unwrap().windows.base[1].unwrap().percent, 61.0);
    }

    #[test]
    fn omp_poll_does_not_start_another_query_before_twenty_seconds() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let clock = Instant::now();
        let mut poll = omp::Poll::new(Some(path), TEST_WAIT);
        assert!(poll.check(NOW, clock).is_none() && poll.pending());
        wait_for(|| {
            poll.check(NOW, clock);
            !poll.pending()
        });
        for seconds in 0..20_u64 {
            assert!(poll.check(NOW, clock + Duration::from_secs(seconds)).is_none());
            assert!(!poll.pending());
        }
        assert!(poll.check(NOW, clock + Duration::from_secs(20)).is_none() && poll.pending());
        wait_for(|| {
            poll.check(NOW, clock + Duration::from_secs(20));
            !poll.pending()
        });
        assert_eq!(omp::database_in(Some(&f.root), None).unwrap(), f.root.join(".omp/agent/agent.db"));
        assert_eq!(omp::database_in(None, Some(&f.root)).unwrap(), f.root.join("agent.db"));
    }

    #[test]
    fn a_deleted_omp_database_no_longer_supplies_a_saved_report() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        report(&path, "claude", "anthropic", NOW * 1000, "[]");
        let start = Instant::now();
        let mut poll = omp::Poll::new(Some(path.clone()), TEST_WAIT);
        let mut read = |clock| {
            assert!(poll.check(NOW, clock).is_none() && poll.pending());
            let mut report = None;
            wait_for(|| {
                report = poll.check(NOW, clock);
                !poll.pending()
            });
            report.unwrap()
        };
        assert!(read(start).claude.is_some());
        fs::remove_file(path).unwrap();
        assert!(read(start + Duration::from_secs(20)).claude.is_none());
    }
}

#[test]
fn json_reader_validates_nested_values_and_escaped_keys() {
    let json = br#"{"other": [null, true, false, -2.5e+2, {"escaped":"\t\uD83D\uDE00"}], "\u006bey":"caf\u00e9"}"#;
    assert!(Value::parse(json).unwrap().fields(["key"])[0].unwrap().is("café"));
    for input in [b"01".as_slice(), b"[1,]", b"{\"a\":true,}", br#"{"a":"\uD83D"}"#, b"1e-", b"-", b"+1", b"{} extra"] {
        assert!(Value::parse(input).is_none());
    }
}

#[test]
fn json_rejects_unpaired_surrogates_and_decodes_a_valid_pair() {
    let value = Value::parse(br#""\uD83D\uDE00""#).unwrap();
    assert_eq!(value.string(4).unwrap(), "\u{1f600}");
    assert!(value.string(3).is_none());
    for input in [
        br#""\uD800""#.as_slice(),
        br#""\uDFFF""#,
        br#""\uD800\u0041""#,
        br#""\uD800\uD801""#,
        br#""\uDC00\uD800""#,
        br#"{ "ignored": [ "\uDC00" ] }"#,
        br#"{ "\uDBFF": null }"#,
    ] {
        assert!(Value::parse(input).is_none(), "{input:?}");
    }
}

#[test]
fn json_accepts_exactly_32_nested_containers_and_rejects_deeper_input() {
    for (open, close) in [("[", "]"), ("{\"a\":", "}")] {
        for depth in [32, 33, 16_384] {
            let input = format!("{}null{}", open.repeat(depth), close.repeat(depth));
            assert_eq!(Value::parse(input.as_bytes()).is_some(), depth == 32, "depth {depth}");
        }
    }
}

#[test]
fn a_small_fuzz_loop_never_panics_on_malformed_input() {
    let mut random = 127_u32;
    let alphabet = b"[{}]-+1.\"\\:nil,true false\x00\xc3\xff\n";
    for size in 0..512 {
        let mut bytes = br#"{"usage": "token_count", "payload": "#.to_vec();
        bytes.extend((0..size).map(|_| {
            random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            alphabet[usize::try_from(random >> 16).unwrap() % alphabet.len()]
        }));
        if let Some(v) = Value::parse(&bytes) {
            let _ = v.fields(["usage", "payload"]);
        }
        let _ = Reading::codex(&bytes, NOW);
        let _ = Sample::parse(&bytes);
        assert_eq!(status::run(bytes.as_slice(), Vec::new(), None, NOW), 0);
    }
}
