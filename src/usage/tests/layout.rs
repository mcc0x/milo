//! What the usage pane shows at different sizes and times.

use super::*;

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
    let tall = matches!(rows.len(), 5 | 8 | 12);
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
    let codex = ("73%", " █  ▄▄▛%");
    let claude = ("48%", "  █ ▜▄▛%");
    let dash = ("—", "—");
    for (name, codex_at, claude_at, reset_at, left, right) in [
        ("both", Some(now), Some(now), next, codex, claude),
        ("Codex only", Some(now), None, next, codex, dash),
        ("Claude only", None, Some(now), next, dash, claude),
        ("neither", None, None, next, dash, dash),
    ] {
        let f = Fixture::folder();
        example_limits(&f, codex_at, claude_at, reset_at, now);
        if name != "neither" {
            write(&f, CLAUDE, &assistant(now - MINUTE, 12_000, 900, 100), now);
        }
        let mut usage = open(&f, now).with_large_figures(true);
        for (w, h) in [(140, 5), (100, 1), (36, 12)] {
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
        let mut usage = open(&f, now).with_large_figures(true);
        let rows = frame(&mut usage, 140, 5);
        assert!(rows.iter().all(|row| row.chars().nth(69) == Some('│')));
        assert!(!rows[0].split_once('│').unwrap().0.trim().is_empty());
        assert!(!rows[0].split_once('│').unwrap().1.trim().is_empty());
        assert!(rows[2].contains("██ Codex") && rows[2].contains("██ Claude"));
        let (left, right) = rows[4].split_once('│').unwrap();
        assert_eq!(left.contains("no sessions found"), !codex);
        assert_eq!(right.contains("no saved limits found"), !claude);
        let rows = frame(&mut usage, 36, 12);
        assert_eq!(rows[5], "─".repeat(36));
        assert!(!rows[0].is_empty() && !rows[6].is_empty());
        assert!(rows[2].contains("██ Codex") && rows[8].contains("██ Claude"));
        assert_eq!(rows[4].contains("no sessions found"), !codex);
        assert_eq!(rows[10].contains("no saved limits found"), !claude);
    }
}

#[test]
fn wide_claude_panes_show_a_model_week_without_displacing_account_limits() {
    let f = Fixture::folder();
    let mut usage = open(&f, NOW);
    let window = |minutes, percent| Window { minutes, percent, resets: Some(NOW + HOUR), observed: NOW };
    usage.claude.native = Some(Reading {
        at: Timestamp::seconds(NOW),
        windows: Limits {
            base: [Some(window(300, 12.0)), Some(window(10_080, 34.0))],
            model_week: ModelWeek::new("opus", window(10_080, 56.0)),
        },
    });
    for (width, model) in [(80, false), (140, true)] {
        let rows = frame(&mut usage, width, 3);
        let claude = |row: usize| rows[row].split_once('│').unwrap().1;
        assert!(claude(0).contains("5h") && claude(0).contains("week"));
        assert!(claude(1).contains("88%") && claude(1).contains("66%"));
        assert_eq!(claude(0).contains("Opus week"), model);
        assert_eq!(claude(1).contains("44%"), model);
    }

    let mut screen = Screen::sized(140, 5);
    screen.set_pictures(true);
    usage.draw(&mut screen);
    // The right panel has three figure pictures and one mark.
    assert_eq!(screen.picture_placements().filter(|(x, ..)| *x > 69).count(), 4);
}

#[test]
fn a_narrow_column_has_one_large_figure_and_a_row_of_words_for_each_window() {
    let f = Fixture::folder();
    let mut usage = open(&f, NOW).with_large_figures(true);
    let window = |percent| Window { minutes: 10_080, percent, resets: Some(NOW + DAY), observed: NOW };
    usage.claude.native = Some(Reading {
        at: Timestamp::seconds(NOW),
        windows: Limits { base: [None, Some(window(31.0))], model_week: ModelWeek::new("fable", window(58.0)) },
    });
    // Claude has the six rows below the rule, and needs five of them.
    let rows = frame(&mut usage, 24, 13);
    assert!(rows[9].ends_with("▜▄▛ ▄▄▛%"), "{}", rows[9]);
    assert_eq!(rows[10].trim(), "of week left · 1 d");
    assert_eq!(rows[11].trim(), "Fable week 42% left");
    assert!(rows[12].is_empty());
}

