//! Claude's status line and the windows it saves for the pane.

use super::*;

#[test]
fn saved_claude_limits_are_still_shown_three_days_later() {
    let f = Fixture::folder();
    let then = NOW - 3 * DAY;
    let status = std::str::from_utf8(STATUS).unwrap().strip_prefix('{').unwrap();
    f.write(&format!("{STATE}/{SAVED_LIMITS}"), &format!("{{\"saved_at\":{then},{status}"));
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].contains("77%") && rows[1].contains("39%"));
    assert_eq!(rows[2].matches("3 d ago").count(), 1);
}

#[test]
fn claude_status_saves_limits_and_prints_a_short_plain_line() {
    let f = Fixture::folder();
    let path = f.root.join(STATE).join(SAVED_LIMITS);
    let mut output = Vec::new();
    assert_eq!(status::run(STATUS, &mut output, Some(&path), NOW), 0);
    assert_eq!(String::from_utf8(output).unwrap(), "5h 77% · week 39% left\n");
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 100, 2)[1].contains("week"));
    let budget = &mut Budget::new(16384);
    let file = fs_ops::open_read(&path).unwrap();
    let version = FileVersion::opened(&file).unwrap();
    let saved = status::saved(file, version, budget).unwrap();
    assert_eq!(saved.windows.base[0].unwrap().observed, NOW);
    assert_eq!(saved.windows.base[1].unwrap().resets, Some(1_795_219_200));
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
    assert!(rows[1].contains("15%") && rows[1].contains("55%"));
    // Keeping the higher, older figures keeps their age too.
    assert_eq!(rows[2].matches("2 h ago").count(), 1);

    let before = fs::read(&path).unwrap();
    claude_report(&path, &format!(r#"{{"five_hour":{{"used_percentage":85,"resets_at":{reset}}}}}"#), NOW);
    assert_eq!(fs::read(&path).unwrap(), before);

    claude_report(&path, &format!(r#"{{"seven_day":{{"used_percentage":55,"resets_at":{week}}}}}"#), NOW);
    assert!(usage.check());
    let rows = frame(&mut usage, 140, 3);
    assert!(rows[1].contains("15%") && rows[1].contains("45%"));
    assert_eq!(rows[2].matches("2 h ago").count(), 1);

    usage.fixed_now = Some(reset);
    let rows = frame(&mut usage, 140, 3);
    assert!(!rows[1].contains("100%") && rows[2].contains("Reset elapsed"));
    let next = reset + 5 * HOUR;
    claude_report(&path, &format!(r#"{{"five_hour":{{"used_percentage":5,"resets_at":{next}}}}}"#), reset);
    claude_report(&path, &format!(r#"{{"five_hour":{{"used_percentage":95,"resets_at":{reset}}}}}"#), reset);
    assert!(usage.check());
    let rows = frame(&mut usage, 140, 3);
    let figures: Vec<&str> = rows[1].split_whitespace().collect();
    assert!(figures.contains(&"95%") && !figures.contains(&"5%"));
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
    let mut before = fs_ops::open_read(&path).unwrap();
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
    assert!(frame(&mut open(&f, NOW + 1), 100, 2)[1].contains("47%"));
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
        assert_eq!(String::from_utf8(output).unwrap(), "5h 77% · week 39% left\n");
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
        let code = status::run_fd(reader, &mut output, None, NOW, status::INPUT_WAIT);
        let _ = send.send((code, output));
    });
    assert_eq!(receive.recv_timeout(TEST_WAIT).unwrap(), (0, b"\n".to_vec()));
    drop(writer);
    thread.join().unwrap();

    let (reader, mut writer) = UnixStream::pair().unwrap();
    writer.write_all(STATUS).unwrap();
    drop(writer);
    let mut output = Vec::new();
    assert_eq!(status::run_fd(reader, &mut output, None, NOW, TEST_WAIT), 0);
    assert_eq!(String::from_utf8(output).unwrap(), "5h 77% · week 39% left\n");
}

#[test]
fn a_newer_empty_claude_reading_explains_why_no_figures_are_shown() {
    let f = with_limits();
    for utilization in ["{}", "null"] {
        let config = format!(
            r#"{{"cachedUsageUtilization":{{"fetchedAtMs":{},"utilization":{utilization}}}}}"#,
            NOW * 1000 + 100
        );
        write(&f, ".claude.json", &config, NOW);
        let mut usage = unopened(&f, NOW);
        usage.claude_config = Some(f.root.join(".claude.json"));
        assert!(usage.check());
        let rows = frame(&mut usage, 140, 3);
        let claude = |row: usize| rows[row].split_once('│').unwrap().1;
        assert!(!claude(1).contains('%'));
        assert_eq!(claude(2).trim(), "no limits to show · Open Claude Code");
    }
}

#[test]
fn the_status_model_keeps_wide_letters_and_accents_without_overrunning_the_line() {
    let wide = "界".repeat(25);
    let input = format!(r#"{{"model":{{"display_name":"\u0301 Cafe\u0301 \u001b\u202e{wide}"}}}}"#);
    let mut output = Vec::new();
    assert_eq!(status::run(input.as_bytes(), &mut output, None, NOW), 0);
    assert_eq!(String::from_utf8(output).unwrap(), format!("Cafe\u{0301} {}\n", "界".repeat(17)));
}

#[test]
fn claude_status_does_not_describe_an_elapsed_reset_as_a_fresh_limit() {
    let mut output = Vec::new();
    assert_eq!(status::run(STATUS, &mut output, None, 1_795_110_000), 0);
    assert_eq!(output, b"5h reset elapsed \xc2\xb7 week 39% left\n");
}
