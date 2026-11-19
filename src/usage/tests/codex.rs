//! Codex session readings, ordering and missing data.

use super::*;

#[test]
fn codex_shows_percent_window_and_time_until_reset() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 23, NOW + 2 * HOUR + 10 * MINUTE, "null"), NOW);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[0].contains("week") && rows[1].contains("77%") && rows[2].contains("left · resets in 2 h 10 m"));
}

#[test]
fn newest_snapshot_wins_across_files_regardless_of_file_order_or_mtime() {
    let f = Fixture::folder();
    let stamp = iso(NOW - 60);
    write(&f, CODEX, &codex_at(&format!("{stamp}.901Z"), 67, NOW + HOUR, "null"), NOW - 60);
    write(&f, OTHER_CODEX, &codex_at(&format!("{stamp}.101Z"), 13, NOW + HOUR, "null"), NOW);
    let rows = frame(&mut open(&f, NOW), 85, 2);
    assert!(rows[0].contains("33%") && !rows[0].contains("87%"));
}

#[test]
fn codex_clamps_overfull_windows_and_accepts_whole_minutes_written_as_floats() {
    let f = Fixture::folder();
    for (percent, minutes) in [("100.5", "300.0"), ("150", "3e2")] {
        let window = format!(r#"{{"used_percent":{percent},"window_minutes":{minutes}}}"#);
        write(&f, CODEX, &codex_snapshot(&format!("{}Z", iso(NOW)), &window, "null"), NOW);
        let rows = frame(&mut open(&f, NOW), 140, 3);
        assert!(rows[0].contains("5h") && rows[1].split_whitespace().any(|word| word == "0%"));
    }
}

#[test]
fn a_nonzero_window_near_an_endpoint_does_not_look_empty_or_full() {
    let f = Fixture::folder();
    for (percent, shown) in [("0.4", "99+%"), ("99.9", "<1%")] {
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
        assert!(left(1).contains('—') && !left(1).contains("60%"));
        assert!(left(2).contains("no limits to show"));
    }
}

#[test]
fn codex_token_count_without_a_limit_report_does_not_erase_the_previous_figure() {
    let f = Fixture::folder();
    let previous = codex(NOW - MINUTE, 40, NOW + DAY, "null");
    let stamp = iso(NOW);
    for limits in ["null", "0", "[]"] {
        let record = format!(
            "{{\"timestamp\":\"{stamp}Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
             \"rate_limits\":{limits}}}}}\n"
        );
        write(&f, CODEX, &format!("{previous}{record}"), NOW);
        assert!(frame(&mut open(&f, NOW), 140, 3)[1].contains("60%"));
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
    assert!(rows[0].contains("38%") && !rows[0].contains("59%") && !rows[0].contains(" 5%"));
}

#[test]
fn codex_shows_reset_when_the_time_passes_without_an_append() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 77, NOW + 5, "null"), NOW);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 85, 2)[0].contains("23%"));
    assert_eq!(usage.next_frame(), Some(Duration::from_secs(5)));
    usage.fixed_now = Some(NOW + 4);
    assert!(frame(&mut usage, 85, 2)[0].contains("23%"));
    usage.fixed_now = Some(NOW + 5);
    assert_eq!(usage.next_frame(), Some(Duration::from_secs(60)));
    let rows = frame(&mut usage, 140, 3);
    assert!(!rows[1].contains("100%") && !rows[1].contains("23%"));
    assert!(rows[2].split('│').next().unwrap().contains("Reset elapsed"));
}

#[test]
fn an_old_codex_snapshot_says_how_old_it_is() {
    let f = Fixture::folder();
    let then = NOW - 10 * DAY;
    write(&f, CODEX, &codex(then, 33, NOW + DAY, "null"), then);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].contains("67%"));
    assert!(rows[2].contains("10 d ago"));
}

