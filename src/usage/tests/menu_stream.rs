use std::io::{BufRead as _, BufReader};
use std::thread;

use super::super::menu::{global_choices, snapshots};
use super::*;

fn snapshot(usage: &mut Usage) -> String {
    let mut line = String::new();
    usage.menu_snapshot(&mut line);
    assert!(line.ends_with('\n'));
    assert_eq!(line.lines().count(), 1);
    assert_eq!(field(Value::parse(line.as_bytes()).unwrap(), "version").unsigned(), Some(1));
    line
}

fn field<'a>(value: Value<'a>, key: &str) -> Value<'a> {
    value.fields([key])[0].unwrap_or_else(|| panic!("missing {key}"))
}

fn providers(line: &str) -> impl Iterator<Item = Value<'_>> {
    field(Value::parse(line.as_bytes()).unwrap(), "providers").elements().unwrap()
}

fn window(provider: Value<'_>, index: usize) -> Value<'_> {
    field(provider, "windows").elements().unwrap().nth(index).unwrap()
}

#[test]
fn menu_stream_limit_values_freshness_and_elapsed_state_match_the_pane() {
    let f = Fixture::folder();
    let reset = NOW + HOUR;
    write(&f, CODEX, &codex(NOW, 23, reset, "null"), NOW);
    let mut usage = automatic(&f, NOW);
    usage.check();

    // OMP can report infrequently, so the Claude figure stays current twice as long.
    usage.claude.omp = Some(Reading {
        at: Timestamp::seconds(NOW),
        windows: Limits::new([Some(Window { minutes: 300, percent: 61.0, resets: Some(reset), observed: NOW }), None]),
    });
    let line = snapshot(&mut usage);
    let [codex, claude]: [Value<'_>; 2] = providers(&line).collect::<Vec<_>>().try_into().ok().unwrap();
    for (provider, (id, label, digits)) in [codex, claude].into_iter().zip([("codex", "week", 77), ("claude", "5h", 39)]) {
        let window = window(provider, 0);
        assert!(field(provider, "id").is(id));
        assert!(field(window, "label").is(label));
        assert_eq!(field(window, "remaining").unsigned(), Some(digits));
        assert_eq!(field(window, "fresh").boolean(), Some(true));
        assert_eq!(field(window, "resetAt").integer(), Some(reset));
    }
    let rail = frame(&mut usage, 76, 2).join("\n");
    assert!(rail.contains("77%") && rail.contains("39%"), "{rail}");

    for (at, expected) in [
        (NOW + 5 * MINUTE - 1, [true, true]),
        (NOW + 5 * MINUTE, [false, true]),
        (NOW + 10 * MINUTE, [false, false]),
    ] {
        usage.fixed_now = Some(at);
        let line = snapshot(&mut usage);
        let actual = providers(&line).map(|provider| field(window(provider, 0), "fresh").boolean().unwrap());
        assert!(actual.eq(expected));
        let rail = frame(&mut usage, 100, 2).join("\n");
        assert_eq!(rail.matches("seen").count(), expected.into_iter().filter(|fresh| !fresh).count(), "{rail}");
    }

    usage.fixed_now = Some(reset);
    let line = snapshot(&mut usage);
    for provider in providers(&line) {
        assert!(field(provider, "windows").elements().unwrap().next().is_none());
        assert!(field(provider, "detail").is("Reset elapsed"));
    }
    let rail = frame(&mut usage, 100, 2).join("\n");
    assert!(!rail.contains('%') && rail.matches("elapsed").count() == 2, "{rail}");
}

#[test]
fn missing_historical_and_exhausted_limits_are_distinct_in_both_surfaces() {
    let f = Fixture::folder();
    let reset = NOW + HOUR;
    write(&f, CODEX, &codex(NOW, 100, reset, "null"), NOW);
    let mut usage = open(&f, NOW + 5 * MINUTE - 1);
    let expected_blank = "Claude no saved limits";

    for (now, shown, fresh) in [
        (NOW + 5 * MINUTE - 1, "Codex week · 0% left", true),
        (NOW + 5 * MINUTE, "Codex week · 0% last", false),
    ] {
        usage.fixed_now = Some(now);
        assert_eq!(frame(&mut usage, 22, 2), [shown, expected_blank]);
        let line = snapshot(&mut usage);
        let mut providers = providers(&line);
        let codex = window(providers.next().unwrap(), 0);
        assert_eq!(field(codex, "remaining").unsigned(), Some(0));
        assert_eq!(field(codex, "fresh").boolean(), Some(fresh));
        assert_eq!(field(codex, "staleAt").integer(), Some(NOW + 5 * MINUTE));
        let claude = providers.next().unwrap();
        assert!(field(claude, "windows").elements().unwrap().next().is_none());
        assert!(field(claude, "detail").is("No saved limits found"));
        assert!(providers.next().is_none(), "absent automatic OpenCode must stay hidden");
    }

    usage.fixed_now = Some(reset);
    let line = snapshot(&mut usage);
    let codex = providers(&line).next().unwrap();
    assert!(field(codex, "windows").elements().unwrap().next().is_none());
    assert!(field(codex, "detail").is("Reset elapsed"));
    assert_eq!(frame(&mut usage, 22, 2), ["Codex · reset elapsed", expected_blank]);
}

#[test]
fn enabled_providers_keep_their_specific_reasons_without_more_rows() {
    let f = Fixture::folder();
    write(&f, CODEX, "{\"type\":\"session_meta\"}\n", NOW);
    write(&f, &format!("{STATE}/{SAVED_LIMITS}"), "not a saved report", NOW);
    let mut usage = automatic(&f, NOW).showing(tools(Show::Always, Show::Always, Show::Always));
    usage.check();
    usage.opencode.report = opencode::Report::Unsupported;
    let rows = frame(&mut usage, 84, 2);
    assert!(rows[0].contains("Codex · no limits to show") && rows[0].contains("Claude · no usable saved limits"));
    assert!(rows[1].contains("OpenCode · database format not checked"));
    assert!(!rows.join("\n").contains('%'));

    let line = snapshot(&mut usage);
    for (provider, expected) in providers(&line).zip([
        "No limits to show",
        "No usable saved limits",
        "Database format not checked",
    ]) {
        assert!(field(provider, "windows").elements().unwrap().next().is_none());
        assert!(field(provider, "detail").is(expected));
    }
    assert_eq!(providers(&line).count(), 3);
}

#[test]
fn a_tighter_old_window_cannot_displace_a_current_one_in_the_two_row_pane() {
    let f = Fixture::folder();
    let mut usage = automatic(&f, NOW);
    let reset = NOW + HOUR;
    usage.claude.native = Some(status::Native {
        account: None,
        reading: Some(Reading {
            at: Timestamp::seconds(NOW),
            windows: Limits::new([
                Some(Window { minutes: 300, percent: 99.9, resets: Some(reset), observed: NOW }),
                Some(Window { minutes: 10_080, percent: 100.0, resets: Some(reset), observed: NOW - 5 * MINUTE }),
            ]),
        }),
    });
    let rail = frame(&mut usage, 22, 2);
    assert_eq!(rail, ["Claude 5h · <1% left", "Resets in 1h"]);
    let panel = frame(&mut usage, 72, 4);
    assert!(panel[0].contains("week · last reported") && panel[1].contains("0%"), "{panel:?}");
    assert!(panel[2].find("left").unwrap() < panel[2].find("5 m ago").unwrap(), "{panel:?}");
    let inline = frame(&mut usage, 72, 1);
    assert!(inline[0].contains("week 0% last") && !inline[0].contains("last left"), "{inline:?}");

    let line = snapshot(&mut usage);
    let claude = providers(&line).next().unwrap();
    let fresh = |index| field(window(claude, index), "fresh").boolean().unwrap();
    assert!(fresh(0));
    assert!(!fresh(1));
    assert_eq!(field(window(claude, 0), "remaining").unsigned(), Some(1));
    assert_eq!(field(window(claude, 1), "remaining").unsigned(), Some(0));
}

#[test]
fn menu_stream_global_preferences_change_without_reopening_an_unaffected_reader() {
    let f = Fixture::new();
    let config = f.root.join("config");
    let clock = Instant::now();
    let initial = codex(NOW - 1, 23, NOW + HOUR, "null");
    write(&f, CODEX, &format!("{initial}{}", "{}\n".repeat(32_768)), NOW - 1);
    f.write("config/preferences.conf", "usage_codex = auto\nusage_opencode = always\n");
    let [global, local] = preferences::layers(&config, &f.root);
    assert_eq!(global, config.join("preferences.conf"));
    fs::create_dir_all(local.parent().unwrap()).unwrap();
    fs::write(local, "usage_codex = hide\n").unwrap();
    let (initial, _) = global_choices(Some(&config)).unwrap();
    let mut usage = automatic(&f, NOW).showing(initial);
    usage.refresh(NOW, clock, false);
    let line = snapshot(&mut usage);
    assert!(providers(&line).map(|p| field(p, "id")).zip(["codex", "opencode"]).all(|(id, expected)| id.is(expected)));
    assert_eq!(providers(&line).count(), 2);
    assert!(field(providers(&line).nth(1).unwrap(), "detail").is("No database found"));

    f.write("config/preferences.conf", "usage_opencode = invalid\n");
    assert!(global_choices(Some(&config)).is_none());
    f.write("config/preferences.conf", "usage_codex = always\nusage_opencode = auto\n");
    usage.apply_menu_tools(None, global_choices(Some(&config)).unwrap().0);
    let newest = codex(NOW, 64, NOW + HOUR, "null");
    let path = f.root.join(CODEX);
    OpenOptions::new().append(true).open(&path).unwrap().write_all(newest.as_bytes()).unwrap();
    File::open(path).unwrap().set_modified(mtime(NOW)).unwrap();
    usage.refresh(NOW, clock + Duration::from_secs(1), true);
    assert_eq!(usage.bytes_read, u64::try_from(newest.len()).unwrap() + 1);
    let line = snapshot(&mut usage);
    assert_eq!(providers(&line).count(), 1);
    assert_eq!(field(window(providers(&line).next().unwrap(), 0), "remaining").unsigned(), Some(36));
    usage.refresh(NOW, clock + Duration::from_secs(31), false);
    assert_eq!(usage.bytes_read, 0);

    f.write("config/preferences.conf", "usage_codex = hide\n");
    usage.apply_menu_tools(None, global_choices(Some(&config)).unwrap().0);
    assert!(usage.codex_dir.is_none() && usage.codex.files.entries.is_empty());
    assert_eq!(providers(&snapshot(&mut usage)).count(), 0);
}

#[test]
fn menu_stream_open_code_reports_activity_but_never_invents_a_limit() {
    if !sqlite_installed() {
        return;
    }
    let f = Fixture::folder();
    let now = local_noon();
    let db = opencode_activity::busy_day(&f, opencode_activity::RELEASES[3], now);
    let mut usage = automatic(&f, now).with_databases(None, Some(db.path), TEST_WAIT);
    assert_eq!(providers(&snapshot(&mut usage)).count(), 0);
    usage.check();
    opencode_activity::settle(&mut usage);
    let line = snapshot(&mut usage);
    let opencode = providers(&line).next().unwrap();
    assert!(field(opencode, "id").is("opencode"));
    assert!(field(opencode, "detail").is("1.5k tokens today · $0.42 est."));
    assert!(field(opencode, "windows").elements().unwrap().next().is_none());
    let rail = frame(&mut usage, 76, 2).join("\n");
    assert!(rail.contains("1.5k tokens today"), "{rail}");
    assert!(!line.contains("/work") && !line.contains("SYNTHETIC-REPLY"));

    usage.fixed_now = Some(now + DAY);
    assert_eq!(providers(&snapshot(&mut usage)).count(), 0);
    usage.tools[UsageTool::OpenCode] = Show::Always;
    let line = snapshot(&mut usage);
    let opencode = providers(&line).next().unwrap();
    assert!(field(opencode, "windows").elements().unwrap().next().is_none());
    assert!(field(opencode, "detail").is("No reading for today yet"));
    assert!(frame(&mut usage, 76, 2)[0].contains("no reading for today yet"));
}

#[test]
fn menu_stream_flushes_on_startup_newline_timer_and_stops_at_eof() {
    let (input, mut request) = io::pipe().unwrap();
    let (output, response) = io::pipe().unwrap();
    let stream = thread::spawn(move || {
        snapshots(input, response, |line, request| {
            match request {
                None => line.push_str("{\"test\":\"startup\"}\n"),
                Some(true) => line.push_str("{\"test\":\"manual\"}\n"),
                Some(false) => line.push_str("{\"test\":\"timer\"}\n"),
            }
            Ok(if request == Some(true) { Duration::from_millis(20) } else { Duration::from_secs(5) })
        })
    });
    let mut lines = BufReader::new(output).lines();
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"test":"startup"}"#);
    request.write_all(b"refresh\n").unwrap();
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"test":"manual"}"#);
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"test":"timer"}"#);
    drop(request);
    assert!(lines.next().is_none());
    stream.join().unwrap().unwrap();
}

#[test]
fn menu_stream_treats_a_broken_pipe_as_normal_shutdown() {
    let (input, _request) = io::pipe().unwrap();
    let (output, response) = io::pipe().unwrap();
    drop(output);
    snapshots(input, response, |line, _| {
        line.push_str("{}\n");
        Ok(Duration::from_secs(60))
    })
    .unwrap();
}

#[test]
fn menu_stream_quits_when_an_idle_output_loses_its_reader() {
    let (input, _request) = io::pipe().unwrap();
    let (output, response) = io::pipe().unwrap();
    let (done, result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let finished = snapshots(input, response, |line, _| {
            line.push_str("{}\n");
            Ok(Duration::from_secs(60))
        });
        let _ = done.send(finished);
    });
    let mut lines = BufReader::new(output).lines();
    assert_eq!(lines.next().unwrap().unwrap(), "{}");
    drop(lines);
    result.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
}
