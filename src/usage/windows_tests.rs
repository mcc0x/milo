//! Usage on native Windows, read from fixture session files in a scratch folder: Codex limits,
//! Claude Code tokens, a link or junction that is never followed, and what stays unsupported.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::windows::fs::symlink_file;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::fixture::Fixture;
use crate::width::str_width;

use super::*;

const NOW: i64 = 1_795_100_000;
const HOUR: i64 = 3600;
const MINUTE: i64 = 60;
const DAY: i64 = 24 * HOUR;
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
const CODEX: &str = "codex/2026/11/18/rollout-a.jsonl";
const CLAUDE: &str = "claude/project/session.jsonl";
const AGENT: &str = "claude/project/session/subagents/agent-good.jsonl";

fn iso(seconds: i64) -> String {
    let (days, rest) = (seconds.div_euclid(DAY), seconds.rem_euclid(DAY));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 { shifted + 3 } else { shifted - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}", rest / HOUR, rest % HOUR / MINUTE, rest % MINUTE)
}

fn write(f: &Fixture, path: &str, data: &str, now: i64) {
    f.write(path, data);
    let modified = UNIX_EPOCH + Duration::from_secs(u64::try_from(now).unwrap());
    OpenOptions::new().write(true).open(f.root.join(path)).unwrap().set_modified(modified).unwrap();
}

fn open(f: &Fixture, now: i64) -> Usage {
    let mut tools = UsageTools::default();
    tools[UsageTool::Codex] = Show::Always;
    tools[UsageTool::Claude] = Show::Always;
    let mut usage =
        Usage::from_dirs(Some(f.root.join("codex")), Some(f.root.join("claude")), Some(f.root.join("state")), now)
            .showing(tools);
    usage.check();
    usage
}

fn frame(usage: &mut Usage, width: usize, height: usize) -> Vec<String> {
    let mut screen = Screen::sized(width, height);
    usage.draw(&mut screen);
    let rows = screen.text();
    assert_eq!(rows.len(), height);
    assert!(rows.iter().all(|row| str_width(row) <= width));
    rows
}

fn codex(at: i64, percent: u32, reset: i64) -> String {
    format!(
        "{{\"timestamp\":\"{}Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
         \"rate_limits\":{{\"primary\":{{\"used_percent\":{percent},\"window_minutes\":10080,\"resets_at\":{reset}}},\
         \"secondary\":null}}}}}}\n",
        iso(at)
    )
}

fn assistant(at: i64, input: u64, output: u64, writes: u64) -> String {
    format!(
        "{{\"timestamp\":\"{}Z\",\"type\":\"assistant\",\"message\":{{\"usage\":{{\
         \"input_tokens\":{input},\"output_tokens\":{output},\"cache_creation_input_tokens\":{writes},\
         \"cache_read_input_tokens\":9010011}},\"model\":\"Claude Test\"}}}}\n",
        iso(at)
    )
}

fn today(usage: &mut Usage) -> String {
    frame(usage, 140, 4).pop().unwrap()
}

fn local_noon() -> i64 {
    Today::at(NOW, when::local_day(NOW)).since + 12 * HOUR
}

fn plain_form(path: &Path) -> PathBuf {
    let text = path.to_str().unwrap();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text).replace('/', "\\"))
}

fn junction(link: &Path, target: &Path) {
    let status = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(plain_form(link))
        .arg(plain_form(target))
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");
}

fn linked(made: io::Result<()>) -> bool {
    match made {
        Ok(()) => true,
        Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => false,
        Err(e) => panic!("cannot create a link: {e}"),
    }
}

#[test]
fn the_calendar_helper_writes_utc_timestamps() {
    assert_eq!(iso(0), "1970-01-01T00:00:00");
    assert_eq!(iso(951_782_400 + 86_399), "2000-02-29T23:59:59");
    assert_eq!(iso(NOW), "2026-11-19T14:53:20");
}

#[test]
fn codex_limits_are_read_from_its_session_folders() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 23, NOW + 2 * HOUR + 10 * MINUTE), NOW);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[0].contains("week") && rows[1].contains("77%") && rows[2].contains("left · resets in 2 h 10 m"));
}

#[test]
fn the_newest_codex_folder_is_read_first_and_only_rollouts_count() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW - MINUTE, 40, NOW + DAY), NOW - MINUTE);
    write(&f, "codex/2026/11/19/rollout-b.jsonl", &codex(NOW, 10, NOW + DAY), NOW);
    let decoy = codex(NOW, 99, NOW + DAY);
    for path in [
        "codex/2026/11/19/notes.jsonl",
        "codex/2026/11/19/rollout-c.jsonl.zst",
        "codex/rollout-d.jsonl",
        "codex/2026/rollout-e.jsonl",
    ] {
        write(&f, path, &decoy, NOW);
    }
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].contains("90%"), "{rows:?}");
}