#[test]
fn a_tiny_frame_keeps_an_intact_figure_for_the_tightest_limit() {
    let f = with_limits();
    let mut usage = open(&f, NOW);
    assert_eq!(frame(&mut usage, 6, 1), ["Cl 39%"]);
    assert_eq!(frame(&mut usage, 12, 2), ["Codex 77%", "Claude 39%"]);
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
    let mut usage = open(&f, now).with_large_figures(true);
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
    assert_eq!(claude, "no saved limits found · 13k tokens today");
    // With a row to spare it has that row to itself.
    let rows = frame(&mut open(&f, now), 140, 6);
    let below: Vec<&str> = rows[4..].iter().map(|row| row.split_once('│').unwrap().1.trim()).collect();
    assert_eq!(below, ["no saved limits found · Run milo setup", "13k tokens today"]);
}

#[test]
fn an_old_reading_says_so_once_and_each_window_keeps_its_reset_time() {
    let now = local_noon();
    let f = Fixture::folder();
    let then = now - 6 * HOUR + 5 * MINUTE;
    example_limits(&f, Some(then), None, now + 2 * HOUR, now);
    let mut usage = open(&f, now);
    let wide = frame(&mut usage, 140, 5)[..5]
        .iter()
        .map(|line| line.split_once('│').unwrap().0)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(wide.matches("5 h 55 m ago").count(), 1, "{wide}");
    assert_eq!(wide.matches("last reported").count(), 2, "{wide}");
    assert!(wide.contains("2 h") && wide.contains("2 d"), "{wide}");

    let narrow = frame(&mut usage, 36, 12)[..5].join("\n");
    assert!(narrow.contains("week · 26% left") && narrow.contains("Last reported 5h55m ago"), "{narrow}");
    assert!(narrow.contains("Resets in 2d") && narrow.contains("Open Codex"), "{narrow}");
}

#[test]
fn a_millisecond_reset_has_the_same_countdown_as_seconds() {
    let f = Fixture::folder();
    let reset = NOW + 2 * HOUR;
    example_limits(&f, Some(NOW), Some(NOW), reset * 1000, NOW);
    let mut usage = open(&f, NOW);
    assert_eq!(frame(&mut usage, 140, 3)[2].matches("resets in 2 h").count(), 2);
    usage.fixed_now = Some(reset);
    let rows = frame(&mut usage, 140, 3);
    assert!(!rows[1].contains("100%"));
    assert_eq!(rows[2].matches("Reset elapsed").count(), 2);
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
fn health_of_an_omp_report_follows_its_own_cadence() {
    let f = Fixture::folder();
    let reset = NOW + HOUR;
    write(&f, CODEX, &codex(NOW, 80, reset, "null"), NOW);
    let mut usage = open(&f, NOW + 10 * MINUTE - 1);
    usage.claude.omp = Some(Reading {
        at: Timestamp::seconds(NOW + 30),
        windows: Limits::new([Some(Window { minutes: 300, percent: 50.0, resets: Some(reset), observed: NOW }), None]),
    });
    assert_eq!(usage.next_frame(), Some(Duration::from_secs(1)));
    let rows = frame(&mut usage, 140, 3);
    let (codex, claude) = rows[0].split_once('│').unwrap();
    assert!(codex.contains("last reported") && !claude.contains("last reported"));

    usage.fixed_now = Some(NOW + 10 * MINUTE);
    let rows = frame(&mut usage, 140, 3);
    let (_, claude) = rows[2].split_once('│').unwrap();
    assert!(claude.contains("10 m ago"));
}

#[test]
fn a_lone_figure_has_room_for_its_reset_caption() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 23, NOW + 2 * HOUR + 10 * MINUTE, "null"), NOW);
    let rows = frame(&mut open(&f, NOW).with_large_figures(true), 28, 12);
    assert_eq!(rows[3].trim(), "of week left · 2 h 10 m");
}

