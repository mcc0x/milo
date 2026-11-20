//! Which tools the pane gives a place to, and how the frame closes up around the rest.

use super::super::opencode::{Activity, Cost, Report, Store, Tokens};
use super::*;

const AUTO: Show = Show::Auto;
const ALWAYS: Show = Show::Always;
const HIDE: Show = Show::Hide;

fn codex_limits(f: &Fixture, now: i64) {
    let primary = codex_window(27, 300, now + 2 * HOUR);
    let secondary = codex_window(74, 10_080, now + 2 * DAY);
    write(f, CODEX, &codex_snapshot(&format!("{}Z", iso(now)), &primary, &secondary), now);
}

fn claude_limits(f: &Fixture, now: i64) {
    let saved = format!(
        r#"{{"rate_limits":{{"five_hour":{{"used_percentage":52,"resets_at":{}}},
        "seven_day":{{"used_percentage":92,"resets_at":{}}}}}}}"#,
        now + 2 * HOUR,
        now + 2 * DAY
    );
    assert_eq!(status::run(saved.as_bytes(), Vec::new(), Some(&f.root.join(STATE).join(SAVED_LIMITS)), now), 0);
}

/// Twelve steps today, as a read of OpenCode's database would report them.
fn opencode_today(usage: &mut Usage, now: i64) {
    usage.opencode.report = Report::Today(Activity {
        since: usage.today.since,
        store: Store::V2 { legacy: false },
        steps: 12,
        tokens: Tokens { input: 40_000, output: 8000, reasoning: 200, cache_read: 9_000_000, cache_write: 0 },
        cost: Some(Cost { usd: 1.37, complete: true }),
        last: Some(now - 5 * MINUTE),
    });
}

/// A pane on its defaults over whichever tools the fixture was given.
fn pane(now: i64, codex: bool, claude: bool, opencode: bool) -> (Fixture, Usage) {
    let f = Fixture::folder();
    if codex {
        codex_limits(&f, now);
    }
    if claude {
        claude_limits(&f, now);
    }
    let mut usage = automatic(&f, now);
    usage.check();
    if opencode {
        opencode_today(&mut usage, now);
    }
    (f, usage)
}

