//! Checks usage frames and provider ingestion with fixtures instead of live data.

mod accounts;
mod activity;
mod codex;
mod failures;
mod idle;
mod layout;
mod menu_stream;
mod opencode_activity;
mod profile;
mod status_line;
mod tools;
mod unchanged;

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fixture::Fixture;
use crate::fs_ops;
use crate::pane::{WATCHED, check_watched_pane, check_watched_pane_at, watch_pane};
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
const CLAUDE_ACCOUNT: &str = "00000000-0000-4000-8000-000000000001";
const OTHER_CLAUDE_ACCOUNT: &str = "00000000-0000-4000-8000-000000000002";
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

/// Codex and Claude Code keep their places, as both did before each tool had
/// a choice. Most tests here are about what a place shows, unknown included.
fn unopened(f: &Fixture, now: i64) -> Usage {
    automatic(f, now).showing(tools(Show::Always, Show::Always, Show::Auto))
}

/// The pane with no choice made: a tool has a place only while it has a figure.
fn automatic(f: &Fixture, now: i64) -> Usage {
    Usage::from_dirs(Some(f.root.join("codex")), Some(f.root.join("claude")), Some(f.root.join(STATE)), now)
}

fn tools(codex: Show, claude: Show, opencode: Show) -> UsageTools {
    let mut tools = UsageTools::default();
    for (tool, show) in UsageTool::ALL.into_iter().zip([codex, claude, opencode]) {
        tools[tool] = show;
    }
    tools
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
    let (mut watcher, mut pace) = watch_pane(&mut usage, true);
    assert_eq!(activity(&usage).today, 2000);
    let covered = if watcher.fd().is_some() { WATCHED } else { Duration::from_secs(2) };
    assert_eq!(pace.as_ref().unwrap().idle(), covered);

    fs::remove_dir(f.root.join("codex")).unwrap();
    check_watched_pane(&mut usage, &mut watcher, &mut pace, true);
    assert_eq!(pace.as_ref().unwrap().idle(), Duration::from_secs(2));
    write(&f, CODEX, &codex(now, 61, now + HOUR, "null"), now - HOUR);
    check_watched_pane(&mut usage, &mut watcher, &mut pace, true);
    assert!(frame(&mut usage, 100, 2)[0].contains("39%"));
    assert_eq!(pace.as_ref().unwrap().idle(), covered, "recreated roots are watched again when supported");
}

#[test]
fn fallback_looks_discover_new_sessions_without_a_native_watcher() {
    let f = Fixture::folder();
    let now = local_noon();
    let clock = Instant::now();
    let fallback = Duration::from_secs(2);
    fs::create_dir_all(f.root.join(STATE)).unwrap();
    write(&f, CODEX, &codex(now - 1, 64, now + DAY, "null"), now - HOUR);
    write(&f, CLAUDE, &assistant(now - 1, 1000, 0, 0), now - 1);
    let mut usage = unopened(&f, now);
    let (mut watcher, mut pace) = watch_pane(&mut usage, false);
    watcher.stop_listening();
    assert!(check_watched_pane_at(&mut usage, &mut watcher, &mut pace, true, || clock));
    assert!(watcher.scope().complete);
    assert_eq!(pace.as_ref().unwrap().idle(), fallback);
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));
    assert_eq!(activity(&usage).today, 1000);

    write(&f, OTHER_CODEX, &codex(now, 82, now + DAY, "null"), now);
    write(&f, "claude/other/new.jsonl", &assistant(now, 2000, 0, 0), now);
    assert!(check_watched_pane_at(&mut usage, &mut watcher, &mut pace, true, || clock + fallback));
    assert!(watcher.fd().is_none());
    assert!(frame(&mut usage, 100, 2)[0].contains("18%"));
    assert_eq!(activity(&usage).today, 3000);
}