#[test]
fn codex_ignores_null_secondary_and_shows_a_present_one() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 23, NOW + DAY, "null"), NOW);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert_eq!(rows[1].split_once('│').unwrap().0.matches('%').count(), 1);
    let secondary = codex_window(89, 300, NOW + HOUR);
    write(&f, CODEX, &codex(NOW, 23, NOW + DAY, &secondary), NOW);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    let figures = rows[1].split_once('│').unwrap().0;
    assert!(rows[0].contains("5h") && figures.contains("11%") && figures.contains("77%"));
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
    assert!(rows[0].contains("54%") && !rows[0].contains(" 1%"));
    assert_eq!(usage.bytes_read, u64::try_from(kept.len() + 3 * empty.len()).unwrap());
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 0);
}

fn append(f: &Fixture, data: &str, now: i64) {
    let mut file = OpenOptions::new().append(true).open(f.root.join(CODEX)).unwrap();
    file.write_all(data.as_bytes()).unwrap();
    file.set_modified(mtime(now)).unwrap();
}

#[test]
fn codex_growth_reads_the_suffix_and_keeps_the_last_report() {
    let f = Fixture::folder();
    let first = codex(NOW - 1, 23, NOW + DAY, "null");
    write(&f, CODEX, &format!("{first}{}", "{}\n".repeat(32_768)), NOW - 1);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));

    let other =
        codex(NOW, 98, NOW + DAY, "null").replace("\"rate_limits\":{", "\"rate_limits\":{\"limit_id\":\"other\",");
    append(&f, &other, NOW);
    assert!(usage.check());
    assert_eq!(usage.bytes_read, other.len() as u64 + 1);
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));

    let latest = codex(NOW, 64, NOW + DAY, "null");
    append(&f, &latest, NOW + 1);
    assert!(usage.check());
    assert_eq!(usage.bytes_read, latest.len() as u64 + 1);
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 0);
}

#[test]
fn codex_resumes_a_partial_line_and_rechecks_a_report_without_a_newline() {
    let f = Fixture::folder();
    let first = codex(NOW - 1, 23, NOW + DAY, "null");
    let newest =
        codex(NOW, 64, NOW + DAY, "null").replacen('{', &format!("{{\"padding\":\"{}\",", "x".repeat(40_000)), 1);
    let (prefix, suffix) = newest.split_at(36_000);
    write(&f, CODEX, &format!("{first}{prefix}"), NOW);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));

    append(&f, suffix.trim_end(), NOW + 1);
    assert!(usage.check());
    assert_eq!(usage.bytes_read, suffix.len() as u64);
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));

    append(&f, "broken\n", NOW + 2);
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 8);
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));

    let latest = codex(NOW + 1, 64, NOW + DAY, "null");
    let (prefix, suffix) = latest.split_at(latest.len() / 2);
    append(&f, prefix, NOW + 3);
    assert!(usage.check());
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));
    append(&f, suffix.trim_end(), NOW + 4);
    assert!(usage.check());
    assert_eq!(usage.bytes_read, suffix.len() as u64);
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));
    append(&f, "\n{}\n", NOW + 5);
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 5);
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));
}

#[test]
fn codex_truncation_rewrite_and_replacement_reset_the_cursor() {
    let f = Fixture::folder();
    let first = codex(NOW - 1, 23, NOW + DAY, "null");
    write(&f, CODEX, &format!("{first}{{\"unfinished\":"), NOW - 1);
    let mut usage = open(&f, NOW);
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));
    write(&f, CODEX, "{}\n", NOW);
    assert!(usage.check());
    assert!(usage.codex.reported().is_none());

    let latest = codex(NOW, 64, NOW + DAY, "null");
    append(&f, &latest, NOW + 1);
    assert!(usage.check());
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));

    let rewrite = codex(NOW, 41, NOW + DAY, "null");
    write(&f, CODEX, &format!("{{}}\n{rewrite}"), NOW + 2);
    assert!(usage.check());
    assert!(frame(&mut usage, 100, 2)[0].contains("59%"));

    let rewrite = codex(NOW, 52, NOW + DAY, "null");
    write(&f, CODEX, &format!("{rewrite}{{}} \n"), NOW + 3);
    assert!(usage.check());
    assert!(frame(&mut usage, 100, 2)[0].contains("48%"));

    let larger = format!("{}x\n", "{}\n".repeat(latest.len()));
    write(&f, "replacement", &larger, NOW + 4);
    fs::rename(f.root.join("replacement"), f.root.join(CODEX)).unwrap();
    assert!(usage.check());
    assert!(usage.codex.reported().is_none());
}

