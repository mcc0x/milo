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
    assert!(today(&mut usage).contains("tokens today"));
    fs::remove_file(f.root.join(CLAUDE)).unwrap();
    if !linked(symlink_file(f.root.join("elsewhere/other.jsonl"), f.root.join(CLAUDE))) {
        return;
    }
    usage.check();
    assert!(!today(&mut usage).contains("tokens today"));
}

#[test]
fn what_needs_a_network_or_a_menu_bar_stays_unsupported() {
    let mut hidden = UsageTools::default();
    for tool in UsageTool::ALL {
        hidden[tool] = Show::Hide;
    }
    assert!(doctor(hidden, UsageAccounts::default()).is_empty());
    let error = menu_stream().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("not supported on Windows yet"));
    assert!(SystemTime::now() > UNIX_EPOCH);
}

#[test]
fn doctor_reads_codex_and_claude_files_and_points_to_the_status_line_switch() {
    let f = Fixture::folder();
    let now = local_noon();
    write(&f, CODEX, &codex(now - MINUTE, 23, now + DAY), now - MINUTE);
    write(&f, CLAUDE, &assistant(now - HOUR, 21_000, 25_000, 2000), now);
    let usage =
        Usage::from_dirs(Some(f.root.join("codex")), Some(f.root.join("claude")), Some(f.root.join("state")), now);
    let lines = usage.doctor(None, None, Duration::from_secs(30));
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].starts_with("Codex limits: ") && !lines[0].contains("no local report"), "{lines:?}");
    assert_eq!(
        lines[1], "Claude Code limits: no local report; turn on Show Claude Code limits in milo web",
        "{lines:?}"
    );
    assert!(lines.iter().all(|line| !line.contains("milo setup")), "{lines:?}");
    assert!(!f.root.join("state").exists(), "doctor wrote into the state folder");
}

/// OMP's and OpenCode's databases, made by Windows' own SQLite as those tools make them, and read only
/// once they are closed.
mod databases {
    use super::super::opencode::{self, Activity, Cost, Report, Store, Tokens};
    use super::super::sqlite::Writer;
    use super::*;

    const WAIT: Duration = Duration::from_secs(30);
    const OMP_SCHEMA: &str = "
        CREATE TABLE cache(key TEXT PRIMARY KEY, value TEXT, expires_at INTEGER);
        CREATE TABLE usage_history(id INTEGER PRIMARY KEY, recorded_at INTEGER, provider TEXT,
            account_key TEXT NOT NULL, email TEXT, account_id TEXT, limit_id TEXT, label TEXT, window_label TEXT,
            used_fraction REAL, status TEXT, resets_at INTEGER);
    ";