#[test]
fn a_rollout_that_grows_is_read_again() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW - MINUTE, 23, NOW + DAY), NOW - MINUTE);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 140, 3)[1].contains("77%"));
    let grown = format!("{}{}", codex(NOW - MINUTE, 23, NOW + DAY), codex(NOW, 50, NOW + DAY));
    write(&f, CODEX, &grown, NOW);
    usage.check();
    assert!(frame(&mut usage, 140, 3)[1].contains("50%"));
}

#[test]
fn claude_counts_today_across_transcripts_and_their_subagents() {
    let f = Fixture::folder();
    let now = local_noon();
    write(&f, CLAUDE, &assistant(now - HOUR, 21_000, 25_000, 2000), now);
    write(&f, AGENT, &assistant(now - HOUR, 2000, 0, 0), now);
    let decoy = assistant(now, 9000, 0, 0);
    for path in [
        "claude/project/session/agent-wrong-level.jsonl",
        "claude/project/session/subagents/not-ours.jsonl",
        "claude/project/session/subagents/agent-compressed.jsonl.zst",
        "claude/project/session/subagents/nested/agent-wrong-level.jsonl",
        "claude/project/session/other/agent-wrong-folder.jsonl",
    ] {
        write(&f, path, &decoy, now);
    }
    assert!(today(&mut open(&f, now)).contains("50k tokens today"));
}

#[test]
fn claude_ignores_transcripts_from_before_today() {
    let f = Fixture::folder();
    let now = local_noon();
    write(&f, CLAUDE, &assistant(now - 2 * DAY, 21_000, 25_000, 2000), now - 2 * DAY);
    assert!(!today(&mut open(&f, now)).contains("tokens today"));
}

#[test]
fn a_junction_or_link_in_the_session_tree_is_not_followed() {
    let f = Fixture::folder();
    let now = local_noon();
    write(&f, "elsewhere/rollout-linked.jsonl", &codex(now, 98, now + DAY), now);
    write(&f, "elsewhere/claude.jsonl", &assistant(now, 3000, 400, 100), now);
    fs::create_dir_all(f.root.join("codex/2026/11")).unwrap();
    junction(&f.root.join("codex/2026/11/18"), &f.root.join("elsewhere"));
    fs::create_dir_all(f.root.join("claude/project")).unwrap();
    junction(&f.root.join("claude/linked"), &f.root.join("elsewhere"));
    if linked(symlink_file(f.root.join("elsewhere/claude.jsonl"), f.root.join(CLAUDE))) {
        fs::create_dir_all(f.root.join("codex/2026/11/19")).unwrap();
        symlink_file(
            f.root.join("elsewhere/rollout-linked.jsonl"),
            f.root.join("codex/2026/11/19/rollout-linked.jsonl"),
        )
        .unwrap();
    }
    let mut usage = open(&f, now);
    let rows = frame(&mut usage, 140, 4);
    assert!(!rows.join("\n").contains("98%") && !rows.join("\n").contains("2%"), "{rows:?}");
    assert!(!rows.join("\n").contains("tokens today"), "{rows:?}");
}

#[test]
fn a_transcript_replaced_by_a_link_after_discovery_is_no_longer_read() {
    let f = Fixture::folder();
    let now = local_noon();
    write(&f, CLAUDE, &assistant(now - HOUR, 1000, 0, 0), now);
    write(&f, "elsewhere/other.jsonl", &assistant(now - HOUR, 9000, 0, 0), now);
    let mut usage = open(&f, now);
    assert!(today(&mut usage).contains("1.0k tokens today"));
    fs::remove_file(f.root.join(CLAUDE)).unwrap();
    if !linked(symlink_file(f.root.join("elsewhere/other.jsonl"), f.root.join(CLAUDE))) {
        return;
    }
    usage.check();
    assert!(!today(&mut usage).contains("tokens today"));
}

#[test]
fn what_needs_setup_the_status_line_or_a_network_stays_unsupported() {
    let lines = doctor(UsageTools::default(), UsageAccounts::default());
    assert_eq!(lines, ["Usage: not supported on Windows yet"]);
    let error = menu_stream().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("not supported on Windows yet"));
    assert_eq!(claude_status(), 0);
    assert!(SystemTime::now() > UNIX_EPOCH);
}