#[test]
fn codex_large_growth_uses_a_bounded_tail_and_resumes_at_its_new_end() {
    let f = Fixture::folder();
    let first = codex(NOW - 1, 23, NOW + DAY, "null");
    write(&f, CODEX, &first, NOW - 1);
    let mut usage = open(&f, NOW);

    let large = "x".repeat(usize::try_from(TAIL_BYTES).unwrap());
    append(&f, &large, NOW);
    assert!(usage.check());
    assert_eq!(usage.bytes_read, TAIL_BYTES);
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));

    // The skipped record has no newline yet. Its suffix must not be treated as a new report.
    let ignored = codex(NOW, 98, NOW + DAY, "null");
    append(&f, &ignored, NOW + 1);
    assert!(usage.check());
    assert_eq!(usage.bytes_read, ignored.len() as u64);
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));

    let latest = codex(NOW, 64, NOW + DAY, "null");
    append(&f, &format!("{large}\n{latest}"), NOW + 2);
    assert!(usage.check());
    assert!(usage.bytes_read < TAIL_BYTES);
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));

    append(&f, "{}\n", NOW + 3);
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 4);
    assert!(frame(&mut usage, 100, 2)[0].contains("36%"));
}

#[test]
fn codex_preserves_a_report_outside_the_tail_after_chmod() {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW - 1, 23, NOW + DAY, "null"), NOW - 1);
    let mut usage = open(&f, NOW);
    append(&f, &"x".repeat(usize::try_from(TAIL_BYTES).unwrap()), NOW);
    assert!(usage.check());
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));

    let path = f.root.join(CODEX);
    let before = path.metadata().unwrap();
    let permissions = fs::Permissions::from_mode(before.mode() ^ 0o100);
    let deadline = Instant::now() + Duration::from_secs(2);
    let after = loop {
        fs::set_permissions(&path, permissions.clone()).unwrap();
        let after = path.metadata().unwrap();
        if (after.ctime(), after.ctime_nsec()) != (before.ctime(), before.ctime_nsec()) {
            break after;
        }
        assert!(Instant::now() < deadline, "inode change time did not advance");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());

    assert!(usage.check());
    assert_eq!(usage.bytes_read, 0);
    assert!(frame(&mut usage, 100, 2)[0].contains("77%"));
}

#[test]
fn no_codex_sessions_and_sessions_without_limits_have_different_notes() {
    let f = Fixture::folder();
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].split('│').next().unwrap().contains('—'));
    assert_eq!(rows[2].split_once('│').unwrap().0.trim(), "no sessions found · Run Codex");
    write(&f, CODEX, "{}\n", NOW);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].split('│').next().unwrap().contains('—'));
    assert_eq!(rows[2].split_once('│').unwrap().0.trim(), "no limits to show · Open Codex");
}

#[test]
fn an_unreadable_codex_root_is_not_reported_as_no_limits() {
    let f = Fixture::folder();
    f.write("codex", "not a directory");
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[2].contains("could not read sessions"), "{}", rows[2]);
}

#[test]
fn an_unreadable_codex_day_is_not_reported_as_no_sessions() {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 23, NOW + DAY, "null"), NOW);
    let dir = f.root.join("codex/2026/11/18");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
    let rows = fs::read_dir(&dir).is_err().then(|| frame(&mut open(&f, NOW), 140, 3));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    if let Some(rows) = rows {
        assert!(rows[2].contains("could not read sessions"), "{}", rows[2]);
    }
}
