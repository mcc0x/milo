//! Codex session readings, ordering and missing data.

use super::*;

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
        assert!(left(2).contains("no limits reported"));
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
    assert_eq!(rows[2].split_once('│').unwrap().0.trim(), "no local source found");
    write(&f, CODEX, "{}\n", NOW);
    let rows = frame(&mut open(&f, NOW), 140, 3);
    assert!(rows[1].split('│').next().unwrap().contains('—'));
    assert_eq!(rows[2].split_once('│').unwrap().0.trim(), "no limits reported");
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
