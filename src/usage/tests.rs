use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fixture::Fixture;
use crate::width::str_width;

use super::time::HOUR;
use super::*;

const NOW: i64 = 1_795_100_000;
const CODEX: &str = "codex/2026/11/18/rollout-a.jsonl";
const OTHER_CODEX: &str = "codex/2026/11/18/rollout-z.jsonl";
const CLAUDE: &str = "claude/project/session.jsonl";
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
        codex(NOW + 90, 62, reset, "null"),
        codex(NOW + DAY, 95, reset, "null"),
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
    usage.fixed_now = Some(NOW + 5);
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
    let add_id = |line: String| line.replace("\"usage\":", "\"id\":\"example-message\",\"usage\":");
    let records = [add_id(assistant(now - 20, 100, 1800, 100)), add_id(assistant(now - 10, 100, 1800, 100))].concat();
    write(&f, CLAUDE, &records, now);
    assert!(caption(&mut open(&f, now)).contains("2k tokens today"));
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
    assert_eq!(saved[0].unwrap().observed, NOW);
    assert_eq!(saved[1].unwrap().resets, Some(1_795_219_200));
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

#[test]
fn json_reader_validates_nested_values_and_escaped_keys() {
    let json = br#"{"other": [null, true, false, -2.5e+2, {"escaped":"\t\uD83D\uDE00"}], "\u006bey":"caf\u00e9"}"#;
    assert!(Value::parse(json).unwrap().fields(["key"])[0].unwrap().is("café"));
    for input in [b"01".as_slice(), b"[1,]", b"{\"a\":true,}", br#"{"a":"\uD83D"}"#, b"1e-", b"-", b"+1", b"{} extra"] {
        assert!(Value::parse(input).is_none());
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
        assert_eq!(screen.picture_placements().collect::<Vec<_>>(), [(2, 0, cols, 4), (72, 0, cols, 4)]);
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
    let marks: Vec<_> = screen.picture_placements().collect();
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
