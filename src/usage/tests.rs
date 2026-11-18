use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fixture::Fixture;
use crate::width::str_width;

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

fn codex_at(timestamp: &str, percent: u32, reset: i64, secondary: &str) -> String {
    format!(
        "{{\"timestamp\":\"{timestamp}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
        \"rate_limits\":{{\"primary\":{{\"used_percent\":{percent},\"window_minutes\":10080,\"resets_at\":{reset}}},\
        \"secondary\":{secondary}}}}}}}\n"
    )
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
    let rows = frame(&mut open(&f, NOW), 85, 2);
    assert!(rows[0].contains("week") && rows[0].contains("23%") && rows[0].contains("resets in 2 h 10 m"));
    assert!(rows[0].contains('━') && rows[0].contains('┄'));
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
fn codex_shows_reset_when_the_time_passes_without_an_append() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 77, NOW + 5, "null"), NOW);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 85, 2)[0].contains("77%"));
    assert_eq!(usage.next_frame(), Some(Duration::from_secs(5)));
    usage.fixed_now = Some(NOW + 5);
    let rows = frame(&mut usage, 85, 2);
    assert!(rows[0].contains("week reset") && !rows[0].contains("77%"));
}

#[test]
fn an_old_codex_snapshot_says_how_old_it_is() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW - 2 * HOUR, 33, NOW + DAY, "null"), NOW);
    assert!(frame(&mut open(&f, NOW), 85, 2)[0].contains("as of 2 h ago"));
}

#[test]
fn codex_ignores_null_secondary_and_shows_a_present_one() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 23, NOW + DAY, "null"), NOW);
    assert_eq!(frame(&mut open(&f, NOW), 100, 2)[0].matches('%').count(), 1);
    let secondary = format!(r#"{{"used_percent":89,"window_minutes":300,"resets_at":{}}}"#, NOW + HOUR);
    write(&f, CODEX, &codex(NOW, 23, NOW + DAY, &secondary), NOW);
    let rows = frame(&mut open(&f, NOW), 100, 2);
    assert!(rows[0].contains("5 h") && rows[0].contains("89%") && rows[0].contains("23%"));
}

#[test]
fn compressed_unrelated_and_old_files_are_ignored() {
    let f = Fixture::folder();
    let data = codex(NOW, 23, NOW + DAY, "null");
    write(&f, &format!("{CODEX}.zst"), &data, NOW);
    write(&f, "codex/2026/11/18/not-a-rollout.jsonl", &data, NOW);
    write(&f, "codex/rollout-b.jsonl", &data, NOW);
    write(&f, CODEX, &data, NOW - WEEK - 1);
    write(&f, "claude/project/subagents/session.jsonl", &assistant(NOW, 600, 900, 100), NOW);
    write(&f, CLAUDE, &assistant(NOW, 600, 900, 100), NOW - 2 * DAY);
    let rows = frame(&mut open(&f, NOW), 100, 2);
    assert_eq!(rows, ["no Codex use found", "no Claude Code use found"]);
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
    assert_eq!(frame(&mut usage, 100, 2), ["no Codex use found", "no Claude Code use found"]);
    assert_eq!(usage.bytes_read, 0);
}

#[test]
fn claude_activity_adds_input_output_and_cache_writes_in_each_period() {
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
    let rows = frame(&mut open(&f, now), 100, 2);
    assert!(rows[1].contains("activity") && rows[1].contains("5 h 49k tokens") && rows[1].contains("today 48k tokens"));
    assert!(!rows[1].contains('%'));
}

#[test]
fn claude_today_includes_earlier_activity_after_five_hours_have_passed() {
    let f = Fixture::folder();
    let midnight = Today::at(NOW, when::local_day(NOW)).since;
    let now = midnight + 12 * HOUR;
    let records = [assistant(now - 6 * HOUR, 1_000_000, 100_000, 100_000), assistant(now - HOUR, 55, 45, 0)].concat();
    write(&f, CLAUDE, &records, now);
    let rows = frame(&mut open(&f, now), 100, 2);
    assert!(rows[1].contains("5 h 100 tokens") && rows[1].contains("today 1.2M tokens"));
}

#[test]
fn multiple_claude_blocks_for_the_same_message_are_not_counted_twice() {
    let f = Fixture::folder();
    let add_id = |line: String| line.replace("\"usage\":", "\"id\":\"example-message\",\"usage\":");
    let records = [add_id(assistant(NOW - 20, 100, 1800, 100)), add_id(assistant(NOW - 10, 100, 1800, 100))].concat();
    write(&f, CLAUDE, &records, NOW);
    let rows = frame(&mut open(&f, NOW), 100, 2);
    assert!(rows[1].contains("5 h 2k tokens"));
}

#[test]
fn an_append_reads_only_the_new_bytes_and_keeps_previous_activity() {
    let f = Fixture::folder();
    let first = assistant(NOW - 20, 100, 1800, 100);
    write(&f, CLAUDE, &first, NOW - 20);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 100, 2)[1].contains("5 h 2k tokens"));
    let path = f.root.join(CLAUDE);
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all(&vec![b'x'; first.len()]).unwrap();
    let second = assistant(NOW - 10, 3000, 800, 200);
    file.write_all(second.as_bytes()).unwrap();
    file.set_modified(mtime(NOW)).unwrap();
    assert!(usage.check());
    assert_eq!(usage.bytes_read, u64::try_from(second.len()).unwrap());
    assert!(frame(&mut usage, 100, 2)[1].contains("5 h 6k tokens"));
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 0);
}

