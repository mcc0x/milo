use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::os::unix::net::UnixStream;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fixture::Fixture;
use crate::width::str_width;

use super::time::HOUR;
use super::*;

const NOW: i64 = 1_795_100_000;
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
fn codex_shows_percent_window_and_time_until_reset() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 23, NOW + 2 * HOUR + 10 * MINUTE, "null"), NOW);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[0].contains("week") && rows[1].contains("23%") && rows[2].contains("resets in 2 h 10 m"));
}

#[test]
fn newest_snapshot_wins_across_files_regardless_of_file_order_or_mtime() {
    let f = Fixture::folder();
    let stamp = iso(NOW - 60);
    write(&f, CODEX, &codex_at(&format!("{stamp}.901Z"), 67, NOW + HOUR, "null"), NOW - 60);
    write(&f, OTHER_CODEX, &codex_at(&format!("{stamp}.101Z"), 13, NOW + HOUR, "null"), NOW);
    let rows = frame(&mut open(&f, NOW), 85, 2);
    assert!(rows[0].contains("67%") && !rows[0].contains("13%"));
}

#[test]
fn codex_clamps_overfull_windows_and_accepts_whole_minutes_written_as_floats() {
    let f = Fixture::folder();
    for (percent, minutes) in [("100.5", "300.0"), ("150", "3e2")] {
        let window = format!(r#"{{"used_percent":{percent},"window_minutes":{minutes}}}"#);
        write(&f, CODEX, &codex_snapshot(&format!("{}Z", iso(NOW)), &window, "null"), NOW);
        let rows = frame(&mut open(&f, NOW), 100, 2);
        assert!(rows[0].contains("5h 100%"));
    }
}

#[test]
fn a_nonzero_window_near_an_endpoint_does_not_look_empty_or_full() {
    let f = Fixture::folder();
    for (percent, shown) in [("0.4", "<1%"), ("99.9", "99+%")] {
        let window = format!(r#"{{"used_percent":{percent},"window_minutes":300}}"#);
        write(&f, CODEX, &codex_snapshot(&format!("{}Z", iso(NOW)), &window, "null"), NOW);
        assert!(frame(&mut open(&f, NOW), 100, 2)[0].contains(shown));
    }
}

#[test]
fn an_unreadable_codex_report_supersedes_older_figures() {
    let f = Fixture::folder();
    let previous = codex(NOW - MINUTE, 40, NOW + DAY, "null");
    for (percent, minutes) in [(r#""55""#, "300"), ("-1", "300"), ("55", "null"), ("55", "0"), ("55", "300.5")] {
        let bad = format!(r#"{{"used_percent":{percent},"window_minutes":{minutes}}}"#);
        let newest = codex_snapshot(&format!("{}Z", iso(NOW)), &bad, "null");
        write(&f, CODEX, &format!("{previous}{newest}"), NOW);
        write(&f, OTHER_CODEX, &previous, NOW - MINUTE);
        let rows = frame(&mut open(&f, NOW), 140, 3);
        let left = |y: usize| rows[y].split_once('│').unwrap().0;
        assert!(left(1).contains('—') && !left(1).contains("40%"));
        assert!(left(2).contains("could not read Codex's figures"));
    }
}

#[test]
fn codex_allows_a_little_clock_skew_but_ignores_far_future_events() {
    let f = Fixture::folder();
    let reset = NOW + HOUR;
    let data = [
        codex(NOW - MINUTE, 41, reset, "null"),
        codex(NOW + 5 * MINUTE, 62, reset, "null"),
        codex(NOW + 5 * MINUTE + 1, 95, reset, "null"),
    ]
    .concat();
    write(&f, CODEX, &data, NOW);
    let rows = frame(&mut open(&f, NOW), 100, 2);
    assert!(rows[0].contains("62%") && !rows[0].contains("41%") && !rows[0].contains("95%"));
}

#[test]
fn a_millisecond_reset_has_the_same_countdown_as_seconds() {
    let f = Fixture::folder();
    let reset = NOW + 2 * HOUR;
    example_limits(&f, Some(NOW), Some(NOW), reset * 1000, NOW);
    let mut usage = open(&f, NOW);
    assert_eq!(frame(&mut usage, 140, 3)[2].matches("resets in 2 h").count(), 2);
    usage.fixed_now = Some(reset);
    assert_eq!(frame(&mut usage, 140, 3)[1].matches("0%").count(), 2);
}

#[test]
fn codex_shows_reset_when_the_time_passes_without_an_append() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 77, NOW + 5, "null"), NOW);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 85, 2)[0].contains("77%"));
    assert_eq!(usage.next_frame(), Some(Duration::from_secs(5)));
    usage.fixed_now = Some(NOW + 4);
    assert!(frame(&mut usage, 85, 2)[0].contains("77%"));
    usage.fixed_now = Some(NOW + 5);
    assert_eq!(usage.next_frame(), Some(Duration::from_secs(60)));
    let rows = frame(&mut usage, 140, 3);
    assert!(rows[1].contains("0%") && !rows[1].contains("77%"));
    assert!(rows[2].split('│').next().unwrap().contains("reset"));
}

#[test]
fn an_old_codex_snapshot_says_how_old_it_is() {
    let f = Fixture::folder();
    let then = NOW - 10 * DAY;
    write(&f, CODEX, &codex(then, 33, NOW + DAY, "null"), then);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].contains("33%"));
    assert!(rows[2].contains("10 d ago"));
}