    fn omp_report(writer: &Writer, fraction: &str) {
        let (ms, reset) = (NOW * 1000, (NOW + DAY) * 1000);
        let week = format!(
            r#"{{"id":"openai-codex:primary","amount":{{"usedFraction":{fraction}}},"scope":{{"tier":null}},"window":{{"durationMs":604800000,"resetsAt":{reset}}}}}"#
        );
        writer.exec(&format!(
            r#"INSERT OR REPLACE INTO cache VALUES ('usage_cache:report:openai-codex:default:oauth|account:codex',
               '{{"value":{{"provider":"openai-codex","fetchedAt":{ms},"limits":[{week}]}}}}',0);"#
        ));
    }

    /// The pane is woken through the same descriptor its event loop waits on.
    fn settle(usage: &mut Usage) -> External {
        let fd = usage.external_fd().expect("no database read is running");
        let mut ready = [crate::sys::pollfd { fd, events: crate::sys::POLLIN, revents: 0 }];
        assert_eq!(crate::sys::poll(&mut ready, 30_000), 1, "the database read did not wake the pane");
        let external = usage.check_external();
        assert!(usage.external_fd().is_none());
        external
    }

    fn finish(usage: &mut Usage) {
        assert!(matches!(settle(usage), External::Redraw));
    }

    /// While OMP has its database open, the limits read when it was last closed stay on screen.
    #[test]
    fn omp_limits_reach_the_pane_while_omp_is_closed() {
        let f = Fixture::folder();
        let path = f.root.join("agent.db");
        let writer = Writer::open(&path);
        writer.exec(&format!("PRAGMA journal_mode=WAL;{OMP_SCHEMA}"));
        omp_report(&writer, "0.25");
        drop(writer);
        let mut tools = UsageTools::default();
        tools[UsageTool::Codex] = Show::Always;
        tools[UsageTool::Claude] = Show::Always;
        let mut usage =
            Usage::from_dirs(Some(f.root.join("codex")), Some(f.root.join("claude")), Some(f.root.join("state")), NOW)
                .showing(tools);
        usage.omp = omp::poll(Some(path.clone()), WAIT);

        assert!(usage.check());
        assert_eq!(frame(&mut usage, 22, 2), ["Codex · checking", "Claude · checking"]);
        finish(&mut usage);
        assert_eq!(frame(&mut usage, 22, 2), ["Codex week · 75% left", "Claude no saved limits"]);
        assert_eq!(usage.notice(), None);

        let writer = Writer::open(&path);
        omp_report(&writer, "0.65");
        assert_eq!(usage.key(Key::Char('r')), After::Stay);
        settle(&mut usage);
        assert_eq!(frame(&mut usage, 22, 2), ["Codex week · 75% left", "Claude no saved limits"]);
        assert_eq!(usage.notice(), Some("On Windows, OMP and OpenCode usage is read only while they are closed."));

        drop(writer);
        assert_eq!(usage.key(Key::Char('r')), After::Stay);
        finish(&mut usage);
        assert_eq!(frame(&mut usage, 22, 2), ["Codex week · 35% left", "Claude no saved limits"]);
        assert_eq!(usage.notice(), None);
    }

    fn opencode_shown() -> UsageTools {
        let mut tools = UsageTools::default();
        tools[UsageTool::OpenCode] = Show::Always;
        tools
    }

    #[test]
    fn doctor_says_when_a_database_is_read_only_while_its_tool_is_closed() {
        let f = Fixture::folder();
        let omp = f.root.join("agent.db");
        let omp_writer = Writer::open(&omp);
        omp_writer.exec(&format!("PRAGMA journal_mode=WAL;{OMP_SCHEMA}"));
        omp_report(&omp_writer, "0.25");
        let opencode = f.root.join("opencode.db");
        let opencode_writer = Writer::open(&opencode);
        let schema = include_str!("tests/opencode/v2.3.2.sql");
        opencode_writer.exec(&format!("PRAGMA journal_mode=WAL;BEGIN;\n{schema}\nCOMMIT;"));

        let look = || {
            let usage = Usage::from_dirs(None, None, None, NOW).showing(opencode_shown());
            usage.doctor(Some(omp.clone()), Some(opencode.clone()), WAIT)
        };
        let lines = look();
        for (tool, path) in [("OMP", &omp), ("OpenCode", &opencode)] {
            let note = format!("{tool} database: in use, so it is read only while {tool} is closed ({path:?})");
            assert_eq!(lines.iter().filter(|line| **line == note).count(), 1, "{lines:?}");
        }
        assert!(lines.iter().all(|line| !line.contains("not read on Windows")), "{lines:?}");

        drop(omp_writer);
        drop(opencode_writer);
        let lines = look();
        assert!(lines.iter().all(|line| !line.contains("database:")), "{lines:?}");
        assert!(lines.iter().any(|line| line.starts_with("OpenCode activity: ")), "{lines:?}");
    }

    #[test]
    fn doctor_says_a_database_over_64_mib_is_not_read_on_windows_yet() {
        let f = Fixture::folder();
        let path = f.root.join("opencode.db");
        let writer = Writer::open(&path);
        writer.exec("CREATE TABLE t(x);");
        drop(writer);
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len((64 << 20) + 1).unwrap();
        let usage = Usage::from_dirs(None, None, None, NOW).showing(opencode_shown());
        let lines = usage.doctor(None, Some(path.clone()), WAIT);
        let note = format!("OpenCode database: over 64 MiB, so it is not read on Windows yet ({path:?})");
        assert_eq!(lines.iter().filter(|line| **line == note).count(), 1, "{lines:?}");
        assert!(lines.iter().any(|line| line.starts_with("OpenCode activity: unknown")), "{lines:?}");
    }

    #[test]
    fn opencode_totals_for_today_are_read_from_a_checked_release() {
        let f = Fixture::folder();
        let path = f.root.join("opencode.db");
        let writer = Writer::open(&path);
        let schema = include_str!("tests/opencode/v2.3.2.sql");
        writer.exec(&format!(
            "PRAGMA journal_mode=WAL;BEGIN;\n{schema}\n\
             INSERT INTO project (id,worktree,time_created,time_updated,sandboxes) VALUES ('global','/',0,0,'[]');COMMIT;"
        ));
        let now = local_noon();
        let since = Today::at(now, when::local_day(now)).since;
        let session = since * 1000;
        writer.exec(&format!(
            "INSERT INTO session_v2 (id,project_id,fork_session_id,slug,directory,version,time_created,time_updated)
             VALUES ('ses_a','global',NULL,'ses_a','/work','2.3.2',{session},{session});"
        ));
        for (n, at, input) in [(1, since - HOUR, 70_000), (2, now - MINUTE, 1000)] {
            let ms = at * 1000 + 494;
            let data = format!(
                r#"{{"time":{{"created":{ms},"completed":{ms}}},"agent":"build","model":{{"id":"fake-1","providerID":"fake"}},"content":[{{"type":"text","text":"SYNTHETIC-REPLY"}}],"finish":"stop","cost":0.00357,"tokens":{{"input":{input},"output":30,"reasoning":4,"cache":{{"read":200,"write":0}}}}}}"#
            );
            writer.exec(&format!(
                "INSERT INTO session_message (id,session_id,type,seq,time_created,time_updated,data)
                 VALUES ('msg_{n}','ses_a','assistant',{n},{ms},{ms},'{data}');"
            ));
        }
        let expected = Activity {
            since,
            store: Store::V2 { legacy: false },
            steps: 1,
            tokens: Tokens { input: 1000, output: 30, reasoning: 4, cache_read: 200, cache_write: 0 },
            cost: Some(Cost { usd: 0.00357, complete: true }),
            last: Some(now - MINUTE),
        };
        assert_eq!(opencode::read(&path, now, WAIT), Report::Unreadable, "read while OpenCode had it open");
        drop(writer);
        assert_eq!(opencode::read(&path, now, WAIT), Report::Today(expected));
    }
}