#[test]
fn claude_truncation_and_replacement_discard_previous_samples() {
    let f = Fixture::folder();
    let first = assistant(NOW - 20, 1_000_000, 1800, 100);
    write(&f, CLAUDE, &first, NOW - 20);
    let mut usage = open(&f, NOW);
    write(&f, CLAUDE, &assistant(NOW - 10, 300, 500, 200), NOW - 10);
    assert!(usage.check());
    assert!(frame(&mut usage, 100, 2)[1].contains("5 h 1k tokens"));
    write(&f, "claude/project/replacement", &assistant(NOW, 900, 900, 200), NOW - 10);
    fs::rename(f.root.join("claude/project/replacement"), f.root.join(CLAUDE)).unwrap();
    assert!(usage.check());
    assert!(frame(&mut usage, 100, 2)[1].contains("5 h 2k tokens"));
}

#[test]
fn a_partial_last_record_becomes_visible_once_complete_without_double_counting() {
    let f = Fixture::folder();
    let full = assistant(NOW - 20, 100, 1800, 100);
    let split = full.len() / 2;
    write(&f, CLAUDE, &full[..split], NOW);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 100, 2)[1].contains("no Claude Code use found"));
    let mut file = OpenOptions::new().append(true).open(f.root.join(CLAUDE)).unwrap();
    file.write_all(&full.as_bytes()[split..full.len() - 1]).unwrap();
    file.set_modified(mtime(NOW)).unwrap();
    assert!(usage.check());
    assert!(frame(&mut usage, 100, 2)[1].contains("5 h 2k tokens"));
    file.write_all(b"\n").unwrap();
    file.set_modified(mtime(NOW + 1)).unwrap();
    assert!(usage.check());
    assert!(frame(&mut usage, 100, 2)[1].contains("5 h 2k tokens"));
}

#[test]
fn fresh_claude_limits_replace_activity_and_expired_windows_say_reset() {
    let f = with_limits();
    write(&f, CLAUDE, &assistant(NOW, 200, 900, 100), NOW);
    let mut usage = open(&f, NOW);
    let rows = frame(&mut usage, 100, 2);
    assert!(rows[1].contains("5 h") && rows[1].contains("23%") && rows[1].contains("week") && rows[1].contains("61%"));
    assert!(!rows[1].contains("activity"));
    usage.fixed_now = Some(1_795_110_000);
    let rows = frame(&mut usage, 100, 2);
    assert!(rows[1].contains("5 h reset") && !rows[1].contains("23%"));
}

#[test]
fn stale_claude_limits_fall_back_to_activity() {
    let f = Fixture::folder();
    let path = f.root.join(STATE).join(SAVED_LIMITS);
    assert_eq!(status::run(STATUS, Vec::new(), Some(&path), NOW - CLAUDE_FRESH), 0);
    write(&f, CLAUDE, &assistant(NOW - 60, 200, 900, 100), NOW);
    let rows = frame(&mut open(&f, NOW), 100, 2);
    assert!(rows[1].contains("activity") && rows[1].contains("5 h 1.2k tokens") && !rows[1].contains('%'));
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
    assert_eq!(saved.at.seconds, NOW);
    assert_eq!(saved.windows[1].unwrap().resets, Some(1_795_219_200));
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
    let huge = format!("{{\"usage\":\"token_count\",\"ignored\":\"{}\"}}\n", "a".repeat(2 * 1024 * 1024 + 1));
    let deep = format!("{{\"usage\":\"token_count\",\"ignored\":{}0{}}}\n", "[".repeat(1000), "]".repeat(1000));
    let garbage = "{\"usage\":\"token_count\",\"broken\"}\n";
    let codex_data = [codex(NOW, 23, NOW + DAY, "null"), huge.clone(), deep.clone(), garbage.into()].concat();
    let claude_data = [huge, deep, garbage.into(), assistant(NOW, 200, 900, 100)].concat();
    write(&f, CODEX, &codex_data, NOW);
    write(&f, CLAUDE, &claude_data, NOW);
    let rows = frame(&mut open(&f, NOW), 100, 2);
    assert!(rows[0].contains("23%") && rows[1].contains("5 h 1.2k tokens"));
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
        let _ = Snapshot::codex(&bytes, NOW);
        let _ = Sample::parse(&bytes);
        assert_eq!(status::run(bytes.as_slice(), Vec::new(), None, NOW), 0);
    }
}

#[test]
fn one_wide_row_places_both_tools_side_by_side() {
    let f = with_limits();
    let rows = frame(&mut open(&f, NOW), 160, 1);
    assert!(rows[0].contains("Codex") && rows[0].contains("Claude Code") && rows[0].contains("61%"));
    assert!(rows[0].contains('│'));
}

#[test]
fn two_rows_give_one_to_each_tool() {
    let f = with_limits();
    let rows = frame(&mut open(&f, NOW), 85, 2);
    assert!(rows[0].starts_with("Codex") && rows[0].contains("23%"));
    assert!(rows[1].starts_with("Claude Code") && rows[1].contains("61%"));
}

#[test]
fn a_narrow_column_stacks_and_widens_the_bars() {
    let f = with_limits();
    let rows = frame(&mut open(&f, NOW), 20, 8);
    assert_eq!(rows[0], "Codex");
    assert!(rows.iter().any(|row| row == "Claude Code"));
    assert!(rows.iter().any(|row| row.contains("61%")));
    assert!(rows.iter().any(|row| row.matches(['━', '┄']).count() > 6));
}

#[test]
fn a_tiny_frame_keeps_an_intact_primary_figure() {
    let f = with_limits();
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 5, 1)[0].contains("23%"));
    assert!(frame(&mut usage, 8, 2).iter().all(|row| row.contains("23%")));
    for w in 1..48 {
        for h in 1..8 {
            frame(&mut usage, w, h);
        }
    }
}