#[test]
fn a_reading_shows_its_age_at_five_minutes_but_not_one_second_earlier() {
    let f = with_limits();
    let mut usage = open(&f, NOW + 5 * MINUTE - 1);
    assert!(!frame(&mut usage, 140, 3)[2].contains("ago"));
    usage.fixed_now = Some(NOW + 5 * MINUTE);
    assert_eq!(frame(&mut usage, 140, 3)[2].matches("5 m ago").count(), 2);
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
fn codex_ignores_null_secondary_and_shows_a_present_one() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 23, NOW + DAY, "null"), NOW);
    assert_eq!(frame(&mut open(&f, NOW), 100, 2)[0].matches('%').count(), 1);
    let secondary = codex_window(89, 300, NOW + HOUR);
    write(&f, CODEX, &codex(NOW, 23, NOW + DAY, &secondary), NOW);
    let rows = frame(&mut open(&f, NOW), 100, 2);
    assert!(rows[0].contains("5h") && rows[0].contains("89%") && rows[0].contains("23%"));
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
    assert!(rows[2].contains("no Codex sessions here"));
    assert!(!rows.join("\n").contains("tokens today"));
}

#[test]
fn codex_reads_only_the_newest_plain_sessions_and_reuses_unchanged_tails() {
    let f = Fixture::folder();
    let oldest = codex(NOW, 99, NOW + DAY, "null");
    write(&f, OTHER_CODEX, &oldest, NOW - 20 * DAY);
    let kept = codex(NOW - DAY, 46, NOW + DAY, "null");
    write(&f, CODEX, &kept, NOW - 10 * DAY);
    let empty = "{}\n";
    for i in 1..files::CODEX_FILES {
        write(&f, &format!("codex/2026/11/18/rollout-{i}.jsonl"), empty, NOW);
    }
    let mut usage = open(&f, NOW);
    let rows = frame(&mut usage, 100, 2);
    assert!(rows[0].contains("46%") && !rows[0].contains("99%"));
    assert_eq!(usage.bytes_read, u64::try_from(kept.len() + 3 * empty.len()).unwrap());
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 0);
}

#[test]
fn no_codex_sessions_and_sessions_without_limits_have_different_notes() {
    let f = Fixture::folder();
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].split('│').next().unwrap().contains('—'));
    assert!(rows[2].contains("no Codex sessions here"));
    write(&f, CODEX, "{}\n", NOW);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].split('│').next().unwrap().contains('—'));
    assert!(rows[2].contains("no limits reported") && !rows[2].contains("no Codex sessions"));
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
    assert!(rows[2].contains("no Codex sessions here"));
    assert!(!rows.join("\n").contains("tokens today"));
    assert_eq!(usage.bytes_read, 0);
}

#[test]
fn claude_counts_only_regular_subagent_transcripts_in_the_session_subagents_folder() {
    let f = Fixture::folder();
    let now = local_noon();
    write(&f, AGENT, &assistant(now, 2000, 0, 0), now);
    let decoy = assistant(now, 9000, 0, 0);
    for path in [
        "claude/project/session/agent-wrong-level.jsonl",
        "claude/project/session/subagents/not-ours.jsonl",
        "claude/project/session/subagents/agent-compressed.jsonl.zst",
        "claude/project/session/subagents/nested/agent-wrong-level.jsonl",
        "claude/project/session/other/agent-wrong-folder.jsonl",
        "elsewhere/subagents/agent-symlink.jsonl",
    ] {
        write(&f, path, &decoy, now);
    }
    fs::create_dir_all(f.root.join("claude/project/other")).unwrap();
    symlink(f.root.join("elsewhere"), f.root.join("claude/project/linked")).unwrap();
    symlink(f.root.join("elsewhere/subagents"), f.root.join("claude/project/other/subagents")).unwrap();
    symlink(
        f.root.join("elsewhere/subagents/agent-symlink.jsonl"),
        f.root.join("claude/project/session/subagents/agent-link.jsonl"),
    )
    .unwrap();
    assert_eq!(activity(&open(&f, now)).today, 2000);
}

#[test]
fn the_first_usage_look_loads_data_and_missing_roots_use_the_two_second_pace() {
    let f = Fixture::folder();
    let now = local_noon();
    fs::create_dir_all(f.root.join("codex")).unwrap();
    fs::create_dir_all(f.root.join(STATE)).unwrap();
    write(&f, CLAUDE, &assistant(now, 2000, 0, 0), now);
    let mut usage = unopened(&f, now);
    let mut pace = crate::Pace::for_pane(&mut usage, crate::WATCHED, true);
    assert_eq!(activity(&usage).today, 2000);
    assert_eq!(pace.idle, crate::WATCHED);

    fs::remove_dir(f.root.join("codex")).unwrap();
    pace.look(&mut usage, true);
    assert_eq!(pace.idle, Duration::from_secs(2));
    write(&f, CODEX, &codex(now, 61, now + HOUR, "null"), now);
    pace.look(&mut usage, true);
    assert!(frame(&mut usage, 100, 2)[0].contains("61%"));
    // A new root is not guaranteed to be covered by the original watcher.
    assert_eq!(pace.idle, Duration::from_secs(2));
}