#[test]
fn the_status_line_gives_up_on_input_left_open_and_reads_input_that_ends() {
    let (reader, mut writer) = io::pipe().unwrap();
    io::Write::write_all(&mut writer, b"{\"model\":").unwrap();
    let started = std::time::Instant::now();
    let mut output = Vec::new();
    assert_eq!(status::run_fd(reader, &mut output, None, NOW, Duration::from_millis(200)), 0);
    assert_eq!(output, b"\n");
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(writer);

    let (reader, mut writer) = io::pipe().unwrap();
    let input = br#"{"rate_limits":{"five_hour":{"used_percentage":23},"seven_day":{"used_percentage":61}}}"#;
    io::Write::write_all(&mut writer, input).unwrap();
    drop(writer);
    let mut output = Vec::new();
    assert_eq!(status::run_fd(reader, &mut output, None, NOW, Duration::from_secs(5)), 0);
    assert_eq!(String::from_utf8(output).unwrap(), "5h 77% · week 39% left\n");

    let f = Fixture::folder();
    let saved = f.root.join("state").join("claude-limits.json");
    assert_eq!(status::run(input, io::sink(), Some(&saved), NOW), 0);
    let text = fs::read_to_string(&saved).unwrap();
    assert!(text.contains(r#""seven_day":{"used_percentage":61"#), "{text}");
}

/// A provider account asked through the curl Windows ships, against a loopback stand-in for the
/// provider. The key comes from the environment, or from a settings file only while it is private.
#[test]
fn a_provider_account_is_asked_through_windows_curl_with_a_key_from_the_environment_or_a_private_file() {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::TcpListener;

    use super::accounts::{Accounts, Failure, Sources, Status};
    use crate::preferences::UsageAccount;

    const KEY: &str = "sk-milo-windows-test-key-1";
    let f = Fixture::folder();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut heads = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            while reader.read_line(&mut head).is_ok_and(|read| read > 0) && !head.ends_with("\r\n\r\n") {}
            let body = include_str!("tests/accounts/deepseek-balance.json");
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 Canned\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            heads.push(head);
        }
        heads
    });
    let settings = f.root.join("usage-accounts.conf");
    let ask = |environment: &[(&'static str, &str)], cache: &str| {
        let sources = Sources::test(environment, settings.clone(), f.root.join(cache), port);
        let mut choices = UsageAccounts::default();
        choices[UsageAccount::DeepSeek] = Show::Always;
        let mut usage = Usage::from_dirs(None, None, Some(f.root.join("state")), NOW);
        usage.accounts = Accounts::with(sources, choices);
        usage.accounts.settle(NOW, false);
        let status = usage.accounts.iter().next().unwrap().status(NOW);
        (status, frame(&mut usage, 60, 1)[0].trim().to_owned())
    };

    let (_, row) = ask(&[("MILO_DEEPSEEK_API_KEY", KEY)], "first");
    assert_eq!(row, "DeepSeek · ¥110.00 balance · ¥10.00 granted");

    // A file written the ordinary way takes its folder's access list, so it is not read.
    fs::write(&settings, format!("MILO_DEEPSEEK_API_KEY = {KEY}\n")).unwrap();
    assert_eq!(ask(&[], "second").0, Status::Failed(Failure::OpenFile));
    fs::remove_file(&settings).unwrap();
    let mut private = crate::sys::private_file(&settings, crate::sys::Existing::Refuse).unwrap();
    private.write_all(format!("MILO_DEEPSEEK_API_KEY = {KEY}\n").as_bytes()).unwrap();
    drop(private);
    let (_, row) = ask(&[], "third");
    assert_eq!(row, "DeepSeek · ¥110.00 balance · ¥10.00 granted");

    let heads = server.join().unwrap();
    for head in &heads {
        assert!(head.starts_with("GET /deepseek/user/balance HTTP/1.1\r\n"), "{head}");
        assert!(head.contains(&format!("Authorization: Bearer {KEY}\r\n")), "{head}");
    }
    let saved =
        fs::read_to_string(fs::read_dir(f.root.join("third")).unwrap().next().unwrap().unwrap().path()).unwrap();
    assert!(!saved.contains(KEY), "a key was saved: {saved}");
}