#[cfg(target_os = "macos")]
#[test]
fn a_recent_codex_rollout_with_a_live_watcher_polls_without_discovery() {
    let f = Fixture::folder();
    let now = local_noon();
    let clock = Instant::now();
    write(&f, CLAUDE, &assistant(now - 1, 1000, 0, 0), now - 1);
    fs::create_dir_all(f.root.join(STATE)).unwrap();
    write(&f, CODEX, "", now);
    let mut writer = OpenOptions::new().append(true).open(f.root.join(CODEX)).unwrap();
    let mut usage = unopened(&f, now);
    let (mut watcher, mut pace) = watch_pane(&mut usage, false);
    assert!(watcher.fd().is_some());
    assert!(check_watched_pane_at(&mut usage, &mut watcher, &mut pace, true, || clock));
    assert_eq!(watcher.scope().roots.len(), 3);
    assert!(!watcher.scope().complete);
    assert_eq!(pace.as_ref().unwrap().idle(), Duration::from_secs(2));
    assert!(usage.codex.reported(usage.now()).is_none());

    let latest = codex(now - 1, 64, now + DAY, "null");
    writer.write_all(latest.as_bytes()).unwrap();
    writer.set_modified(mtime(now)).unwrap();
    write(&f, OTHER_CODEX, &codex(now, 82, now + DAY, "null"), now);
    write(&f, "claude/other/new.jsonl", &assistant(now, 2000, 0, 0), now);
    assert!(check_watched_pane_at(&mut usage, &mut watcher, &mut pace, true, || clock + Duration::from_secs(2)));
    assert_eq!(usage.bytes_read, latest.len() as u64);
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));
    assert_eq!(pace.as_ref().unwrap().idle(), Duration::from_secs(2));
    for seconds in [4, 6] {
        let next = clock + Duration::from_secs(seconds);
        assert!(check_watched_pane_at(&mut usage, &mut watcher, &mut pace, true, || next));
        assert_eq!(usage.bytes_read, 0);
        assert!(frame(&mut usage, 100, 2)[0].contains("36%"));
        assert_eq!(activity(&usage).today, 1000);
    }
    drop(writer);

    usage.fixed_now = Some(now + CODEX_LIVE + MINUTE);
    assert!(check_watched_pane_at(&mut usage, &mut watcher, &mut pace, true, || clock + Duration::from_secs(8)));
    assert!(watcher.scope().complete);
    assert_eq!(pace.as_ref().unwrap().idle(), WATCHED);
    assert_eq!(usage.bytes_read, 0);

    assert!(check_watched_pane_at(&mut usage, &mut watcher, &mut pace, true, || clock + WATCHED));
    assert!(frame(&mut usage, 100, 2)[0].contains("18%"));
    assert_eq!(activity(&usage).today, 3000);
}

#[test]
fn notification_bursts_refresh_known_files_before_discovering_new_sessions() {
    let f = Fixture::folder();
    let now = local_noon();
    let clock = Instant::now();
    fs::create_dir_all(f.root.join(STATE)).unwrap();
    write(&f, CODEX, &codex(now - 2, 23, now + DAY, "null"), now - HOUR);
    write(&f, CLAUDE, &assistant(now - 2, 1000, 0, 0), now - 2);
    let mut usage = unopened(&f, now);
    assert!(usage.refresh(now, clock, false));
    assert!(usage.watch_scope().complete);

    let latest = codex(now - 1, 64, now + DAY, "null");
    let extra = assistant(now - 1, 2000, 0, 0);
    for (path, line, modified) in [(CODEX, &latest, now - HOUR + 1), (CLAUDE, &extra, now)] {
        let mut file = OpenOptions::new().append(true).open(f.root.join(path)).unwrap();
        file.write_all(line.as_bytes()).unwrap();
        file.set_modified(mtime(modified)).unwrap();
    }
    write(&f, "codex/2026/11/19/rollout-new.jsonl", &codex(now, 82, now + DAY, "null"), now - HOUR + 2);
    write(&f, "claude/other/new.jsonl", &assistant(now, 4000, 0, 0), now);

    usage.filesystem_changed();
    assert!(usage.refresh(now, clock + Duration::from_millis(120), false));
    assert_eq!(usage.bytes_read, (latest.len() + extra.len() + 1) as u64);
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));
    assert_eq!(activity(&usage).today, 3000);
    assert!(!usage.watch_scope().complete, "a deferred discovery still needs a look");
    for ms in [240, 360, 480] {
        usage.filesystem_changed();
        assert!(usage.refresh(now, clock + Duration::from_millis(ms), false));
        assert_eq!(usage.bytes_read, 0);
    }

    assert!(usage.refresh(now, clock + Duration::from_secs(2), false));
    assert!(frame(&mut usage, 100, 2)[0].contains("18%"));
    assert_eq!(activity(&usage).today, 7000);
    assert!(usage.watch_scope().complete);
}