#[test]
fn claude_activity_only_shows_today_and_never_takes_the_place_of_limits() {
    let f = Fixture::folder();
    let midnight = Today::at(NOW, when::local_day(NOW)).since;
    let now = midnight + 3 * HOUR;
    let records = [
        assistant(now - 6 * HOUR, 498_000, 1000, 1000),
        assistant(now - 4 * HOUR, 300, 600, 100),
        assistant(now - HOUR, 21_000, 25_000, 2000),
    ]
    .concat();
    write(&f, CLAUDE, &records, now);
    let rows = frame(&mut open(&f, now), 140, 4);
    assert!(rows[2].contains("turn on in milo setup"));
    assert_eq!(rows[1].matches('—').count(), 3);
    assert!(!rows[1].contains('%'));
    assert!(rows[3].contains("48k tokens today"));
    assert!(!rows[..3].join("\n").contains("tokens"));
}

#[test]
fn claude_today_includes_earlier_activity_after_five_hours_have_passed() {
    let f = Fixture::folder();
    let now = local_noon();
    let records = [assistant(now - 6 * HOUR, 1_000_000, 100_000, 100_000), assistant(now - HOUR, 55, 45, 0)].concat();
    write(&f, CLAUDE, &records, now);
    assert!(caption(&mut open(&f, now)).contains("1.2M tokens today"));
}

#[test]
fn multiple_claude_blocks_for_the_same_message_are_not_counted_twice() {
    let f = Fixture::folder();
    let now = local_noon();
    let sample = |at, tokens| identified(&assistant(at, tokens, 0, 0), "example-message");
    write(&f, CLAUDE, &format!("{}{}", sample(now - 20, 1000), sample(now - 10, 2000)), now - 10);
    let mut usage = open(&f, now);
    assert_eq!(activity(&usage).today, 2000);

    let mut file = OpenOptions::new().append(true).open(f.root.join(CLAUDE)).unwrap();
    file.write_all(sample(now, 3000).as_bytes()).unwrap();
    file.set_modified(mtime(now)).unwrap();
    assert!(usage.check());
    assert_eq!(activity(&usage).today, 3000);
}

#[test]
fn resumed_and_forked_files_share_message_ids_but_not_anonymous_records() {
    let f = Fixture::folder();
    let now = local_noon();
    let anon = assistant(now - 5, 200, 0, 0);
    let older = identified(&assistant(now - 20, 2000, 0, 0), "shared");
    let updated = identified(&assistant(now - 10, 4000, 0, 0), "shared");
    let pending = identified(&assistant(now - 1, 5000, 0, 0), "shared");
    write(&f, CLAUDE, &format!("{older}{anon}"), now - 5);
    write(&f, "claude/fork/session.jsonl", &format!("{updated}{anon}"), now - 5);
    write(&f, AGENT, pending.trim_end(), now);
    let mut usage = open(&f, now);
    assert_eq!(activity(&usage).today, 5400);
    fs::remove_file(f.root.join(AGENT)).unwrap();
    assert!(usage.check());
    assert_eq!(activity(&usage).today, 4400);
}

#[test]
fn a_file_caps_daily_samples_and_marks_the_total_as_partial() {
    let f = Fixture::folder();
    let now = local_noon();
    let records = assistant(now, 1, 0, 0).repeat(MAX_SAMPLES + 1);
    write(&f, CLAUDE, &records, now);
    let usage = open(&f, now);
    assert_eq!(activity(&usage).today, MAX_SAMPLES as u64);
    assert!(activity(&usage).partial);
}

#[test]
fn first_load_skips_old_blocks_but_keeps_todays_long_unterminated_tail() {
    let f = Fixture::folder();
    let now = local_noon();
    let today = Today::at(now, when::local_day(now)).since;
    let old = assistant(today - 1, 9000, 0, 0).repeat(12_000);
    let first = assistant(today, 1000, 0, 0);
    let last = assistant(now, 2000, 0, 0).replacen('{', &format!("{{\"padding\":\"{}\",", "x".repeat(40_000)), 1);
    write(&f, CLAUDE, &format!("{old}{first}{}", last.trim_end()), now);
    let mut usage = open(&f, now);
    assert!(usage.bytes_read < 128 * 1024, "{} bytes read", usage.bytes_read);
    assert_eq!(activity(&usage).today, 3000);
    assert!(!activity(&usage).partial);

    let mut file = OpenOptions::new().append(true).open(f.root.join(CLAUDE)).unwrap();
    file.write_all(b"\n").unwrap();
    file.set_modified(mtime(now + 1)).unwrap();
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 1);
    assert_eq!(activity(&usage).today, 3000);
}

#[test]
fn a_budget_limited_first_load_resumes_backwards_then_reads_new_appends() {
    let f = Fixture::folder();
    let now = local_noon();
    let today = Today::at(now, when::local_day(now)).since;
    let first = assistant(now - 2, 1000, 0, 0);
    let second = assistant(now - 1, 2000, 0, 0);
    write(&f, CLAUDE, &format!("{first}{second}"), now - 1);
    let mut claude = Claude::default();
    let root = f.root.join("claude");
    assert!(!claude.check(Some(&root), today, &mut Budget::new(second.len() as u64 + 3)));
    assert_eq!(claude.activity(today, now).today, 2000);
    assert!(claude.activity(today, now).partial);

    let append = assistant(now, 4000, 0, 0);
    let mut file = OpenOptions::new().append(true).open(f.root.join(CLAUDE)).unwrap();
    file.write_all(append.as_bytes()).unwrap();
    file.set_modified(mtime(now)).unwrap();
    let mut budget = Budget::new(4096);
    assert!(claude.check(Some(&root), today, &mut budget));
    assert_eq!(budget.read, (first.len() - 3 + append.len()) as u64);
    assert_eq!(claude.activity(today, now).today, 7000);
    assert!(!claude.activity(today, now).partial);
}

