//! Daily Claude token totals across files, appends and rewrites.

use super::*;

#[test]
fn claude_counts_only_regular_subagent_transcripts_in_the_session_subagents_folder() {
    let f = Fixture::folder();
    let now = local_noon();
    write(&f, AGENT, &assistant(now, 2000, 0, 0), now);
    let decoy = assistant(now, 9000, 0, 0);
    for path in [
        "claude/project/session/agent-wrong-level.jsonl",
        "claude/project/session/subagents/not-ours.jsonl",
        "claude/project/session/subagents/agent-compressed.jsonl.zst",
        "claude/project/session/subagents/nested/agent-wrong-level.jsonl",
        "claude/project/session/other/agent-wrong-folder.jsonl",
        "elsewhere/subagents/agent-symlink.jsonl",
    ] {
        write(&f, path, &decoy, now);
    }
    fs::create_dir_all(f.root.join("claude/project/other")).unwrap();
    symlink(f.root.join("elsewhere"), f.root.join("claude/project/linked")).unwrap();
    symlink(f.root.join("elsewhere/subagents"), f.root.join("claude/project/other/subagents")).unwrap();
    symlink(
        f.root.join("elsewhere/subagents/agent-symlink.jsonl"),
        f.root.join("claude/project/session/subagents/agent-link.jsonl"),
    )
    .unwrap();
    assert_eq!(activity(&open(&f, now)).today, 2000);
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
    let sample = |at, tokens| identified(&assistant(at, tokens, 0, 0), "example-message");
    write(&f, CLAUDE, &format!("{}{}", sample(now - 20, 1000), sample(now - 10, 2000)), now - 10);
    let mut usage = open(&f, now);
    assert_eq!(activity(&usage).today, 2000);

    let mut file = OpenOptions::new().append(true).open(f.root.join(CLAUDE)).unwrap();
    file.write_all(sample(now, 3000).as_bytes()).unwrap();
    file.set_modified(mtime(now)).unwrap();
    assert!(usage.check());
    assert_eq!(activity(&usage).today, 3000);
}

#[test]
fn resumed_and_forked_files_share_message_ids_but_not_anonymous_records() {
    let f = Fixture::folder();
    let now = local_noon();
    let anon = assistant(now - 5, 200, 0, 0);
    let older = identified(&assistant(now - 20, 2000, 0, 0), "shared");
    let updated = identified(&assistant(now - 10, 4000, 0, 0), "shared");
    let pending = identified(&assistant(now - 1, 5000, 0, 0), "shared");
    write(&f, CLAUDE, &format!("{older}{anon}"), now - 5);
    write(&f, "claude/fork/session.jsonl", &format!("{updated}{anon}"), now - 5);
    write(&f, AGENT, pending.trim_end(), now);
    let mut usage = open(&f, now);
    assert_eq!(activity(&usage).today, 5400);
    fs::remove_file(f.root.join(AGENT)).unwrap();
    assert!(usage.check());
    assert_eq!(activity(&usage).today, 4400);
}

#[test]
fn a_file_caps_daily_samples_and_marks_the_total_as_partial() {
    let f = Fixture::folder();
    let now = local_noon();
    let records = assistant(now, 1, 0, 0).repeat(MAX_SAMPLES + 1);
    write(&f, CLAUDE, &records, now);
    let usage = open(&f, now);
    assert_eq!(activity(&usage).today, MAX_SAMPLES as u64);
    assert!(activity(&usage).partial);
}

#[test]
fn first_load_skips_old_blocks_but_keeps_todays_long_unterminated_tail() {
    let f = Fixture::folder();
    let now = local_noon();
    let today = Today::at(now, when::local_day(now)).since;
    let old = assistant(today - 1, 9000, 0, 0).repeat(12_000);
    let first = assistant(today, 1000, 0, 0);
    let last = assistant(now, 2000, 0, 0).replacen('{', &format!("{{\"padding\":\"{}\",", "x".repeat(40_000)), 1);
    write(&f, CLAUDE, &format!("{old}{first}{}", last.trim_end()), now);
    let mut usage = open(&f, now);
    assert!(usage.bytes_read < 128 * 1024, "{} bytes read", usage.bytes_read);
    assert_eq!(activity(&usage).today, 3000);
    assert!(!activity(&usage).partial);

    let mut file = OpenOptions::new().append(true).open(f.root.join(CLAUDE)).unwrap();
    file.write_all(b"\n").unwrap();
    file.set_modified(mtime(now + 1)).unwrap();
    assert!(usage.check());
    assert_eq!(usage.bytes_read, 1);
    assert_eq!(activity(&usage).today, 3000);
}

#[test]
fn a_budget_limited_first_load_resumes_backwards_then_reads_new_appends() {
    let f = Fixture::folder();
    let now = local_noon();
    let today = Today::at(now, when::local_day(now)).since;
    let first = assistant(now - 2, 1000, 0, 0);
    let second = assistant(now - 1, 2000, 0, 0);
    write(&f, CLAUDE, &format!("{first}{second}"), now - 1);
    let mut claude = Claude::default();
    let root = f.root.join("claude");
    assert!(!claude.check(Some(&root), today, &mut Budget::new(second.len() as u64 + 3)));
    assert_eq!(claude.activity(today, now).today, 2000);
    assert!(claude.activity(today, now).partial);

    let append = assistant(now, 4000, 0, 0);
    let mut file = OpenOptions::new().append(true).open(f.root.join(CLAUDE)).unwrap();
    file.write_all(append.as_bytes()).unwrap();
    file.set_modified(mtime(now)).unwrap();
    let mut budget = Budget::new(4096);
    assert!(claude.check(Some(&root), today, &mut budget));
    assert_eq!(budget.read, (first.len() - 3 + append.len()) as u64);
    assert_eq!(claude.activity(today, now).today, 7000);
    assert!(!claude.activity(today, now).partial);
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
fn a_same_size_claude_rewrite_replaces_the_previous_total() {
    let f = Fixture::folder();
    let now = local_noon();
    let initial = assistant(now - 2, 1000, 0, 0);
    let rewritten = assistant(now - 1, 3000, 0, 0);
    assert_eq!(initial.len(), rewritten.len());
    write(&f, CLAUDE, &initial, now - 2);
    let mut usage = open(&f, now);
    assert!(caption(&mut usage).contains("1k tokens today"));

    write(&f, CLAUDE, &rewritten, now - 1);
    assert!(usage.check());
    let row = caption(&mut usage);
    assert!(row.contains("3k tokens today") && !row.contains("1k tokens today"), "{row}");
}