#[test]
fn quick_idle_looks_do_not_walk_the_other_store_while_a_root_is_missing() {
    let f = Fixture::folder();
    let now = local_noon();
    let clock = Instant::now();
    fs::create_dir_all(f.root.join(STATE)).unwrap();
    write(&f, CLAUDE, &assistant(now, 1000, 0, 0), now);
    let mut usage = unopened(&f, now);
    assert!(usage.refresh(now, clock, false));
    assert!(!usage.watch_scope().complete);

    write(&f, "claude/other/new.jsonl", &assistant(now, 2000, 0, 0), now);
    for seconds in [2, 4, 6] {
        assert!(usage.refresh(now, clock + Duration::from_secs(seconds), false));
        assert_eq!(usage.bytes_read, 0);
        assert_eq!(activity(&usage).today, 1000);
    }
    write(&f, CODEX, &codex(now, 61, now + DAY, "null"), now - HOUR);
    assert!(usage.refresh(now, clock + Duration::from_secs(8), false));
    assert!(frame(&mut usage, 100, 2)[0].contains("39%"));
    assert!(usage.watch_scope().complete);
    assert_eq!(activity(&usage).today, 1000);

    assert!(usage.refresh(now, clock + WATCHED, false));
    assert_eq!(activity(&usage).today, 3000);
}