#[test]
fn an_append_reads_only_the_new_bytes_and_keeps_previous_activity() {
    let f = Fixture::folder();
    let now = local_noon();
    let first = assistant(now - 20, 100, 1800, 100);
    write(&f, CLAUDE, &first, now - 20);
    let mut usage = open(&f, now);
    assert!(caption(&mut usage).contains("2k tokens today"));
    let path = f.root.join(CLAUDE);
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all(&vec![b'x'; first.len()]).unwrap();
    let second = assistant(now - 10, 3000, 800, 200);
    file.write_all(second.as_bytes()).unwrap();
    file.set_modified(mtime(now)).unwrap();
    assert!(usage.check());
    assert_eq!(usage.bytes_read, u64::try_from(second.len()).unwrap());
    assert!(caption(&mut usage).contains("6k tokens today"));
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 0);
}

#[test]
fn claude_truncation_and_replacement_discard_previous_samples() {
    let f = Fixture::folder();
    let now = local_noon();
    let first = assistant(now - 20, 1_000_000, 1800, 100);
    write(&f, CLAUDE, &first, now - 20);
    let mut usage = open(&f, now);
    write(&f, CLAUDE, &assistant(now - 10, 300, 500, 200), now - 10);
    assert!(usage.check());
    assert!(caption(&mut usage).contains("1k tokens today"));
    write(&f, "claude/project/replacement", &assistant(now, 900, 900, 200), now - 10);
    fs::rename(f.root.join("claude/project/replacement"), f.root.join(CLAUDE)).unwrap();
    assert!(usage.check());
    assert!(caption(&mut usage).contains("2k tokens today"));
}

#[test]
fn a_partial_last_record_becomes_visible_once_complete_without_double_counting() {
    let f = Fixture::folder();
    let now = local_noon();
    let full = assistant(now - 20, 100, 1800, 100);
    let split = full.len() / 2;
    write(&f, CLAUDE, &full[..split], now);
    let mut usage = open(&f, now);
    assert!(!caption(&mut usage).contains("tokens today"));
    let mut file = OpenOptions::new().append(true).open(f.root.join(CLAUDE)).unwrap();
    file.write_all(&full.as_bytes()[split..full.len() - 1]).unwrap();
    file.set_modified(mtime(now)).unwrap();
    assert!(usage.check());
    assert!(caption(&mut usage).contains("2k tokens today"));
    file.write_all(b"\n").unwrap();
    file.set_modified(mtime(now + 1)).unwrap();
    assert!(usage.check());
    assert!(caption(&mut usage).contains("2k tokens today"));
}

#[test]
fn claude_shows_activity_below_limits_and_resets_without_a_change_to_the_save() {
    let f = with_limits();
    write(&f, CLAUDE, &assistant(NOW, 200, 900, 100), NOW);
    let mut usage = open(&f, NOW);
    let rows = frame(&mut usage, 140, 4);
    let claude = |row: usize| rows[row].split('│').nth(1).unwrap();
    assert!(claude(0).contains("5h") && claude(0).contains("week"));
    assert!(claude(1).contains("23%") && claude(1).contains("61%") && !claude(1).contains("tokens"));
    assert!(claude(3).contains("1.2k tokens today"));
    usage.fixed_now = Some(1_795_110_000);
    let rows = frame(&mut usage, 140, 4);
    let claude = |row: usize| rows[row].split('│').nth(1).unwrap();
    assert!(claude(1).contains("0%") && !claude(1).contains("23%") && claude(1).contains("61%"));
    assert!(claude(2).contains("reset"));
}

#[test]
fn saved_claude_limits_are_still_shown_three_days_later() {
    let f = Fixture::folder();
    let then = NOW - 3 * DAY;
    let status = std::str::from_utf8(STATUS).unwrap().strip_prefix('{').unwrap();
    f.write(&format!("{STATE}/{SAVED_LIMITS}"), &format!("{{\"saved_at\":{then},{status}"));
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].contains("23%") && rows[1].contains("61%"));
    assert_eq!(rows[2].matches("3 d ago").count(), 1);
}

#[test]
fn claude_status_saves_limits_and_prints_a_short_plain_line() {
    let f = Fixture::folder();
    let path = f.root.join(STATE).join(SAVED_LIMITS);
    let mut output = Vec::new();
    assert_eq!(status::run(STATUS, &mut output, Some(&path), NOW), 0);
    assert_eq!(String::from_utf8(output).unwrap(), "5h 23% · week 61%\n");
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 100, 2)[1].contains("week"));
    let budget = &mut Budget::new(16384);
    let file = files::open_file(&path).unwrap();
    let mark = Mark::opened(&file).unwrap();
    let saved = status::saved(file, mark, budget).unwrap();
    assert_eq!(saved.windows.base[0].unwrap().observed, NOW);
    assert_eq!(saved.windows.base[1].unwrap().resets, Some(1_795_219_200));
}