fn trimmed(usage: &mut Usage, width: usize, height: usize) -> Vec<String> {
    let mut rows: Vec<String> = frame(usage, width, height).iter().map(|row| row.trim().to_owned()).collect();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

const WIDTHS: [usize; 12] = [6, 8, 12, 17, 22, 28, 30, 36, 48, 64, 100, 140];
const HEIGHTS: [usize; 10] = [1, 2, 3, 4, 5, 8, 9, 12, 16, 24];

/// Every size class: one-row strips, the two-row rail, short and tall
/// sidebars, and wide panes, with text and with large figures.
fn every_size(usage: &mut Usage, mut look: impl FnMut(usize, usize, &[String])) {
    for large in [false, true] {
        usage.large_figures = large;
        for width in WIDTHS {
            for height in HEIGHTS {
                look(width, height, &frame(usage, width, height));
            }
        }
    }
    usage.large_figures = false;
}

fn names_opencode(rows: &[String]) -> bool {
    rows.iter().any(|row| row.contains("OpenCode") || row.split_whitespace().any(|word| word == "OC"))
}

#[test]
fn a_codex_and_claude_user_sees_nothing_of_opencode_and_no_place_kept_for_it() {
    let now = local_noon();
    let (f, mut usage) = pane(now, true, true, false);
    write(&f, CLAUDE, &assistant(now - MINUTE, 12_000, 900, 100), now);
    usage.check();
    // The frame is the one two tools have always had, cell for cell: nothing is reserved.
    let mut both = open(&f, now);
    for large in [false, true] {
        (usage.large_figures, both.large_figures) = (large, large);
        for (w, h) in WIDTHS.into_iter().flat_map(|w| HEIGHTS.map(|h| (w, h))) {
            let rows = frame(&mut usage, w, h);
            assert!(!names_opencode(&rows) && !rows.join("\n").contains("N/A"), "{w}x{h}: {rows:?}");
            assert_eq!(rows, frame(&mut both, w, h), "{w}x{h}");
        }
    }
    assert!(!usage.help().text().contains("OpenCode"));

    // Whatever else sits where its database would be is not OpenCode.
    let db = f.root.join("share/opencode/opencode.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    f.write("share/opencode/auth.json", "{}");
    for leftover in [None, Some(""), Some("not a database")] {
        if let Some(leftover) = leftover {
            fs::write(&db, leftover).unwrap();
        }
        let mut usage = automatic(&f, now).with_databases(None, Some(db.clone()), TEST_WAIT);
        usage.check();
        opencode_activity::settle(&mut usage);
        assert_eq!(usage.opencode.presence(usage.today.since), Presence::Absent, "{leftover:?}");
        assert_eq!(trimmed(&mut usage, 28, 3), ["Codex week · 26% left", "Claude week · 8% left", "Resets in 2d"]);
    }
}

#[test]
fn one_tool_with_limits_has_the_frame_to_itself() {
    let now = local_noon();
    for (codex, name, other) in [(true, "Codex", "Claude"), (false, "Claude", "Codex")] {
        let (f, mut usage) = pane(now, codex, !codex, false);
        if !codex {
            write(&f, CLAUDE, &assistant(now - MINUTE, 12_000, 900, 100), now);
            usage.check();
        }
        every_size(&mut usage, |w, h, rows| {
            let text = rows.join("\n");
            assert!(!text.contains(other) && !names_opencode(rows), "{w}x{h}: {rows:?}");
            // No rule, no divider and no dash stand in for a second tool.
            assert!(!text.contains(['│', '─', '—']), "{w}x{h}: {rows:?}");
            assert!(!text.contains("no source") && !text.contains("no limits"), "{w}x{h}: {rows:?}");
        });
        let (week, five) = if codex { ("26%", "73%") } else { ("8%", "48%") };
        assert_eq!(trimmed(&mut usage, 22, 2), [format!("{name} week · {week} left"), "Resets in 2d".into()]);
        // A taller rail lists the tool's other window instead of leaving its rows empty.
        let tall = trimmed(&mut usage, 28, 8);
        assert_eq!(tall[..3], [format!("{name} week · {week} left"), "Resets in 2d".into(), format!("5h {five} left")]);
        assert_eq!(tall.get(3).map(String::as_str), (!codex).then_some("13k tokens today"));
        // A wide pane keeps both windows together at the left rather than a pane apart.
        let wide = frame(&mut usage, 140, 5);
        assert!(wide[0].contains("5h") && wide[0].contains("week"), "{wide:?}");
        assert!(wide.iter().all(|row| str_width(row.trim_end()) <= 72), "{wide:?}");
        assert_eq!(frame(&mut usage, 140, 1)[0].trim(), format!("██ {name} 5h {five}  week {week} left"));
    }
}

#[test]
fn opencode_alone_is_useful_on_a_small_rail_and_fills_a_larger_pane() {
    let now = local_noon();
    let (_f, mut usage) = pane(now, false, false, true);
    every_size(&mut usage, |w, h, rows| {
        let text = rows.join("\n");
        assert!(!text.contains("Codex") && !text.contains("Claude"), "{w}x{h}: {rows:?}");
        assert!(!text.contains(['│', '─', '—', '%']), "{w}x{h}: {rows:?}");
        assert!(text.contains("48.2k") || w < 8 || text.contains("█ █ ▟▀▙"), "{w}x{h}: {rows:?}");
    });
    assert_eq!(trimmed(&mut usage, 22, 2), ["OpenCode 48.2k tokens", "$1.37 est. · 12 steps"]);
    assert_eq!(trimmed(&mut usage, 40, 2), ["OpenCode · 48.2k tokens today", "$1.37 est. · 12 steps · last 5m ago"]);
    assert_eq!(trimmed(&mut usage, 12, 1), ["OC 48.2k"]);
    assert_eq!(trimmed(&mut usage, 14, 2), ["OpenCode 48.2k", "$1.37 est."]);
    assert_eq!(trimmed(&mut usage, 64, 1), ["OpenCode · 48.2k tokens today · $1.37 est."]);
    let tall = trimmed(&mut usage, 30, 24);
    assert_eq!(tall[..3], ["OpenCode 48.2k tokens today", "$1.37 est. · 12 steps", "last 5m ago · 9M cache reads"]);
    assert!(tall[3..23].iter().all(String::is_empty) && tall[23] == "r reload · q quit · ? help", "{tall:?}");
    assert_eq!(
        trimmed(&mut usage, 100, 5),
        [
            "tokens today",
            "█ █ ▟▀▙   ▀▀▙",
            "██ OpenCode     ▀▀█ ▟▀▙   ▟▀▀",
            "█ ▜▄▛ ▄ █▄▄k",
            "$1.37 est. · 12 steps · last 5m ago · 9M cache reads",
        ]
    );
    // Large figures on a narrow rail use the same digits once they fit beside the name.
    usage.large_figures = true;
    assert_eq!(
        trimmed(&mut usage, 36, 9)[1..5],
        ["tokens today", "█ █ ▟▀▙   ▀▀▙", "██ OpenCode ▀▀█ ▟▀▙   ▟▀▀", "█ ▜▄▛ ▄ █▄▄k"]
    );
}

#[test]
fn opencode_takes_the_second_place_beside_one_tool_and_a_row_beneath_two() {
    let now = local_noon();
    for (codex, name, left) in [(true, "Codex", "26%"), (false, "Claude", "8%")] {
        let (_f, mut usage) = pane(now, codex, !codex, true);
        let tool = format!("{name} week · {left} left");
        assert_eq!(trimmed(&mut usage, 22, 2), [tool.as_str(), "OpenCode 48.2k tokens"]);
        assert_eq!(trimmed(&mut usage, 22, 1), [tool.as_str()], "one row goes to the limit");
        let rail = trimmed(&mut usage, 28, 9);
        assert_eq!(rail[..2], [tool.as_str(), "Resets in 2d"]);
        assert_eq!(
            rail[4..],
            [
                "─".repeat(26).as_str(),
                "OpenCode 48.2k tokens",
                "$1.37 est. · 12 steps",
                "last 5m ago",
                "9M cache reads"
            ]
        );
        let wide = frame(&mut usage, 140, 5);
        let (first, second) = wide[2].split_once('│').unwrap();
        assert!(first.contains(&format!("██ {name}")) && second.contains("██ OpenCode"), "{wide:?}");
        let strip = frame(&mut usage, 140, 1);
        assert!(strip[0].contains(name) && strip[0].contains("│  OpenCode · 48.2k tokens today · $1.37 est."));
    }

    let (_f, mut usage) = pane(now, true, true, true);
    let limits = ["Codex week · 26% left", "Claude week · 8% left"];
    assert_eq!(trimmed(&mut usage, 28, 2), limits, "two rows go to the two limits");
    assert_eq!(trimmed(&mut usage, 28, 3), [limits[0], limits[1], "OpenCode 48.2k tokens"]);
    let rail = trimmed(&mut usage, 28, 8);
    assert_eq!(rail[6..], ["─".repeat(26).as_str(), "OpenCode 48.2k tokens"]);
    let wide = trimmed(&mut usage, 100, 9);
    assert_eq!(wide[7..], ["OpenCode · 48.2k tokens today · $1.37 est.", "r reload · q quit · ? help"]);
    assert!(!names_opencode(&frame(&mut usage, 100, 8)), "the panels keep their rows on a shorter pane");
}

#[test]
fn always_show_keeps_a_place_and_says_what_is_unknown_without_a_number() {
    let now = local_noon();
    let f = Fixture::folder();
    codex_limits(&f, now);
    let mut usage = automatic(&f, now).showing(tools(AUTO, ALWAYS, ALWAYS));
    usage.check();
    assert_eq!(
        trimmed(&mut usage, 28, 3),
        ["Codex week · 26% left", "Claude · no saved limits", "OpenCode · no database"]
    );
    let rail = trimmed(&mut usage, 30, 12).join("\n");
    assert!(rail.contains("Run milo setup") && rail.contains("OpenCode · no database found"), "{rail}");
    assert_eq!(trimmed(&mut usage, 22, 2), ["Codex week · 26% left", "Claude no saved limits"]);
    // Where a figure would be, the unknown tool has a dash and its next step, never a number.
    let wide = frame(&mut usage, 140, 5);
    let claude: Vec<&str> = wide.iter().map(|row| row.split_once('│').unwrap().1).collect();
    assert!(claude[2].contains('—') && claude[4].contains("no saved limits found · Run milo setup"), "{wide:?}");
    assert!(!claude.concat().contains(|ch: char| ch == '%' || ch.is_ascii_digit()), "{wide:?}");

    // Asked for alone, OpenCode says what it is waiting for rather than nothing.
    let mut usage = automatic(&Fixture::folder(), now).showing(tools(AUTO, AUTO, ALWAYS));
    usage.check();
    assert_eq!(trimmed(&mut usage, 30, 2), ["OpenCode · no database found", "Run OpenCode"]);
    usage.opencode.report = Report::Today(Activity {
        since: usage.today.since,
        store: Store::V1,
        steps: 0,
        tokens: Tokens::default(),
        cost: None,
        last: None,
    });
    assert_eq!(trimmed(&mut usage, 40, 2), ["OpenCode · no tokens recorded today"]);
    assert_eq!(trimmed(&mut usage, 22, 2), ["OpenCode · no tokens"]);
}

#[test]
fn a_hidden_tool_is_neither_shown_nor_looked_for() {
    let now = local_noon();
    let f = Fixture::folder();
    codex_limits(&f, now);
    claude_limits(&f, now);
    write(&f, CLAUDE, &assistant(now - MINUTE, 12_000, 900, 100), now);
    let databases =
        |usage: Usage| usage.with_databases(Some(f.root.join("agent.db")), Some(f.root.join("opencode.db")), TEST_WAIT);

    let mut usage = databases(automatic(&f, now).showing(tools(HIDE, AUTO, HIDE)));
    usage.check();
    assert!(
        usage.codex.files.entries.is_empty() && usage.codex.reported(usage.now()).is_none(),
        "Codex's sessions were walked"
    );
    assert!(usage.opencode.poll.fd().is_none(), "OpenCode's database was read");
    assert!(usage.omp.fd().is_some(), "Claude Code still has OMP's reports to read");
    let codex = Reading { at: Timestamp::seconds(now), windows: Limits::default() };
    assert!(!usage.keep(omp::Reports { codex: Some(codex), claude: None }), "a hidden tool's report is dropped");
    opencode_today(&mut usage, now);
    every_size(&mut usage, |w, h, rows| {
        let text = rows.join("\n");
        assert!(!text.contains("Codex") && !names_opencode(rows), "{w}x{h}: {rows:?}");
    });
    assert_eq!(
        trimmed(&mut usage, 28, 4),
        ["Claude week · 8% left", "Resets in 2d", "5h 48% left", "13k tokens today"]
    );
    assert!(!usage.watch_scope().roots.iter().any(|root| root.ends_with("codex")));

    let mut usage = databases(automatic(&f, now).showing(tools(HIDE, HIDE, HIDE)));
    usage.check();
    assert!(usage.external_fd().is_none(), "no database is opened for hidden tools");
    assert!(usage.watch_scope().roots.is_empty() && usage.bytes_read == 0);
    assert_eq!(trimmed(&mut usage, 28, 4), ["Usage is hidden", "Every tool is hidden.", "Change this in milo setup."]);
    assert_eq!(trimmed(&mut usage, 22, 2), ["Usage is hidden", "See milo setup"]);
    assert_eq!(usage.doctor(None, None, TEST_WAIT), [""; 0]);
}

#[test]
fn leftovers_and_unrelated_files_are_not_a_tool_but_a_saved_report_is() {
    let now = local_noon();
    let f = Fixture::folder();
    // Folders with nothing in them, empty files, and files that are something else.
    fs::create_dir_all(f.root.join("codex/2026/11/18")).unwrap();
    write(&f, "codex/2026/11/17/rollout-empty.jsonl", "", now);
    write(&f, "codex/config.toml", "model = \"x\"\n", now);
    write(&f, "codex/2026/11/17/notes.jsonl", &codex(now, 23, now + DAY, "null"), now);
    fs::create_dir_all(f.root.join("claude/project/subagents")).unwrap();
    write(&f, "claude/project/empty.jsonl", "", now);
    write(&f, "claude/settings.json", "{}", now);
    write(&f, &format!("{STATE}/{SAVED_LIMITS}"), "", now);
    write(&f, "claude.json", "", now);
    let found = |f: &Fixture| {
        let mut usage = automatic(f, now);
        usage.claude_config = Some(f.root.join("claude.json"));
        usage.check();
        usage
    };
    let mut usage = found(&f);
    assert_eq!(
        [usage.codex.presence(usage.codex.reported(now)), usage.claude.presence(usage.claude.reported(now))],
        [Presence::Absent; 2]
    );
    assert_eq!(usage.opencode.presence(usage.today.since), Presence::Absent);
    assert_eq!(trimmed(&mut usage, 28, 2), ["No usage to show yet"]);
    assert_eq!(trimmed(&mut usage, 28, 4), ["No usage to show yet"]);
    assert_eq!(trimmed(&mut usage, 12, 2), ["No usage yet"]);
    every_size(&mut usage, |w, h, rows| {
        assert!(!rows.join("\n").contains(['%', '—', '│', '─']), "{w}x{h}: {rows:?}");
    });
    assert_eq!(usage.doctor(None, None, TEST_WAIT), ["Usage tools: none found"]);

    // A tool that keeps data here is found before it has a figure. It gets one
    // quiet line with its next step, and still no place or number.
    write(&f, "claude.json", r#"{"numStartups":3}"#, now);
    let mut usage = found(&f);
    assert_eq!(
        [usage.codex.presence(usage.codex.reported(now)), usage.claude.presence(usage.claude.reported(now))],
        [Presence::Absent, Presence::Found]
    );
    assert_eq!(trimmed(&mut usage, 36, 2), ["Claude · no saved limits found", "Run milo setup"]);
    assert_eq!(
        usage.doctor(None, None, TEST_WAIT),
        ["Claude Code limits: no local report; turn on Show Claude Code limits in milo setup"]
    );
    write(&f, CODEX, "{\"type\":\"session_meta\"}\n", now);
    let mut usage = found(&f);
    assert_eq!(usage.codex.presence(usage.codex.reported(now)), Presence::Found);
    assert_eq!(
        trimmed(&mut usage, 36, 4),
        ["No usage to show yet", "Codex · no limits to show", "Claude · no saved limits found"]
    );
    assert_eq!(trimmed(&mut usage, 22, 2), ["Codex · no limits", "Claude · no limits"]);
    assert!(usage.help().text().contains("Claude Code") && !usage.help().text().contains("OpenCode"));

    // A found tool without a figure gives its rows to one that has figures.
    codex_limits(&f, now);
    let mut usage = found(&f);
    assert_eq!(trimmed(&mut usage, 22, 2), ["Codex week · 26% left", "Resets in 2d"]);
    assert_eq!(
        usage.doctor(None, None, TEST_WAIT),
        [
            "Codex limits: Codex session log",
            "Claude Code limits: no local report; turn on Show Claude Code limits in milo setup",
        ]
    );

    // A report OMP saved stands for its tool with nothing else of the tool's on disk.
    let mut usage = automatic(&Fixture::folder(), now);
    usage.check();
    let week = Window { minutes: 10_080, percent: 32.0, resets: Some(now + DAY), observed: now };
    let claude = Reading { at: Timestamp::seconds(now), windows: Limits::new([None, Some(week)]) };
    assert!(usage.keep(omp::Reports { codex: None, claude: Some(claude) }));
    assert_eq!(
        [usage.codex.presence(usage.codex.reported(now)), usage.claude.presence(usage.claude.reported(now))],
        [Presence::Absent, Presence::Reading]
    );
    assert_eq!(trimmed(&mut usage, 22, 2), ["Claude week · 68% left", "Resets in 1d"]);
}

#[test]
fn the_first_look_says_it_is_checking_and_then_settles() {
    if !sqlite_installed() {
        return;
    }
    let now = local_noon();
    let f = Fixture::folder();
    let db = opencode_activity::busy_day(&f, opencode_activity::RELEASES[3], now);
    let mut usage = automatic(&f, now).with_databases(None, Some(db.path.clone()), TEST_WAIT);
    usage.check();
    assert_eq!(trimmed(&mut usage, 28, 2), ["Checking local usage…"]);
    opencode_activity::settle(&mut usage);
    assert_eq!(usage.opencode.presence(usage.today.since), Presence::Reading);
    assert_eq!(trimmed(&mut usage, 28, 2), ["OpenCode 1.5k tokens today", "$0.42 est. · 2 steps"]);
    assert!(usage.help().text().contains("OpenCode"));
    let lines = usage_doctor(&f, tools(AUTO, AUTO, AUTO), &db.path, now);
    assert!(lines[0].starts_with("OpenCode activity: 1504 tokens") && lines.len() == 3, "{lines:?}");

    // A database with nothing from today is found, but has no figure to show.
    fs::remove_file(&db.path).unwrap();
    let (version, schema, store) = opencode_activity::RELEASES[3];
    let quiet = opencode_activity::Database::new(&f, schema, store);
    quiet.session("ses_a", version, now - DAY, None);
    let mut usage = automatic(&f, now).with_databases(None, Some(quiet.path.clone()), TEST_WAIT);
    usage.check();
    opencode_activity::settle(&mut usage);
    assert_eq!(usage.opencode.presence(usage.today.since), Presence::Found);
    assert_eq!(trimmed(&mut usage, 40, 2), ["No usage to show yet", "OpenCode · no tokens recorded today"]);
}

#[test]
fn every_combination_and_choice_draws_at_every_small_size() {
    let now = local_noon();
    for combination in 0..8 {
        let (_f, mut usage) = pane(now, combination & 1 != 0, combination & 2 != 0, combination & 4 != 0);
        for choices in [tools(AUTO, AUTO, AUTO), tools(ALWAYS, ALWAYS, ALWAYS), tools(HIDE, ALWAYS, AUTO)] {
            usage.tools = choices;
            for large in [false, true] {
                usage.large_figures = large;
                for (w, h) in (0..=72).flat_map(|w| (0..=10).map(move |h| (w, h))) {
                    frame(&mut usage, w, h);
                }
            }
        }
    }
}

fn usage_doctor(f: &Fixture, tools: UsageTools, opencode: &Path, now: i64) -> Vec<String> {
    automatic(f, now).showing(tools).doctor(None, Some(opencode.to_owned()), TEST_WAIT)
}

#[test]
fn doctor_says_nothing_of_an_absent_tool_unless_it_is_always_shown() {
    let now = local_noon();
    let f = Fixture::folder();
    codex_limits(&f, now);
    claude_limits(&f, now);
    let missing = f.root.join("opencode.db");
    let limits = ["Codex limits: Codex session log", "Claude Code limits: Milo Claude status line"];
    assert_eq!(usage_doctor(&f, tools(AUTO, AUTO, AUTO), &missing, now), limits);
    assert_eq!(
        usage_doctor(&f, tools(AUTO, AUTO, ALWAYS), &missing, now),
        [limits[0], limits[1], "OpenCode activity: no local database"]
    );
    assert_eq!(usage_doctor(&f, tools(HIDE, AUTO, AUTO), &missing, now), &limits[1..]);
    let empty = Fixture::folder();
    assert_eq!(
        usage_doctor(&empty, tools(ALWAYS, AUTO, HIDE), &missing, now),
        ["Codex limits: no local report; run Codex to save one"]
    );
}