#[test]
fn manual_refresh_and_a_new_local_day_bypass_the_discovery_rest() {
    let f = Fixture::folder();
    let midnight = Today::at(NOW, when::local_day(NOW)).since;
    let before = midnight - 1;
    let clock = Instant::now();
    write(&f, CLAUDE, &assistant(before, 1000, 0, 0), before);
    let mut usage = unopened(&f, before);
    assert!(usage.refresh(before, clock, false));

    write(&f, "claude/other/manual.jsonl", &assistant(before, 2000, 0, 0), before);
    assert_eq!(usage.key(Key::Char('r')), After::Stay);
    assert_eq!(activity(&usage).today, 3000);

    usage.fixed_now = Some(midnight);
    assert!(!caption(&mut usage).contains("tokens today"));
    write(&f, "claude/other/today.jsonl", &assistant(midnight, 4000, 0, 0), midnight);
    assert!(usage.refresh(midnight, clock + Duration::from_millis(120), false));
    assert_eq!(activity(&usage).today, 4000);
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

/// Fixture databases are made with the same sqlite3 the readers run.
fn sqlite_installed() -> bool {
    let installed = Command::new(sqlite::executable()).arg("-version").output().is_ok();
    if !installed {
        eprintln!("skipping database fixture: sqlite3 is not installed");
    }
    installed
}

fn exec(path: &Path, sql: &str) {
    let result =
        Command::new(sqlite::executable()).args(["-batch", "-init", "/dev/null"]).arg(path).arg(sql).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
}

mod omp_limits {
    use std::process::{Command, Stdio};

    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE cache(key TEXT PRIMARY KEY, value TEXT, expires_at INTEGER);
        CREATE TABLE usage_history(id INTEGER PRIMARY KEY, recorded_at INTEGER, provider TEXT,
            account_key TEXT NOT NULL, email TEXT, account_id TEXT, limit_id TEXT, label TEXT, window_label TEXT,
            used_fraction REAL, status TEXT, resets_at INTEGER);
    ";

    // OMP v18.18.0's series and time indexes. Keep unindexed fixtures too.
    pub(super) const HISTORY_INDEXES: &str = "
        CREATE INDEX idx_usage_history_series ON usage_history(provider,account_key,limit_id,recorded_at);
        CREATE INDEX idx_usage_history_recorded ON usage_history(recorded_at);
    ";

    pub(super) fn sqlite(f: &Fixture) -> Option<PathBuf> {
        if !sqlite_installed() {
            return None;
        }
        let path = f.root.join("agent.db");
        exec(&path, SCHEMA);
        Some(path)
    }

    pub(super) fn report(path: &Path, key: &str, provider: &str, ms: i64, limits: &str) {
        exec(
            path,
            &format!(
                r#"INSERT OR REPLACE INTO cache VALUES ('usage_cache:report:{provider}:default:oauth|account:{key}',
                   '{{"value":{{"provider":"{provider}","fetchedAt":{ms},"limits":{limits}}}}}',0);"#
            ),
        );
    }

    pub(super) fn window(id: &str, fraction: &str, reset_ms: i64, minutes: u64, tier: &str) -> String {
        let duration_ms = minutes * 60_000;
        format!(
            r#"{{"id":"{id}","amount":{{"usedFraction":{fraction}}},"scope":{{"tier":{tier}}},
                 "window":{{"durationMs":{duration_ms},"resetsAt":{reset_ms}}}}}"#
        )
    }

    pub(super) fn wait_for(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + TEST_WAIT * 2;
        while !ready() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn finish_omp(usage: &mut Usage) {
        assert!(matches!(wait_omp(usage), External::Redraw));
    }

    /// The pane's external fd must wake without a filesystem event or another scheduled check.
    pub(super) fn wait_omp(usage: &mut Usage) -> External {
        let mut ready = libc::pollfd { fd: usage.external_fd().unwrap(), events: libc::POLLIN, revents: 0 };
        let timeout = i32::try_from(TEST_WAIT.as_millis()).unwrap();
        loop {
            // SAFETY: The single pollfd is live, and usage still owns its descriptor.
            let result = unsafe { libc::poll(&raw mut ready, 1, timeout) };
            if result == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            assert_eq!(result, 1, "OMP query did not wake the pane");
            break;
        }
        let result = usage.check_external();
        assert!(usage.external_fd().is_none());
        assert!(matches!(usage.check_external(), External::Unchanged));
        result
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
                 INSERT INTO usage_history(provider, account_key, recorded_at, limit_id, used_fraction, resets_at, window_label)
                 VALUES ('anthropic','oauth|account:key-only-secret',{now}-2000,'anthropic:5h',0.92,{reset},'5 Hours'),
                        ('anthropic','oauth|account:key-only-secret',{now}-950,'anthropic:7d',0.29,{reset},'7 Day'),
                        ('anthropic','oauth|account:key-only-secret',{now}-900,'anthropic:7d',0.27,{reset},'7 Day'),
                        ('openai-codex','oauth|account:codex',{now}-2000,'openai-codex:primary',0.8,{reset},'7 days');"
            ),
        );

        let sql = omp::sql(NOW);
        let bytes = sqlite::query(sqlite::executable(), &path, &sql, TEST_WAIT).unwrap();
        assert!(!sql.to_ascii_lowercase().contains("auth_credentials"));
        assert!(sql.trim_start().starts_with("PRAGMA query_only=ON;"));
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
                "INSERT INTO usage_history(provider, account_key, recorded_at, limit_id, used_fraction, resets_at, window_label)
                 VALUES ('anthropic','oauth|account:recent-empty',{now}-11,'anthropic:7d',0.27,NULL,'7 Day'),
                        ('anthropic','oauth|account:recent-empty',{now},'anthropic:7d:fable',0.53,NULL,'7 Day'),
                        ('openai-codex','oauth|account:codex',{now},'openai-codex:primary',0.01,NULL,'7 days');"
            ),
        );
        let reading = omp::read(&path, NOW, TEST_WAIT).unwrap();
        let claude = reading.claude.unwrap().windows;
        assert!(claude.base.into_iter().all(|window| window.is_none()));
        assert_eq!(claude.model_week.unwrap().label(), "Fable");
        assert_eq!(reading.codex.unwrap().windows.base[1].unwrap().percent, 1.0);
    }

    #[test]
    fn misdated_omp_rows_do_not_displace_eligible_reports_or_history() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let now = NOW * 1000;
        let latest = (NOW + CLOCK_SKEW) * 1000 + 999;
        let claude = window("anthropic:7d", "0.27", (NOW + DAY) * 1000, 10_080, "null");
        report(&path, "current", "anthropic", now, &format!("[{claude}]"));
        report(&path, "future", "anthropic", latest + 1, "[]");
        exec(
            &path,
            &format!(
                "INSERT INTO usage_history(provider, account_key, recorded_at, limit_id, used_fraction, window_label)
                 VALUES ('openai-codex','oauth|account:codex',{latest},'openai-codex:primary',0.25,'5 Hours'),
                        ('openai-codex','oauth|account:codex',{latest}+1,'openai-codex:primary',0.50,'5 Hours');"
            ),
        );

        let reports = omp::read(&path, NOW, TEST_WAIT).unwrap();
        assert_eq!(reports.claude.unwrap().windows.base[1].unwrap().percent, 27.0);
        assert_eq!(reports.codex.unwrap().windows.base[0].unwrap().percent, 25.0);
    }

    #[test]
    fn omp_does_not_combine_usage_from_different_accounts_of_one_provider() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let now = NOW * 1000;
        let week = window("anthropic:7d", "0.27", (NOW + DAY) * 1000, 10_080, "null");
        report(&path, "claude-current", "anthropic", now - 10, &format!("[{week}]"));
        exec(
            &path,
            &format!(
                "INSERT INTO usage_history(provider, account_key, recorded_at, limit_id, used_fraction, window_label)
                 VALUES ('anthropic','oauth|account:claude-current',{now}-9,'anthropic:5h',0.25,'5 Hours'),
                        ('anthropic','oauth|account:claude-other',{now}-8,'anthropic:7d',0.95,'7 Day'),
                        ('openai-codex','oauth|account:codex-new',{now}-7,'openai-codex:primary',0.50,'5 Hours'),
                        ('openai-codex','oauth|account:codex-old',{now}-8,'openai-codex:primary',0.95,'5 Hours'),
                        ('openai-codex','oauth|account:codex-old',{now}-8,'openai-codex:secondary',0.95,'7 Day');"
            ),
        );

        let reports = omp::read(&path, NOW, TEST_WAIT).unwrap();
        let [five, week] = reports.claude.unwrap().windows.base;
        assert_eq!(five.unwrap().percent, 25.0);
        assert_eq!(week.unwrap().percent, 27.0);
        let [five, week] = reports.codex.unwrap().windows.base;
        assert_eq!(five.unwrap().percent, 50.0);
        assert!(week.is_none());
    }

    #[test]
    fn omp_breaks_history_ties_by_row_id_for_both_the_account_and_its_limits() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let now = NOW * 1000;
        exec(
            &path,
            &format!(
                "INSERT INTO usage_history(id, provider, account_key, recorded_at, limit_id, used_fraction, window_label)
                 VALUES (9,'anthropic','oauth|account:zulu',{now},'anthropic:7d',0.375,'7 Day'),
                        (2,'anthropic','oauth|account:alpha',{now},'anthropic:5h',0.125,'5 Hours'),
                        (8,'anthropic','oauth|account:zulu',{now},'anthropic:5h',0.50,'5 Hours'),
                        (7,'anthropic','oauth|account:zulu',{now},'anthropic:5h',0.25,'5 Hours'),
                        (4,'anthropic','oauth|account:alpha',{now},'anthropic:7d:fable',0.625,'7 Day'),
                        (10,'anthropic','oauth|account:alpha',{now}-1,'anthropic:7d',0.875,'7 Day');"
            ),
        );
        let limits = omp::read(&path, NOW, TEST_WAIT).unwrap().claude.unwrap().windows;
        assert_eq!(limits.base.map(|entry| entry.map(|window| window.percent)), [Some(50.0), Some(37.5)]);
        assert!(limits.model_week.is_none(), "history from the other account must not fill a missing limit");

        // A cache report pins the account despite the history tie and wins over history at the same instant.
        let week = window("anthropic:7d", "0.75", (NOW + DAY) * 1000, 10_080, "null");
        report(&path, "alpha", "anthropic", now - 1, &format!("[{week}]"));
        let limits = omp::read(&path, NOW, TEST_WAIT).unwrap().claude.unwrap().windows;
        assert_eq!(limits.base.map(|entry| entry.map(|window| window.percent)), [Some(12.5), Some(75.0)]);
        assert_eq!(limits.model_week.unwrap().window.percent, 62.5);
    }

    #[test]
    fn omp_history_breaks_timestamp_ties_without_reviving_an_older_limit() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        exec(&path, HISTORY_INDEXES);
        let now = NOW * 1000;
        let future = (NOW + CLOCK_SKEW + 1) * 1000;
        let five = window("anthropic:5h", "0.125", (NOW + HOUR) * 1000, 300, "null");
        let week = window("anthropic:7d", "0.375", (NOW + DAY) * 1000, 10_080, "null");
        report(&path, "current", "anthropic", now - 1000, &format!("[{five},{week}]"));
        // A newer write made the five-hour limit unknown. The subsequent
        // backfill has a higher row ID but an older observation time.
        exec(
            &path,
            &format!(
                "INSERT INTO usage_history(provider, account_key, recorded_at, limit_id, used_fraction, window_label)
                 VALUES ('anthropic','oauth|account:current',{now}-500,'anthropic:5h',0.25,'5 Hours'),
                        ('anthropic','oauth|account:current',{now}-500,'anthropic:5h',NULL,'5 Hours'),
                        ('anthropic','oauth|account:current',{now}-900,'anthropic:5h',0.75,'5 Hours'),
                        ('anthropic','oauth|account:other',{now}-100,'anthropic:7d',0.875,'7 Day'),
                        ('anthropic','oauth|account:current',{now}-1000,'anthropic:7d',0.50,'7 Day'),
                        ('anthropic','oauth|account:current',{future},'anthropic:5h',0.625,'5 Hours');"
            ),
        );
        let reports = omp::read(&path, NOW, TEST_WAIT).unwrap();
        assert_eq!(reports.ahead, Some(future));
        let limits = reports.claude.unwrap().windows;
        assert_eq!(limits.base.map(|entry| entry.map(|window| window.percent)), [None, Some(37.5)]);

        let reports = omp::read(&path, NOW + 1, TEST_WAIT).unwrap();
        assert_eq!(reports.ahead, None);
        let limits = reports.claude.unwrap().windows;
        assert_eq!(limits.base.map(|entry| entry.map(|window| window.percent)), [Some(62.5), Some(37.5)]);
    }

    #[test]
    fn omp_errors_are_quiet_and_oversized_output_is_cut_off() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        assert!(omp::read(&f.root.join("missing.db"), NOW, TEST_WAIT).is_none());
        let sql = omp::sql(NOW);
        assert!(sqlite::query(f.root.join("missing-sqlite").as_os_str(), &path, &sql, TEST_WAIT).is_none());
        let executable = f.root.join("sqlite-stub");
        let script = |lines: &str| write_executable(&executable, lines);
        script("printf '%s' 'not JSON'");
        let bytes = sqlite::query(executable.as_os_str(), &path, &sql, TEST_WAIT).unwrap();
        assert!(omp::parse(&bytes, NOW).is_none());
        script("printf '%s\\n' \"$$\" >\"$0.pid\"; printf '%70000s' x");
        assert!(sqlite::query(executable.as_os_str(), &path, &sql, TEST_WAIT).is_none());
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
        assert!(sqlite::query(std::ffi::OsStr::new("/usr/bin/true"), &path, &omp::sql(NOW), TEST_WAIT).is_none());
    }

    #[test]
    fn omp_reads_committed_wal_limits_during_a_write_without_recreating_a_lost_sidecar() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let db = f.root.join("agent #?% café.db");
        fs::rename(path, &db).unwrap();
        let week = window("anthropic:7d", "0.27", (NOW + DAY) * 1000, 10_080, "null");
        let mut writer = Command::new(sqlite::executable())
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
        let result = sqlite::query(stub.as_os_str(), &f.root.join("copy.db"), &omp::sql(NOW), TEST_WAIT);
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
        usage.omp = omp::poll(Some(path), TEST_WAIT);
        assert!(usage.check());
        finish_omp(&mut usage);
        let (codex, codex_source) = usage.codex.reported(usage.now()).unwrap();
        let (claude, claude_source) = usage.claude.reported(usage.now()).unwrap();
        assert_eq!(codex_source.label(), "OMP usage database");
        assert_eq!(claude_source.label(), "OMP usage database");
        let (codex, claude) = (codex.windows.base, claude.windows.base);
        assert!(codex[0].is_none() && codex[1].unwrap().percent == 1.0);
        assert!(claude[0].is_none() && claude[1].unwrap().percent == 27.0);

        let rollout = codex_at(&format!("{}.950Z", iso(NOW - 1)), 45, reset, &secondary);
        write(&f, CODEX, &rollout, NOW + 1);
        claude_report(&state, r#"{"seven_day":{"used_percentage":62,"resets_at":1795219200}}"#, NOW);
        assert!(usage.check());
        let (codex, codex_source) = usage.codex.reported(usage.now()).unwrap();
        let (claude, claude_source) = usage.claude.reported(usage.now()).unwrap();
        assert_eq!(codex_source.label(), "Codex session log");
        assert_eq!(claude_source.label(), "Milo Claude status line");
        let (codex, claude) = (codex.windows.base, claude.windows.base);
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
                r#"{{"oauthAccount":{{"accountUuid":"{CLAUDE_ACCOUNT}"}},
                     "cachedUsageUtilization":{{"fetchedAtMs":{},"accountUuid":"{account}",
                      "utilization":{{"five_hour":null,"seven_day":{{"utilization":18,"resets_at":"{stamp}.987+01:00"}}}}}}}}"#,
                NOW * 1000 + 250
            )
        };
        let state = f.root.join(STATE).join(SAVED_LIMITS);
        let mut usage = unopened(&f, NOW);
        usage.claude_config = Some(config.clone());
        write(&f, ".claude.json", &content(CLAUDE_ACCOUNT), NOW);
        let target = status::SaveTo { path: &state, config: Some(&config) };
        status::render(STATUS, Vec::new(), Some(target), NOW);
        assert!(usage.check());
        let (reading, source) = usage.claude.reported(usage.now()).unwrap();
        assert_eq!(source.label(), "Claude Code usage cache");
        let windows = reading.windows.base;
        assert!(windows[0].is_none() && windows[1].unwrap().percent == 18.0);
        assert_eq!(windows[1].unwrap().resets, Some(reset));
        write(&f, ".claude.json", &content(OTHER_CLAUDE_ACCOUNT), NOW + 1);
        assert!(usage.check());
        assert_eq!(usage.claude.reported(usage.now()).unwrap().0.windows.base[1].unwrap().percent, 61.0);
        write(&f, ".claude.json", &content(CLAUDE_ACCOUNT), NOW + 2);
        OpenOptions::new().write(true).open(config).unwrap().set_len(6 * 1024 * 1024).unwrap();
        assert!(usage.check());
        assert_eq!(usage.bytes_read, 0);
        assert!(usage.claude.reported(usage.now()).is_none());
    }

    #[test]
    fn omp_pending_is_distinct_from_no_source_and_manual_reload_wakes_the_pane() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let reset = (NOW + DAY) * 1000;
        let week = |fraction| window("openai-codex:primary", fraction, reset, 10_080, "null");
        report(&path, "codex", "openai-codex", NOW * 1000, &format!("[{}]", week("0.25")));
        let mut usage = unopened(&f, NOW);
        usage.omp = omp::poll(Some(path.clone()), TEST_WAIT);

        assert!(usage.check());
        assert_eq!(frame(&mut usage, 22, 2), ["Codex · checking", "Claude · checking"]);
        finish_omp(&mut usage);
        assert_eq!(frame(&mut usage, 22, 2), ["Codex week · 75% left", "Claude no saved limits"]);

        report(&path, "codex", "openai-codex", NOW * 1000, &format!("[{}]", week("0.65")));
        assert!(usage.check());
        assert!(usage.external_fd().is_none(), "automatic checks still respect the cooldown");
        assert_eq!(usage.key(Key::Char('r')), After::Stay);
        assert_eq!(frame(&mut usage, 22, 2), ["Codex week · 75% left", "Claude · checking"]);
        finish_omp(&mut usage);
        assert_eq!(frame(&mut usage, 22, 2), ["Codex week · 35% left", "Claude no saved limits"]);
    }

    #[test]
    fn background_omp_polling_does_not_replace_settled_hints_with_checking() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        report(&path, "claude", "anthropic", NOW * 1000, "[]");
        let mut usage = unopened(&f, NOW);
        usage.omp = omp::poll(Some(path.clone()), TEST_WAIT);

        let checking = ["Codex · checking", "Claude · checking"];
        let settled = ["Codex · no sessions", "Claude · no limits"];
        assert!(usage.check());
        let next_poll = Instant::now() + Duration::from_secs(20);
        assert_eq!(frame(&mut usage, 22, 2), checking);
        finish_omp(&mut usage);
        assert_eq!(frame(&mut usage, 22, 2), settled);

        usage.omp.start(NOW, next_poll);
        assert_eq!(frame(&mut usage, 22, 2), settled);
        assert!(matches!(wait_omp(&mut usage), External::Unchanged));
        assert_eq!(frame(&mut usage, 22, 2), settled);

        assert_eq!(usage.key(Key::Char('r')), After::Stay);
        assert_eq!(frame(&mut usage, 22, 2), ["Codex · checking", "Claude · no limits"]);
        finish_omp(&mut usage);
        assert_eq!(frame(&mut usage, 22, 2), settled);

        let week = window("anthropic:7d", "0.32", (NOW + DAY) * 1000, 10_080, "null");
        report(&path, "claude", "anthropic", NOW * 1000, &format!("[{week}]"));
        usage.omp.start(NOW, Instant::now() + Duration::from_secs(20));
        finish_omp(&mut usage);
        assert_eq!(frame(&mut usage, 22, 2), ["Codex · no sessions", "Claude week · 68% left"]);
    }

    #[test]
    fn a_report_omp_saved_gives_its_tool_a_place_with_nothing_else_of_the_tool_on_disk() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let week = window("anthropic:7d", "0.32", (NOW + DAY) * 1000, 10_080, "null");
        report(&path, "claude", "anthropic", NOW * 1000, &format!("[{week}]"));
        // Another provider's report is no tool of Milo's, and an empty one has no figure.
        report(&path, "gemini", "google", NOW * 1000, &format!("[{week}]"));
        report(&path, "codex", "openai-codex", NOW * 1000, "[]");
        let mut usage = automatic(&f, NOW).with_databases(Some(path), None, TEST_WAIT);
        assert!(usage.check());
        assert_eq!(frame(&mut usage, 22, 2), ["Checking local usage…", ""]);
        finish_omp(&mut usage);
        assert_eq!(
            [usage.codex.presence(usage.codex.reported(NOW)), usage.claude.presence(usage.claude.reported(NOW))],
            [Presence::Found, Presence::Reading]
        );
        assert_eq!(frame(&mut usage, 22, 2), ["Claude week · 68% left", "Resets in 1d"]);
    }

    #[test]
    fn omp_poll_does_not_start_another_query_before_twenty_seconds() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let clock = Instant::now();
        let mut poll = omp::poll(Some(path), TEST_WAIT);
        poll.start(NOW, clock);
        assert!(poll.fd().is_some());
        wait_for(|| poll.collect().is_some());
        for seconds in 0..20_u64 {
            poll.start(NOW, clock + Duration::from_secs(seconds));
            assert!(poll.fd().is_none());
        }
        poll.start(NOW, clock + Duration::from_secs(20));
        assert!(poll.fd().is_some());
        wait_for(|| poll.collect().is_some());
        assert_eq!(omp::database_in(Some(&f.root), None).unwrap(), f.root.join(".omp/agent/agent.db"));
        assert_eq!(omp::database_in(None, Some(&f.root)).unwrap(), f.root.join("agent.db"));
    }

    #[test]
    fn a_deleted_omp_database_no_longer_supplies_a_saved_report() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        report(&path, "claude", "anthropic", NOW * 1000, "[]");
        let start = Instant::now();
        let mut poll = omp::poll(Some(path.clone()), TEST_WAIT);
        let mut read = |clock| {
            poll.start(NOW, clock);
            assert!(poll.fd().is_some());
            let mut report = None;
            wait_for(|| {
                report = poll.collect();
                report.is_some()
            });
            report.unwrap().unwrap()
        };
        assert!(read(start).claude.is_some());
        fs::remove_file(path).unwrap();
        assert!(read(start + Duration::from_secs(20)).claude.is_none());
    }

    #[test]
    fn omp_and_opencode_reads_started_together_both_reach_the_pane() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let now = local_noon();
        let week = window("anthropic:7d", "0.27", (now + DAY) * 1000, 10_080, "null");
        report(&path, "claude", "anthropic", now * 1000, &format!("[{week}]"));
        let db = opencode_activity::busy_day(&f, opencode_activity::RELEASES[3], now);
        let mut usage = unopened(&f, now);
        usage.omp = omp::poll(Some(path), TEST_WAIT);
        usage.opencode = OpenCode::new(Some(db.path.clone()), TEST_WAIT);

        usage.check();
        assert!(usage.omp.fd().is_some() && usage.opencode.poll.fd().is_some());
        opencode_activity::settle(&mut usage);
        let rows = frame(&mut usage, 100, 9);
        let claude = rows.iter().any(|row| row.contains("left · resets in 1 d"));
        assert!(claude && rows[7].contains("OpenCode · 1.5k"), "{rows:?}");
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
        let _ = Reading::codex(&bytes);
        let _ = Sample::parse(&bytes);
        assert_eq!(status::run(bytes.as_slice(), Vec::new(), None, NOW), 0);
    }
}