fn claude_report(path: &Path, limits: &str, now: i64) {
    let input = format!("{{\"rate_limits\":{limits}}}");
    assert_eq!(status::run(input.as_bytes(), Vec::new(), Some(path), now), 0);
}

#[test]
fn claude_keeps_the_highest_figures_for_the_same_period_and_merges_partial_reports() {
    let f = Fixture::folder();
    let path = f.root.join(STATE).join(SAVED_LIMITS);
    let reset = NOW + HOUR;
    let week = NOW + DAY;
    let observed = NOW - 2 * HOUR;
    claude_report(
        &path,
        &format!(
            r#"{{"five_hour":{{"used_percentage":85,"resets_at":{reset}}},
            "seven_day":{{"used_percentage":45,"resets_at":{week}}}}}"#
        ),
        observed,
    );
    claude_report(
        &path,
        &format!(
            r#"{{"five_hour":{{"used_percentage":20,"resets_at":{reset}}},
            "seven_day":{{"used_percentage":30,"resets_at":{week}}}}}"#
        ),
        NOW,
    );
    let mut usage = open(&f, NOW);
    let rows = frame(&mut usage, 140, 3);
    assert!(rows[1].contains("85%") && rows[1].contains("45%"));
    // Keeping the higher, older figures keeps their age too.
    assert_eq!(rows[2].matches("2 h ago").count(), 1);

    let before = fs::read(&path).unwrap();
    claude_report(&path, &format!(r#"{{"five_hour":{{"used_percentage":85,"resets_at":{reset}}}}}"#), NOW);
    assert_eq!(fs::read(&path).unwrap(), before);

    claude_report(&path, &format!(r#"{{"seven_day":{{"used_percentage":55,"resets_at":{week}}}}}"#), NOW);
    assert!(usage.check());
    let rows = frame(&mut usage, 140, 3);
    assert!(rows[1].contains("85%") && rows[1].contains("55%"));
    assert_eq!(rows[2].matches("2 h ago").count(), 1);

    usage.fixed_now = Some(reset);
    let rows = frame(&mut usage, 140, 3);
    assert!(rows[1].contains("0%") && rows[2].contains("reset"));
    let next = reset + 5 * HOUR;
    claude_report(&path, &format!(r#"{{"five_hour":{{"used_percentage":5,"resets_at":{next}}}}}"#), reset);
    claude_report(&path, &format!(r#"{{"five_hour":{{"used_percentage":95,"resets_at":{reset}}}}}"#), reset);
    assert!(usage.check());
    let rows = frame(&mut usage, 140, 3);
    assert!(rows[1].split_whitespace().any(|figure| figure == "5%") && !rows[1].contains("95%"));
}

#[test]
fn a_report_without_any_valid_claude_window_neither_creates_nor_replaces_a_save() {
    let f = Fixture::folder();
    let path = f.root.join(STATE).join(SAVED_LIMITS);
    let invalid = [
        "{}",
        r#"{"five_hour":{"used_percentage":100.5}}"#,
        r#"{"seven_day":{"used_percentage":"55"}}"#,
        r#"{"other":{"used_percentage":12}}"#,
    ];
    for limits in invalid {
        claude_report(&path, limits, NOW);
        assert!(!path.exists());
    }
    assert_eq!(status::run(STATUS, Vec::new(), Some(&path), NOW), 0);
    let before = fs::read(&path).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    for limits in invalid {
        claude_report(&path, limits, NOW + HOUR);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    }
}

#[test]
fn a_claude_save_replaces_the_whole_private_file_at_once() {
    let f = Fixture::folder();
    let path = f.root.join(STATE).join(SAVED_LIMITS);
    assert_eq!(status::run(STATUS, Vec::new(), Some(&path), NOW), 0);
    let mut before = files::open_file(&path).unwrap();
    let old_bytes = fs::read(&path).unwrap();
    let previous = before.metadata().unwrap();
    assert_eq!(previous.mode() & 0o777, 0o600);
    claude_report(&path, r#"{"five_hour":{"used_percentage":53,"resets_at":1795110000}}"#, NOW + 1);
    let after = fs::metadata(&path).unwrap();
    assert_ne!(after.ino(), previous.ino());
    assert_eq!(after.mode() & 0o777, 0o600);
    let mut still_old = Vec::new();
    before.read_to_end(&mut still_old).unwrap();
    assert_eq!(still_old, old_bytes);
    assert!(frame(&mut open(&f, NOW + 1), 100, 2)[1].contains("53%"));
}

#[test]
fn claude_status_handles_missing_limits_and_strips_controls_from_model_names() {
    let mut output = Vec::new();
    let input = br#"{"rate_limits":null,"model":{"display_name":"Claude\n\u001b Test"}}"#;
    assert_eq!(status::run(input.as_slice(), &mut output, None, NOW), 0);
    assert_eq!(output, b"Claude Test\n");
}

#[test]
fn claude_status_prints_an_empty_line_and_succeeds_on_garbage_or_too_much_input() {
    let f = Fixture::folder();
    let path = f.root.join(STATE).join(SAVED_LIMITS);
    for input in [b"not JSON".as_slice(), &vec![b' '; 1024 * 1024 + 1]] {
        let mut output = Vec::new();
        assert_eq!(status::run(input, &mut output, Some(&path), NOW), 0);
        assert_eq!(output, b"\n");
        assert!(!path.exists());
    }
}

#[test]
fn claude_status_prints_the_limits_even_when_the_save_cannot_be_made() {
    let f = Fixture::folder();
    f.write(STATE, "a file, not a directory");
    let path = f.root.join(STATE).join(SAVED_LIMITS);
    for path in [None, Some(path.as_path())] {
        let mut output = Vec::new();
        assert_eq!(status::run(STATUS, &mut output, path, NOW), 0);
        assert_eq!(String::from_utf8(output).unwrap(), "5h 23% · week 61%\n");
    }
}

#[test]
fn claude_status_prints_a_plain_line_when_its_input_remains_open() {
    use std::sync::mpsc;

    let (reader, mut writer) = UnixStream::pair().unwrap();
    writer.write_all(b"{\"model\":").unwrap();
    let (send, receive) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let code = status::run_fd(reader, &mut output, None, NOW);
        let _ = send.send((code, output));
    });
    assert_eq!(receive.recv_timeout(Duration::from_secs(5)).unwrap(), (0, b"\n".to_vec()));
    drop(writer);
    thread.join().unwrap();

    let (reader, mut writer) = UnixStream::pair().unwrap();
    writer.write_all(STATUS).unwrap();
    drop(writer);
    let mut output = Vec::new();
    assert_eq!(status::run_fd(reader, &mut output, None, NOW), 0);
    assert_eq!(String::from_utf8(output).unwrap(), "5h 23% · week 61%\n");
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
    assert!(rows[1].contains("23%") && rows[3].contains("1.2k tokens today"));
}

mod omp_limits {
    use std::process::Command;

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

    fn await_check(usage: &mut Usage) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !usage.check() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
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

        let bytes = omp::query(omp::sqlite(), &path, Duration::from_secs(1)).unwrap();
        assert!(!omp::SQL.to_ascii_lowercase().contains("auth_credentials"));
        assert!(omp::SQL.trim_start().starts_with("PRAGMA query_only=ON;"));
        assert!(!String::from_utf8_lossy(&bytes).contains("key-only-secret"));
        let reading = omp::parse(&bytes, NOW).unwrap();
        let claude = reading.claude.unwrap();
        assert!(claude.at == Stamp::millis(Value::parse((now - 900).to_string().as_bytes()).unwrap()).unwrap());
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
        let reading = omp::read(&path, NOW).unwrap();
        let claude = reading.claude.unwrap().windows;
        assert!(claude.base.into_iter().all(|window| window.is_none()));
        assert_eq!(claude.model_week.unwrap().label(), "Fable");
        assert_eq!(reading.codex.unwrap().windows.base[1].unwrap().percent, 1.0);
    }

    #[test]
    fn omp_errors_are_quiet_and_stop_reading_oversized_or_stalled_children() {
        use std::os::unix::fs::PermissionsExt;

        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let wait = Duration::from_secs(3);
        assert!(omp::read(&f.root.join("missing.db"), NOW).is_none());
        assert!(omp::query(f.root.join("missing-sqlite").as_os_str(), &path, wait).is_none());
        let executable = f.root.join("sqlite-stub");
        let script = |lines: &str| {
            fs::write(&executable, format!("#!/bin/sh\n{lines}\n")).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        };
        script("printf '%s' 'not JSON'");
        let bytes = omp::query(executable.as_os_str(), &path, wait).unwrap();
        assert!(omp::parse(&bytes, NOW).is_none());
        script("printf '%70000s' x");
        assert!(omp::query(executable.as_os_str(), &path, wait).is_none());
        script("exec 1>&-; exec /bin/sleep 8");
        let wait = Duration::from_millis(250);
        let started = Instant::now();
        assert!(omp::query(executable.as_os_str(), &path, wait).is_none());
        assert!(started.elapsed() >= wait && started.elapsed() < Duration::from_secs(4));
        exec(&path, "DROP TABLE usage_history;");
        assert!(omp::read(&path, NOW).is_none());
    }

    #[test]
    fn omp_does_not_create_missing_wal_sidecars() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        exec(&path, "PRAGMA journal_mode=WAL;");
        for sidecar in ["agent.db-wal", "agent.db-shm"] {
            assert!(!f.root.join(sidecar).exists());
        }
        assert!(omp::read(&path, NOW).is_none());
        for sidecar in ["agent.db-wal", "agent.db-shm"] {
            assert!(!f.root.join(sidecar).exists());
        }
        // Use a child that would otherwise succeed even on a database without sidecars.
        assert!(omp::query(std::ffi::OsStr::new("/usr/bin/true"), &path, Duration::from_secs(1)).is_none());
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
        usage.omp = omp::Poll::new(Some(path));
        await_check(&mut usage);
        let codex = usage.codex.latest().unwrap().windows.base;
        let claude = usage.claude.saved.unwrap().base;
        assert!(codex[0].is_none() && codex[1].unwrap().percent == 1.0);
        assert!(claude[0].is_none() && claude[1].unwrap().percent == 27.0);

        let rollout = codex_at(&format!("{}.950Z", iso(NOW - 1)), 45, reset, &secondary);
        write(&f, CODEX, &rollout, NOW + 1);
        claude_report(&state, r#"{"seven_day":{"used_percentage":62,"resets_at":1795219200}}"#, NOW);
        assert!(usage.check());
        let codex = usage.codex.latest().unwrap().windows.base;
        let claude = usage.claude.saved.unwrap().base;
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
        let windows = usage.claude.saved.unwrap().base;
        assert!(windows[0].is_none() && windows[1].unwrap().percent == 18.0);
        assert_eq!(windows[1].unwrap().resets, Some(reset));
        write(&f, ".claude.json", &content("fixture-other"), NOW + 1);
        assert!(usage.check());
        assert_eq!(usage.claude.saved.unwrap().base[1].unwrap().percent, 61.0);
        write(&f, ".claude.json", &content("fixture-current"), NOW + 2);
        OpenOptions::new().write(true).open(config).unwrap().set_len(6 * 1024 * 1024).unwrap();
        assert!(usage.check());
        assert_eq!(usage.bytes_read, 0);
        assert_eq!(usage.claude.saved.unwrap().base[1].unwrap().percent, 61.0);
    }

    #[test]
    fn omp_poll_does_not_start_another_query_before_twenty_seconds() {
        let f = Fixture::folder();
        let Some(path) = sqlite(&f) else { return };
        let clock = Instant::now();
        let mut poll = omp::Poll::new(Some(path));
        assert!(poll.check(NOW, clock).is_none() && poll.pending());
        while poll.pending() {
            assert!(clock.elapsed() < Duration::from_secs(3));
            poll.check(NOW, clock);
            std::thread::yield_now();
        }
        for seconds in 0..20_u64 {
            assert!(poll.check(NOW, clock + Duration::from_secs(seconds)).is_none());
            assert!(!poll.pending());
        }
        assert!(poll.check(NOW, clock + Duration::from_secs(20)).is_none() && poll.pending());
        while poll.pending() {
            assert!(clock.elapsed() < Duration::from_secs(3));
            poll.check(NOW, clock + Duration::from_secs(20));
            std::thread::yield_now();
        }
        assert_eq!(omp::database_in(Some(&f.root), None).unwrap(), f.root.join(".omp/agent/agent.db"));
        assert_eq!(omp::database_in(None, Some(&f.root)).unwrap(), f.root.join("agent.db"));
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
        let _ = CodexReading::parse(&bytes, NOW);
        let _ = Sample::parse(&bytes);
        assert_eq!(status::run(bytes.as_slice(), Vec::new(), None, NOW), 0);
    }
}

fn example_limits(f: &Fixture, codex_at: Option<i64>, claude_at: Option<i64>, reset: i64, now: i64) {
    let week = now + 2 * DAY;
    if let Some(at) = codex_at {
        let primary = codex_window(27, 300, reset);
        let secondary = codex_window(74, 10_080, week);
        write(f, CODEX, &codex_snapshot(&format!("{}Z", iso(at)), &primary, &secondary), at);
    }
    if let Some(at) = claude_at {
        let saved = format!(
            r#"{{"rate_limits":{{"five_hour":{{"used_percentage":52,"resets_at":{reset}}},
            "seven_day":{{"used_percentage":92,"resets_at":{week}}}}}}}"#
        );
        assert_eq!(status::run(saved.as_bytes(), Vec::new(), Some(&f.root.join(STATE).join(SAVED_LIMITS)), at), 0);
    }
}

fn assert_figures(rows: &[String], width: usize, codex: (&str, &str), claude: (&str, &str)) {
    let tall = matches!(rows.len(), 5 | 12);
    let (codex, claude) = if tall { (codex.1, claude.1) } else { (codex.0, claude.0) };
    match rows.len() {
        1 if width < 15 => assert!(rows[0].contains(if codex == "—" { claude } else { codex })),
        5 | 1 => {
            let contents = |right| {
                rows.iter()
                    .map(|row| row.split_once('│').map(|(left, other)| if right { other } else { left }).unwrap())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            assert!(contents(false).contains(codex));
            assert!(contents(true).contains(claude));
        }
        2 => {
            assert!(rows[0].contains(codex) && rows[1].contains(claude));
        }
        height => {
            let rule = (height - 1) / 2;
            assert_eq!(rows[rule], "─".repeat(width));
            assert!(rows[..rule].join("\n").contains(codex));
            assert!(rows[rule + 1..].join("\n").contains(claude));
        }
    }
}

#[test]
fn size_classes_keep_the_primary_figures_in_both_panels() {
    let now = local_noon();
    let next = now + 2 * HOUR + 10 * MINUTE;
    // Each figure as plain text, and as the foot of its large form: the
    // last row of the digits with the percent sign closed up after them.
    let codex = ("27%", "█▄▄  █ %");
    let claude = ("52%", "▄▄▛ █▄▄%");
    let dash = ("—", "—");
    let zero = ("0%", "▜▄▛%");
    for (name, codex_at, claude_at, reset_at, left, right) in [
        ("both", Some(now), Some(now), next, codex, claude),
        ("Codex only", Some(now), None, next, codex, dash),
        ("Claude only", None, Some(now), next, dash, claude),
        ("neither", None, None, next, dash, dash),
        ("reset", Some(now - HOUR), Some(now - HOUR), now, zero, zero),
        ("old", Some(now - 10 * DAY), Some(now - 3 * DAY), next, codex, claude),
    ] {
        let f = Fixture::folder();
        example_limits(&f, codex_at, claude_at, reset_at, now);
        if name != "neither" {
            write(&f, CLAUDE, &assistant(now - MINUTE, 12_000, 900, 100), now);
        }
        let mut usage = open(&f, now);
        for (w, h) in [(140, 5), (100, 2), (100, 1), (36, 12), (30, 8), (12, 2), (6, 1)] {
            let rows = frame(&mut usage, w, h);
            assert_figures(&rows, w, left, right);
        }
    }
}

#[test]
fn the_two_marks_fit_four_rows_and_the_fallback_keeps_both_names() {
    let f = with_limits();
    let mut usage = open(&f, NOW);
    let mut screen = Screen::sized(140, 5);
    for able in [None, Some(false)] {
        if let Some(able) = able {
            screen.set_pictures(able);
        }
        usage.draw(&mut screen);
        assert!(screen.picture_at().is_none());
        let rows = screen.text();
        assert!(rows[2].contains("██ Codex") && rows[2].contains("██ Claude"));
    }
    screen.set_pictures(true);
    for (cell, cols) in [((9, 18), 8), ((12, 12), 4)] {
        screen.set_cell_size(cell.0, cell.1);
        usage.draw(&mut screen);
        assert_eq!(super::draw::mark_placements(&screen).collect::<Vec<_>>(), [(2, 0, cols, 4), (72, 0, cols, 4)]);
        assert!(!screen.text().join("\n").contains("██ Codex"));
        assert!(!screen.text().join("\n").contains("██ Claude"));
    }
}

#[test]
fn frame_rows_are_the_same_in_all_data_states() {
    let now = local_noon();
    for (codex, claude) in [(true, true), (true, false), (false, true), (false, false)] {
        let f = Fixture::folder();
        example_limits(&f, codex.then_some(now), claude.then_some(now), now + HOUR, now);
        let mut usage = open(&f, now);
        let rows = frame(&mut usage, 140, 5);
        assert!(rows.iter().all(|row| row.chars().nth(69) == Some('│')));
        assert!(!rows[0].split_once('│').unwrap().0.trim().is_empty());
        assert!(!rows[0].split_once('│').unwrap().1.trim().is_empty());
        assert!(rows[2].contains("██ Codex") && rows[2].contains("██ Claude"));
        let rows = frame(&mut usage, 36, 12);
        assert_eq!(rows[5], "─".repeat(36));
        assert!(!rows[0].is_empty() && !rows[6].is_empty());
        assert!(rows[2].contains("██ Codex") && rows[8].contains("██ Claude"));
    }
}

#[test]
fn a_tiny_frame_keeps_an_intact_primary_figure() {
    let f = with_limits();
    let mut usage = open(&f, NOW);
    assert_eq!(frame(&mut usage, 6, 1), ["Cx 23%"]);
    assert_eq!(frame(&mut usage, 12, 2), ["Codex 23%", "Claude 23%"]);
    for w in 0..=48 {
        for h in 0..=8 {
            frame(&mut usage, w, h);
        }
    }
}

#[test]
fn every_usage_help_row_fits_a_28_column_pane() {
    for row in help_rows() {
        let width = row.iter().map(|(text, _)| str_width(text)).sum::<usize>();
        assert!(width <= 26, "{width} columns: {:?}", row.iter().map(|(text, _)| text).collect::<Vec<_>>());
    }
}

#[test]
fn stacked_marks_are_the_same_size_though_one_half_is_a_row_taller() {
    let now = local_noon();
    let f = Fixture::folder();
    example_limits(&f, Some(now), Some(now), now + HOUR, now);
    let mut usage = open(&f, now);
    // Twelve rows less the rule leave five above and six below.
    let mut screen = Screen::sized(36, 12);
    screen.set_pictures(true);
    usage.draw(&mut screen);
    let marks: Vec<_> = super::draw::mark_placements(&screen).collect();
    assert_eq!(marks.len(), 2);
    assert_eq!((marks[0].2, marks[0].3), (marks[1].2, marks[1].3), "{marks:?}");
}

#[test]
fn activity_follows_the_hint_and_never_sits_under_a_window_like_its_caption() {
    let now = local_noon();
    let f = Fixture::folder();
    write(&f, CLAUDE, &assistant(now - MINUTE, 12_000, 900, 100), now);
    // Five rows leave none to spare under the captions.
    let rows = frame(&mut open(&f, now), 140, 5);
    let claude = rows[4].split_once('│').unwrap().1.trim();
    assert_eq!(claude, "turn on in milo setup · 13k tokens today");
    // With a row to spare it has that row to itself.
    let rows = frame(&mut open(&f, now), 140, 6);
    let below: Vec<&str> = rows[4..].iter().map(|row| row.split_once('│').unwrap().1.trim()).collect();
    assert_eq!(below, ["turn on in milo setup", "13k tokens today"]);
}

#[test]
fn an_old_reading_says_so_once_and_each_window_keeps_its_reset_time() {
    let now = local_noon();
    let f = Fixture::folder();
    let then = now - 6 * HOUR + 5 * MINUTE;
    example_limits(&f, Some(then), None, now + 2 * HOUR, now);
    let mut usage = open(&f, now);
    for (w, h, age) in [(140, 5, "5 h 55 m ago"), (36, 12, "6 h ago")] {
        let codex = frame(&mut usage, w, h)[..5].join("\n");
        assert_eq!(codex.matches("ago").count(), 1, "{codex}");
        assert!(codex.contains(age), "{codex}");
        assert!(codex.contains("in 2 h") && codex.contains("in 2 d"), "{codex}");
    }
}