#[test]
fn the_key_hints_fit_below_the_figures_where_there_is_room() {
    let f = with_limits();
    let mut usage = open(&f, NOW);
    for (w, h) in [(80, 24), (28, 20)] {
        assert_eq!(frame(&mut usage, w, h)[h - 1], " r reload · q quit · ? help");
    }
    assert_eq!(frame(&mut usage, 10, 20)[19], " ? help");
    assert!(!frame(&mut usage, 140, 5)[4].contains("? help"));
}

#[test]
fn a_two_row_pane_shows_both_providers_and_their_tightest_remaining_capacity() {
    let f = Fixture::folder();
    example_limits(&f, Some(NOW), Some(NOW), NOW + 38 * MINUTE, NOW);
    let mut usage = open(&f, NOW).with_large_figures(true);
    for width in [22, 100] {
        let mut screen = Screen::sized(width, 2);
        screen.set_pictures(true);
        usage.draw(&mut screen);
        assert!(screen.picture_at().is_none());
        assert_eq!(
            screen.text().iter().map(|row| row.trim()).collect::<Vec<_>>(),
            ["Codex week · 26% left", "Claude week · 8% left"],
        );
    }
}

#[test]
fn a_taller_rail_adds_reset_times_and_keeps_the_tightest_window_for_each_tool() {
    let f = Fixture::folder();
    example_limits(&f, Some(NOW), Some(NOW), NOW + 38 * MINUTE, NOW);
    let mut usage = open(&f, NOW);
    let mut screen = Screen::sized(22, 10);
    screen.set_pictures(true);
    usage.draw(&mut screen);
    assert!(screen.picture_at().is_none());
    let rows = screen.text();
    assert_eq!(rows[0], "Codex week · 26% left");
    assert_eq!(rows[1], "Resets in 2d");
    assert_eq!(rows[5], "Claude week · 8% left");
    assert_eq!(rows[6], "Resets in 2d");
    assert!(!rows.join("\n").contains("73%"));

    let five_hour = codex_window(88, 300, NOW + 38 * MINUTE);
    let week = codex_window(20, 10_080, NOW + DAY);
    write(&f, CODEX, &codex_snapshot(&format!("{}Z", iso(NOW)), &five_hour, &week), NOW + 1);
    assert!(usage.check());
    let rows = frame(&mut usage, 21, 4);
    assert_eq!(&rows[..2], ["Codex 5h · 12% left", "Resets in 38m"]);
}

#[test]
fn elapsed_and_missing_limits_give_different_next_steps_without_guessing_percentages() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 80, NOW + MINUTE, "null"), NOW);
    let mut usage = open(&f, NOW + MINUTE);
    let rows = frame(&mut usage, 22, 6);
    assert_eq!(rows[0], "Codex · reset elapsed");
    assert_eq!(rows[1], "Open Codex");
    assert_eq!(rows[3], "Claude · no source");
    assert_eq!(rows[4], "Run milo setup");
    assert!(!rows.join("\n").contains('%'));

    let exhausted = codex_window(100, 300, NOW + MINUTE);
    let active = codex_window(20, 10_080, NOW + DAY);
    write(&f, CODEX, &codex_snapshot(&format!("{}Z", iso(NOW)), &exhausted, &active), NOW);
    assert!(usage.check());
    assert_eq!(frame(&mut usage, 22, 6)[0], "Codex week · 80% left");
}

#[test]
fn a_short_stale_summary_is_labeled_even_without_a_spare_line() {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW, 80, NOW + DAY, "null"), NOW);
    let mut usage = open(&f, NOW + 5 * MINUTE - 1);
    assert_eq!(usage.next_frame(), Some(Duration::from_secs(1)));
    assert_eq!(frame(&mut usage, 22, 1), ["Codex week · 20% left"]);
    usage.fixed_now = Some(NOW + 5 * MINUTE);
    assert_eq!(frame(&mut usage, 22, 1), ["Codex week · 20% last"]);
    assert_eq!(frame(&mut usage, 12, 6)[0], "Cx 20% last");
    assert!(!frame(&mut usage, 6, 1)[0].contains('%'));
}
