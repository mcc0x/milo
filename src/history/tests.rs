use crate::fixture::Fixture;
use crate::keys::DOUBLE_CLICK;

use super::*;

fn open(f: &Fixture) -> History {
    History::new(Repo::open(&f.root).unwrap(), false)
}

fn draw(history: &mut History, w: usize, h: usize) -> Screen {
    let mut screen = Screen::sized(w, h);
    history.draw(&mut screen);
    screen
}

fn row<'a>(rows: &'a [String], text: &str) -> &'a str {
    rows.iter().find(|row| row.contains(text)).expect(text)
}

fn commit(f: &Fixture, subject: &str) {
    f.git(&["commit", "-q", "--allow-empty", "-m", subject]);
}

fn active_row(screen: &Screen, name: &str) -> bool {
    screen.text().iter().enumerate().any(|(y, row)| row.contains(name) && screen.bar_at(y))
}

fn read_next(history: &mut History, name: &str) -> Vec<String> {
    assert_eq!(history.key(Key::Down), After::Stay);
    assert!(active_row(&draw(history, 65, 12), name), "{name} is not selected");
    assert_eq!(history.key(Key::Enter), After::Stay);
    draw(history, 65, 12).text()
}

#[test]
fn the_heading_names_the_project_branch_and_commits_to_push() {
    let f = Fixture::new();
    commit(&f, "first");
    let mut history = open(&f);
    let heading = &draw(&mut history, 65, 8).text()[0];
    assert!(heading.starts_with(" project  main") && heading.ends_with("no remote"), "{heading}");
    assert_eq!(heading, &format!(" project  main{}no remote", " ".repeat(41)));
    f.push();
    assert!(history.check());
    let heading = &draw(&mut history, 65, 8).text()[0];
    assert!(heading.ends_with("✓ all pushed"), "{heading}");
    commit(&f, "second");
    assert!(history.check());
    let heading = &draw(&mut history, 65, 8).text()[0];
    assert!(heading.ends_with("1 commit to push"), "{heading}");
    commit(&f, "third");
    assert!(history.check());
    let heading = &draw(&mut history, 65, 8).text()[0];
    assert!(heading.ends_with("2 commits to push"), "{heading}");
}

#[test]
fn an_all_pushed_heading_uses_a_check_and_a_narrow_count_still_names_commits() {
    let f = Fixture::new();
    commit(&f, "first");
    f.push();
    let mut history = open(&f);
    assert_eq!(draw(&mut history, 65, 8).text()[0], format!(" project  main{}✓ all pushed", " ".repeat(38)));
    assert_eq!(draw(&mut history, 22, 8).text()[0], " project  main      ✓");
    for i in 1..=3 {
        commit(&f, &format!("local {i}"));
    }
    assert!(history.check());
    assert_eq!(draw(&mut history, 65, 8).text()[0], format!(" project  main{}3 commits to push", " ".repeat(33)));
    assert_eq!(draw(&mut history, 30, 8).text()[0], " project  main    3 commits ↑");
    assert_eq!(draw(&mut history, 22, 8).text()[0], " project  main     ↑3");
}

#[test]
fn the_heading_identifies_a_branch_without_a_configured_push_comparison() {
    let f = Fixture::new();
    commit(&f, "first");
    f.git(&["switch", "-q", "-c", "feature"]);
    commit(&f, "feature work");
    let mut history = open(&f);
    assert!(draw(&mut history, 50, 12).text()[0].ends_with("no remote"));
    f.git(&["remote", "add", "backup", "../missing"]);
    assert!(history.check());
    let heading = &draw(&mut history, 50, 12).text()[0];
    assert!(heading.starts_with(" project  feature") && heading.ends_with("no branch tracked"), "{heading}");
    assert_eq!(heading, &format!(" project  feature{}no branch tracked", " ".repeat(15)));
    f.git(&["remote", "remove", "backup"]);
    assert!(history.check());
    assert!(draw(&mut history, 50, 12).text()[0].ends_with("no remote"));
    f.push();
    f.git(&["push", "-q", "origin", "feature:main"]);
    f.git(&["remote", "set-head", "origin", "main"]);
    let mut history = open(&f);
    assert!(draw(&mut history, 50, 12).text()[0].ends_with("no branch tracked"));
    f.git(&["push", "-q", "-u", "origin", "feature"]);
    assert!(history.check());
    assert!(draw(&mut history, 50, 12).text()[0].ends_with("✓ all pushed"));
}

#[test]
fn uncommitted_then_unpushed_then_the_remote_boundary() {
    let f = Fixture::new();
    commit(&f, "oldest");
    f.push();
    commit(&f, "middle");
    commit(&f, "newest");
    f.write("one", "uncommitted\n");
    f.write("two", "uncommitted\n");
    let mut history = open(&f);
    let rows = draw(&mut history, 65, 12).text();
    assert_eq!(rows[1], " ○ ● ● │ ●");
    assert_eq!(rows[2], " ○ 2 files to commit");
    assert!(rows[3].starts_with(" ● newest"), "{rows:?}");
    assert_eq!(rows[4], " ● middle");
    assert_eq!(rows[5], format!(" ── on origin/main {}", "─".repeat(45)));
    assert_eq!(rows[6], " ● oldest");
    assert!(rows[11].contains("Enter show files") && rows[11].contains("? help"));
}

#[test]
fn a_merge_shows_one_remote_boundary_after_the_last_local_commit() {
    let f = Fixture::new();
    commit(&f, "shared base");
    f.git(&["branch", "local"]);
    let dated = |day: &str, subject: &str| {
        let mut git = std::process::Command::new("git");
        crate::fixture::isolate_git(&mut git);
        assert!(
            git.current_dir(&f.root)
                .env("GIT_COMMITTER_DATE", day)
                .args(["commit", "-q", "--allow-empty", "-m", subject])
                .status()
                .unwrap()
                .success()
        );
    };
    dated("2001-02-05T00:00:00Z", "remote work");
    f.push();
    f.git(&["switch", "-q", "local"]);
    f.git(&["branch", "--set-upstream-to=origin/main"]);
    dated("2001-02-04T00:00:00Z", "local work");
    f.git(&["merge", "-q", "--no-ff", "-m", "merge upstream", "main"]);
    let rows = draw(&mut open(&f), 65, 12).text();
    let position = |text: &str| rows.iter().position(|r| r.contains(text)).unwrap();
    assert!(position("remote work") < position("local work"), "{rows:?}");
    assert!(position("local work") < position("── on origin/main"), "{rows:?}");
    assert!(position("── on origin/main") < position("shared base"), "{rows:?}");
    assert_eq!(rows[1].matches('│').count(), 1, "{rows:?}");
}

#[test]
fn the_remote_boundary_is_a_rule_and_does_not_take_the_cursor() {
    let f = Fixture::new();
    commit(&f, "on main");
    f.push();
    commit(&f, "still here");
    let mut history = open(&f);
    let screen = draw(&mut history, 30, 12);
    assert_eq!(screen.text()[2], " ── on origin/main ──────────");
    history.key(Key::Down);
    assert!(active_row(&draw(&mut history, 30, 12), "on main"));
}

#[test]
fn clicks_toggle_commits_and_only_a_quick_repeat_reads_the_same_file() {
    let f = Fixture::new();
    for i in 0..6 {
        commit(&f, &format!("older {i}"));
    }
    for name in ["a", "b", "c"] {
        f.write(&format!("{name}.txt"), &format!("stored {name}\n"));
    }
    f.commit("write three files");
    let mut history = open(&f);
    draw(&mut history, 40, 5);
    history.key(Key::Click(5, 1));
    let screen = draw(&mut history, 40, 5);
    assert!(active_row(&screen, "write three files") && screen.text()[1].contains("▾"));
    assert!(screen.text()[4].contains("c.txt"));

    let now = Instant::now();
    let step = std::time::Duration::from_millis(1);
    history.input(Key::Click(5, 3), now);
    history.input(Key::Click(5, 4), now + step);
    let selected = draw(&mut history, 40, 5);
    assert!(selected.text()[4].contains("c.txt") && selected.bar_at(4));
    history.input(Key::Click(5, 4), now + step + DOUBLE_CLICK);
    assert!(active_row(&draw(&mut history, 40, 5), "c.txt"));
    history.input(Key::Click(5, 4), now + 2 * step + DOUBLE_CLICK);
    assert_eq!(draw(&mut history, 40, 5).text()[1], " 1 + stored c");

    history.key(Key::Esc);
    let before = draw(&mut history, 40, 5).text();
    history.key(Key::WheelDown);
    let after = draw(&mut history, 40, 5);
    assert_ne!(after.text(), before);
    assert!(active_row(&after, "older 3"));
    history.key(Key::Home);
    draw(&mut history, 40, 5);
    history.key(Key::Click(5, 1));
    let closed = draw(&mut history, 40, 5);
    assert!(active_row(&closed, "write three files"));
    assert!(!closed.text().iter().any(|row| row.contains("c.txt")));
    history.key(Key::WheelDown);
    let scrolled = draw(&mut history, 40, 5);
    assert_ne!(scrolled.text()[1], closed.text()[1]);
    assert!(active_row(&scrolled, "older 3"));
}

#[test]
fn opening_a_long_commit_in_a_narrow_pane_keeps_the_subject_and_previews_eight_names() {
    let f = Fixture::new();
    commit(&f, "older commit");
    f.push();
    for i in 0..40 {
        f.write(&format!("long/shared/folder/path/page{i:02}.txt"), "a file\n");
    }
    f.commit("touch forty files");
    for i in 1..=8 {
        commit(&f, &format!("newer {i}"));
    }
    let mut history = open(&f);
    draw(&mut history, 30, 12);
    history.key(Key::PgDn);
    assert!(active_row(&draw(&mut history, 30, 12), "older commit"));
    history.key(Key::Up);
    history.key(Key::Enter);
    let preview = draw(&mut history, 30, 12);
    let rows = preview.text();
    assert!(preview.bar_at(1) && rows[1].contains("touch forty files") && rows[1].ends_with("40 files"), "{rows:?}");
    for (i, row) in rows[2..10].iter().enumerate() {
        assert!(
            row.contains(&format!("page{i:02}.txt lon")) && row.trim_end_matches('▐').ends_with("new +1"),
            "{rows:?}"
        );
    }
    assert!(rows[10].contains("… 32 more"), "{rows:?}");
    assert_eq!(rows[11], " Enter hide files · ? help");

    history.key(Key::End);
    history.key(Key::Up);
    let more = draw(&mut history, 30, 12);
    assert!(active_row(&more, "… 32 more"));
    assert_eq!(more.text()[11], " Enter show all · ? help");
    history.key(Key::Enter);
    assert!(active_row(&draw(&mut history, 30, 12), "page08.txt"));
    history.key(Key::End);
    history.key(Key::Up);
    assert!(active_row(&draw(&mut history, 30, 12), "page39.txt"));
    commit(&f, "a commit arrived");
    assert!(history.check());
    assert!(active_row(&draw(&mut history, 30, 12), "page39.txt"));
    history.key(Key::Left);
    history.key(Key::Enter);
    history.key(Key::Enter);
    assert!(draw(&mut history, 30, 12).text()[10].contains("… 32 more"));
}

#[test]
fn enter_hints_follow_the_selected_row_and_disappear_where_enter_does_nothing() {
    let f = Fixture::new();
    let mut history = open(&f);
    let rows = draw(&mut history, 65, 12).text();
    assert!(rows[1].contains("No commits yet.") && !rows[11].contains("Enter"));
    f.write("notes.txt", "some notes\n");
    assert!(history.check());
    let uncommitted = draw(&mut history, 65, 12).text();
    assert!(uncommitted[1].contains("1 file to commit") && !uncommitted[11].contains("Enter"));
    history.key(Key::Enter);
    assert_eq!(draw(&mut history, 65, 12).text(), uncommitted);
    f.commit("add notes");
    assert!(history.check());
    assert_eq!(draw(&mut history, 30, 12).text()[11], " Enter show files · ? help");
    history.key(Key::Enter);
    assert_eq!(draw(&mut history, 30, 12).text()[11], " Enter hide files · ? help");
    history.key(Key::Down);
    assert_eq!(draw(&mut history, 30, 12).text()[11], " Enter read · q quit · ? help");
}

#[test]
fn enter_shows_the_files_and_counts_and_then_hides_them() {
    let f = Fixture::new();
    f.write("edit.txt", "old\n");
    f.write("remove.txt", "delete\nme\n");
    f.write("move.txt", "move\n");
    f.commit("original");
    f.write("add.txt", "a\nb\nc\n");
    f.write("edit.txt", "new\nsome more\n");
    std::fs::remove_file(f.root.join("remove.txt")).unwrap();
    std::fs::rename(f.root.join("move.txt"), f.root.join("moved.txt")).unwrap();
    f.commit("touch four files");
    let mut history = open(&f);
    let before = draw(&mut history, 65, 12).text();
    history.key(Key::Enter);
    let rows = draw(&mut history, 65, 12).text();
    assert!(row(&rows, "touch four files").contains("▾"));
    assert!(row(&rows, "add.txt").contains("new") && row(&rows, "add.txt").ends_with("+3"), "{rows:?}");
    assert!(row(&rows, "edit.txt").contains("edited") && row(&rows, "edit.txt").ends_with("+2 −1"), "{rows:?}");
    assert!(row(&rows, "moved.txt").ends_with("moved"), "{rows:?}");
    assert!(row(&rows, "remove.txt").contains("removed") && row(&rows, "remove.txt").ends_with("−2"), "{rows:?}");
    history.key(Key::Enter);
    assert_eq!(draw(&mut history, 65, 12).text(), before);
    history.key(Key::Enter);
    assert_eq!(draw(&mut history, 65, 12).text(), rows);
    history.key(Key::End);
    history.key(Key::Enter);
    let root = draw(&mut history, 65, 12).text();
    assert!(row(&root, "remove.txt").contains("new") && row(&root, "remove.txt").ends_with("+2"), "{root:?}");
}

#[test]
fn reading_a_file_shows_its_commit_and_returns_to_the_same_row() {
    let f = Fixture::new();
    f.write("page.rs", "fn main() {\n    let n = 1;\n}\n");
    f.commit("original");
    f.write("page.rs", "fn main() {\n    let n = 2;\n}\n");
    f.commit("change the number");
    f.write("page.rs", "uncommitted work\n");

    let mut history = open(&f);
    let id = format!("commit {}", &history.entries[0].commit.hash[..7]);
    history.key(Key::Enter);
    let rows = read_next(&mut history, "page.rs");
    assert!(rows[0].contains("page.rs") && rows[0].contains(&id) && rows[0].ends_with("ago"), "{rows:?}");
    assert_eq!(&rows[1..5], [" 1   fn main() {", "   -     let n = 1;", " 2 +     let n = 2;", " 3   }"]);
    assert!(!rows.iter().any(|row| row.contains("uncommitted work")));
    assert!(rows[11].contains("n N changes") && !rows[11].contains("reload"));
    let mut help = Screen::sized(40, 20);
    history.help().draw(&mut help);
    let help = help.text();
    assert!(help.iter().any(|row| row.contains("As of a commit")));
    assert!(!help.iter().any(|row| row.contains("reload")));

    f.write("page.rs", "another edit\n");
    history.key(Key::Char('r'));
    assert!(history.check());
    assert_eq!(draw(&mut history, 65, 12).text(), rows);

    assert_eq!(history.key(Key::Char('q')), After::Stay);
    let screen = draw(&mut history, 65, 12);
    assert!(active_row(&screen, "page.rs"));
    assert!(row(&screen.text(), "change the number").contains("▾"));
    assert!(screen.text()[11].contains("Enter read"));
    history.key(Key::Enter);
    assert_eq!(history.key(Key::Esc), After::Stay);
    assert!(active_row(&draw(&mut history, 65, 12), "page.rs"));

    history.key(Key::Left);
    let screen = draw(&mut history, 65, 12);
    assert!(active_row(&screen, "change the number"));
    assert!(row(&screen.text(), "change the number").contains("▾"));
    assert!(screen.text()[11].contains("Enter hide files"));
    history.key(Key::Left);
    let rows = draw(&mut history, 65, 12).text();
    assert!(!row(&rows, "change the number").contains("▾"));
    assert!(!rows.iter().any(|row| row.contains("page.rs")));
}

#[test]
fn closing_the_standalone_reader_picks_up_changes_and_keeps_its_file_selected() {
    let f = Fixture::new();
    f.write("page.txt", "historical content\n");
    f.commit("read this commit");
    let mut history = open(&f);
    history.key(Key::Enter);
    let reading = read_next(&mut history, "page.txt");

    commit(&f, "arrived while reading");
    f.write("pending.txt", "uncommitted work\n");
    assert!(history.check());
    assert_eq!(draw(&mut history, 65, 12).text(), reading);

    assert_eq!(history.key(Key::Esc), After::Stay);
    let screen = draw(&mut history, 65, 12);
    let rows = screen.text();
    assert!(rows.iter().any(|line| line.contains("1 file to commit")), "{rows:?}");
    assert!(row(&rows, "arrived while reading").starts_with('•'));
    assert!(row(&rows, "read this commit").contains("▾"));
    assert!(active_row(&screen, "page.txt"));
}

#[test]
fn a_removed_file_and_moves_read_from_the_right_paths() {
    let f = Fixture::new();
    let start = "first retained line\nsecond retained line\nthird retained line\n";
    f.write("gone.txt", "a lost line\n");
    f.write("old.txt", &format!("{start}before\n"));
    f.write("unchanged.txt", "an unchanged line\n");
    f.commit("original");
    std::fs::remove_file(f.root.join("gone.txt")).unwrap();
    std::fs::rename(f.root.join("old.txt"), f.root.join("moved.txt")).unwrap();
    std::fs::rename(f.root.join("unchanged.txt"), f.root.join("same.txt")).unwrap();
    f.write("moved.txt", &format!("{start}after\n"));
    f.commit("moves and a removal");
    f.write("gone.txt", "a new file with the same name\n");
    f.write("same.txt", "edited after the move\n");

    let mut history = open(&f);
    history.key(Key::Enter);
    let rows = read_next(&mut history, "gone.txt");
    assert_eq!(rows[1], "   - a lost line");
    assert!(!rows.iter().any(|row| row.contains("a new file")));
    history.key(Key::Esc);
    let rows = read_next(&mut history, "moved.txt");
    assert_eq!(
        &rows[1..6],
        [
            " 1   first retained line",
            " 2   second retained line",
            " 3   third retained line",
            "   - before",
            " 4 + after"
        ]
    );
    history.key(Key::Esc);
    let rows = read_next(&mut history, "same.txt");
    assert_eq!(rows[1], " 1   an unchanged line");
    assert!(!rows.iter().any(|row| row.contains("edited after the move")));
}

#[test]
fn a_file_in_the_first_commit_has_only_additions() {
    let f = Fixture::new();
    f.write("first.txt", "first line\nsecond line\n");
    f.commit("start");
    let mut history = open(&f);
    history.key(Key::Enter);
    let rows = read_next(&mut history, "first.txt");
    assert_eq!(&rows[1..3], [" 1 + first line", " 2 + second line"]);
}

#[test]
fn a_shallow_boundary_does_not_claim_unchanged_files_were_added() {
    let f = Fixture::new();
    f.write("old.txt", "already there\n");
    f.commit("first");
    f.write("new.txt", "just added\n");
    f.commit("second");
    f.git(&["clone", "-q", "--depth=1", &format!("file://{}", f.root.display()), "shallow"]);
    let mut history = History::new(Repo::open(&f.root.join("shallow")).unwrap(), false);
    history.key(Key::Enter);
    let rows = draw(&mut history, 65, 12).text();
    assert!(row(&rows, "second").contains("▾"));
    assert!(rows.iter().any(|row| row.contains("Git did not answer for this commit.")), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("old.txt")), "{rows:?}");

    f.git(&["-C", "shallow", "fetch", "--unshallow", "origin"]);
    assert!(history.check());
    let rows = draw(&mut history, 65, 12).text();
    assert!(row(&rows, "second").contains("▾"));
    assert!(!row(&rows, "first").starts_with('•'), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("new.txt")), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("old.txt")), "{rows:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_commit_with_unrepresentable_names_reports_a_partial_file_list() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let f = Fixture::new();
    f.write("page\u{fffd}.txt", "valid path\n");
    std::fs::write(f.root.join(OsStr::from_bytes(b"page\xff.txt")), "raw path\n").unwrap();
    f.commit("two files");

    let mut history = open(&f);
    history.key(Key::Enter);
    let rows = draw(&mut history, 65, 12).text();
    assert!(row(&rows, "two files").ends_with("2 files"), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("Some file names cannot be shown.")), "{rows:?}");
    assert_eq!(rows.iter().filter(|r| r.contains("page\u{fffd}.txt")).count(), 1);
    assert_eq!(read_next(&mut history, "page\u{fffd}.txt")[1], " 1 + valid path");
}

#[test]
fn opening_a_merge_at_the_bottom_shows_its_first_file() {
    let f = Fixture::new();
    commit(&f, "base");
    f.git(&["branch", "topic"]);
    f.write("main.txt", "main\n");
    f.commit("main work");
    f.git(&["switch", "-q", "topic"]);
    f.write("topic.txt", "topic\n");
    f.commit("topic work");
    f.git(&["switch", "-q", "main"]);
    f.git(&["merge", "-q", "--no-ff", "-m", "merge topic", "topic"]);
    commit(&f, "later");
    let mut history = open(&f);
    draw(&mut history, 40, 2);
    history.key(Key::Down);
    history.key(Key::Enter);
    let rows = draw(&mut history, 40, 2).text();
    assert!(rows[0].contains("merge topic"), "{rows:?}");
    assert!(rows[1].contains("topic.txt") && rows[1].ends_with("+1"), "{rows:?}");

    let rows = read_next(&mut history, "topic.txt");
    assert_eq!(rows[1], " 1 + topic");
    assert!(!rows.iter().any(|row| row.contains("main work")));
}

#[test]
fn an_empty_commit_opened_at_the_edge_shows_why_it_has_no_files() {
    let f = Fixture::new();
    commit(&f, "first");
    commit(&f, "second");
    let mut history = open(&f);
    draw(&mut history, 40, 2);
    history.key(Key::Down);
    history.key(Key::Enter);
    let rows = draw(&mut history, 40, 2).text();
    assert!(rows[0].contains("first") && rows[1].contains("No files changed."), "{rows:?}");
}

#[test]
fn a_two_row_focused_pane_keeps_the_open_commit_preview_visible() {
    let f = Fixture::new();
    commit(&f, "first chapter second chapter final chapter");
    commit(&f, "newer");
    let mut history = open(&f);
    draw(&mut history, 21, 2);
    history.key(Key::Down);
    assert!(draw(&mut history, 21, 2).text()[1].contains("↳ first chapter"));

    history.key(Key::Enter);
    let screen = draw(&mut history, 21, 2);
    let rows = screen.text();
    assert!(screen.bar_at(0) && rows[0].contains("first chapter"), "{rows:?}");
    assert!(rows[1].contains("No files changed."), "{rows:?}");

    history.key(Key::Left);
    assert!(draw(&mut history, 21, 2).text()[1].contains("↳ first chapter"));
}

#[test]
fn a_subject_peek_keeps_the_preview_below_a_keyboard_opened_commit() {
    let f = Fixture::new();
    commit(&f, "first chapter second chapter final chapter");
    commit(&f, "newer");
    let mut history = open(&f);
    draw(&mut history, 25, 4);
    history.key(Key::Down);
    history.key(Key::Enter);
    let rows = draw(&mut history, 25, 4).text();
    assert!(rows[1].contains("first chapter") && rows[2].contains("No files changed."), "{rows:?}");
    assert!(rows[3].contains("↳"), "{rows:?}");
}

#[test]
fn the_newest_commit_preview_can_scroll_past_the_uncommitted_row() {
    let f = Fixture::new();
    commit(&f, "first");
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    f.commit("both files");
    f.write("pending.txt", "still editing\n");
    let mut history = open(&f);
    let rows = draw(&mut history, 40, 4).text();
    assert!(rows[1].contains("to commit"), "{rows:?}");
    history.key(Key::Enter);
    let rows = draw(&mut history, 40, 4).text();
    assert!(rows[1].contains("both files") && rows[3].contains("b.txt"), "{rows:?}");
}

#[test]
fn scrolling_keeps_the_cursor_on_screen() {
    let f = Fixture::new();
    for i in 1..=8 {
        commit(&f, &format!("commit {i}"));
    }
    let mut history = open(&f);
    for (key, subject) in [
        (Key::Home, "commit 8"),
        (Key::PgDn, "commit 6"),
        (Key::PgDn, "commit 4"),
        (Key::WheelDown, "commit 1"),
        (Key::Up, "commit 2"),
        (Key::Home, "commit 8"),
    ] {
        draw(&mut history, 40, 4);
        history.key(key);
        let screen = draw(&mut history, 40, 4);
        let rows = screen.text();
        let y = rows.iter().position(|row| row.contains(subject)).expect(subject);
        assert!(screen.bar_at(y), "{rows:?}");
    }
}

#[test]
fn switching_branches_does_not_mark_existing_commits_as_arrivals() {
    let f = Fixture::new();
    commit(&f, "original");
    let mut history = open(&f);
    f.git(&["switch", "--orphan", "other"]);
    commit(&f, "unrelated");
    for branch in ["other", "main"] {
        f.git(&["switch", branch]);
        history.check();
        let rows = draw(&mut history, 65, 8).text();
        let subject = if branch == "main" { "original" } else { "unrelated" };
        let row = rows.iter().find(|row| row.contains(subject)).unwrap();
        assert!(!row.starts_with('•'), "{row}");
    }
}

#[test]
fn moving_between_detached_commits_does_not_mark_arrivals() {
    let f = Fixture::new();
    commit(&f, "older");
    commit(&f, "already existed");
    f.git(&["switch", "--detach", "HEAD~1"]);
    let mut history = open(&f);
    f.git(&["switch", "--detach", "main"]);
    history.check();
    let rows = draw(&mut history, 65, 8).text();
    let row = rows.iter().find(|row| row.contains("already existed")).unwrap();
    assert!(!row.starts_with('•'), "{row}");
}

#[test]
fn a_commit_that_lands_while_open_appears_marked_on_check() {
    let f = Fixture::new();
    let mut history = open(&f);
    assert!(draw(&mut history, 65, 8).text().iter().any(|row| row.contains("No commits yet.")));
    commit(&f, "arrived");
    assert!(history.check());
    let screen = draw(&mut history, 65, 8);
    let rows = screen.text();
    let y = rows.iter().position(|row| row.contains("arrived")).unwrap();
    assert!(rows[y].starts_with("•● arrived"), "{rows:?}");
    assert!(screen.bar_at(y));
}

#[test]
fn short_and_narrow_panes_spend_their_space_on_commits() {
    let f = Fixture::new();
    commit(&f, "older");
    f.push();
    commit(&f, "newer");
    f.write("uncommitted", "uncommitted\n");
    let mut history = open(&f);
    let short = draw(&mut history, 65, 2).text();
    assert!(short[0].contains("newer") && short[1].contains("older"), "{short:?}");
    let narrow = draw(&mut history, 22, 12).text();
    assert!(narrow[0].starts_with(" project  main") && narrow[0].ends_with("↑1"), "{narrow:?}");
    assert_eq!(narrow[1], " ○ 1 file to commit");
    assert!(narrow[2].contains("newer"));
}

#[test]
fn ten_columns_still_show_the_commit_and_its_file() {
    let f = Fixture::new();
    f.write("a", "notes\n");
    f.commit("notes");
    let mut history = open(&f);
    assert!(active_row(&draw(&mut history, 10, 4), "notes"));
    history.key(Key::Enter);
    assert!(draw(&mut history, 10, 4).text()[1].starts_with("   │ • a"));
}

#[test]
fn an_unreadable_history_does_not_look_empty_at_one_row() {
    let f = Fixture::new();
    commit(&f, "first");
    f.write(".git/index", "corrupt index");
    let mut history = open(&f);
    assert!(draw(&mut history, 40, 1).text()[0].contains("Git did not answer."));
}

#[test]
fn a_narrow_heading_names_the_git_error() {
    let f = Fixture::new();
    commit(&f, "first");
    f.write(".git/index", "corrupt index");
    let mut history = open(&f);
    assert_eq!(draw(&mut history, 24, 12).text()[0], " project      git error");
}

#[test]
fn setting_the_remote_default_branch_updates_a_clean_timeline() {
    let f = Fixture::new();
    commit(&f, "already pushed");
    f.push();
    f.git(&["remote", "set-head", "origin", "-d"]);
    f.git(&["switch", "-q", "-c", "topic", "--no-track"]);
    commit(&f, "not pushed");
    let mut history = open(&f);
    let rows = draw(&mut history, 40, 8).text();
    assert!(!rows.iter().any(|row| row.contains("── on origin")), "{rows:?}");
    f.git(&["remote", "set-head", "origin", "main"]);
    assert!(history.check());
    let rows = draw(&mut history, 40, 8).text();
    assert!(rows[2].contains("── on origin") && rows[3].contains("already pushed"), "{rows:?}");
}

#[test]
fn a_project_name_can_contain_a_newline() {
    let mut f = Fixture::new();
    let root = f.root.with_file_name("line\nbreak");
    std::fs::rename(&f.root, &root).unwrap();
    f.root = root;
    commit(&f, "first in this project");
    let mut history = open(&f);
    let rows = draw(&mut history, 50, 8).text();
    assert!(rows[0].contains("linebreak") && rows[1].contains("first in this project"), "{rows:?}");
}

#[test]
fn a_plain_folder_says_it_is_not_a_git_project() {
    let f = Fixture::folder();
    f.write("page.txt", "a file\n");
    let mut history = open(&f);
    assert!(history.check());
    assert_eq!(draw(&mut history, 40, 3).text(), [" Not a Git project.", "", ""]);
}

#[test]
fn a_plain_folder_history_promotes_on_explicit_refresh() {
    let f = Fixture::folder();
    let mut history = open(&f);
    f.git(&["init", "-q", "--template=", "-b", "main"]);
    commit(&f, "first in the new repository");

    assert_eq!(history.key(Key::Char('r')), After::Stay);
    let rows = draw(&mut history, 50, 8).text();
    assert!(rows[0].starts_with(" project  main"), "{rows:?}");
    assert!(rows[1].contains("first in the new repository"), "{rows:?}");
}

#[test]
fn a_narrow_focus_peek_wraps_the_subject_and_restores_the_same_list_on_blur() {
    let f = Fixture::new();
    let subject = "fix: preserve 界é input across narrow rails";
    commit(&f, subject);
    for i in 1..=9 {
        commit(&f, &format!("newer {i}"));
    }
    let mut history = open(&f);
    draw(&mut history, 21, 8);
    history.key(Key::End);
    history.key(Key::Focus(false));
    let before = draw(&mut history, 21, 8).text();
    assert!(before.iter().any(|line| line.contains("fix: preserve")) && !before.join("\n").contains("narrow rails"));

    assert_eq!(history.key(Key::Focus(true)), After::Stay);
    let focused = draw(&mut history, 21, 8);
    assert!(active_row(&focused, "fix: preserve"));
    let rows = focused.text();
    let start = rows.iter().position(|row| row.contains('↳')).unwrap();
    let detail =
        rows[start..].iter().map(|row| row.trim().trim_start_matches('↳').trim()).collect::<Vec<_>>().join(" ");
    assert_eq!(detail, subject);
    history.key(Key::Click(4, start));
    assert_eq!(history.open_index(), None);

    history.key(Key::Focus(false));
    assert_eq!(draw(&mut history, 21, 8).text(), before);
}

#[test]
fn a_narrow_subject_peek_keeps_clicked_files_under_the_pointer() {
    let f = Fixture::new();
    for name in ['a', 'b', 'c', 'd', 'e', 'f'] {
        f.write(&format!("{name}.txt"), &format!("stored {name}\n"));
    }
    f.commit("first chapter second chapter final chapter");

    for (already_focused, y, name, peek_rows) in [(false, 7, 'f', 0), (false, 6, 'e', 1), (true, 2, 'd', 3)] {
        let mut history = open(&f);
        draw(&mut history, 21, 8);
        history.key(Key::Enter);
        history.key(if already_focused { Key::End } else { Key::Focus(false) });
        let file = format!("{name}.txt");
        let before = draw(&mut history, 21, 8).text();
        assert!(before[y].contains(&file), "{before:?}");

        // Focus and the first click can be received before a new frame.
        history.key(Key::Focus(true));
        let now = Instant::now();
        history.input(Key::Click(6, y), now);
        let selected = draw(&mut history, 21, 8);
        let rows = selected.text();
        assert!(rows[y].contains(&file) && selected.bar_at(y), "{rows:?}");
        assert_eq!(rows.iter().position(|line| line.contains('↳')), (peek_rows > 0).then_some(8 - peek_rows));

        history.input(Key::Click(6, y), now + std::time::Duration::from_millis(1));
        assert_eq!(draw(&mut history, 21, 8).text()[1], format!(" 1 + stored {name}"));
    }
}

#[test]
fn new_commits_only_take_the_selection_when_following_at_the_tip() {
    let f = Fixture::new();
    for i in 0..8 {
        commit(&f, &format!("older {i}"));
    }
    commit(&f, "first tip");
    let mut parked = open(&f);
    let mut following = open(&f).with_follow_new(true);
    let before = draw(&mut parked, 21, 5).text();

    commit(&f, "second tip");
    assert!(parked.check() && following.check());
    assert_eq!(draw(&mut parked, 21, 5).text(), before);
    assert!(active_row(&draw(&mut following, 21, 5), "second tip"));

    following.key(Key::Enter);
    commit(&f, "third tip");
    assert!(following.check());
    assert!(active_row(&draw(&mut following, 21, 5), "second tip"));
    assert_eq!(following.open_index(), Some(1));

    following.key(Key::Home);
    commit(&f, "fourth tip");
    assert!(following.check());
    assert!(active_row(&draw(&mut following, 21, 5), "fourth tip"));
}

#[test]
fn history_uses_text_git_marks_and_respects_the_file_icon_preference() {
    let f = Fixture::new();
    commit(&f, "pushed commit");
    f.push();
    f.write("page.txt", "committed text\n");
    f.commit("local commit");
    f.write("pending.txt", "work in progress\n");
    let mut history = open(&f).with_appearance(IconMode::Off, true);
    draw(&mut history, 40, 8);
    history.key(Key::Enter);
    let rows = draw(&mut history, 40, 8).text();
    assert!(row(&rows, "to commit").starts_with(" C"));
    assert!(row(&rows, "local commit").starts_with(" P"));
    assert!(row(&rows, "pushed commit").starts_with(" ="));
    assert!(row(&rows, "page.txt").starts_with("   │ page.txt"));
}

#[test]
fn an_oversized_subject_can_be_paged_without_moving_the_commit_cursor() {
    let f = Fixture::new();
    commit(&f, "first chapter second chapter final chapter");
    let mut history = open(&f);
    let screen = draw(&mut history, 21, 4);
    assert!(screen.text()[3].contains("↳ first chapter") && screen.text()[3].ends_with('→'));

    for (key, tail) in
        [(Key::Right, "second chapter"), (Key::Char('l'), "final chapter"), (Key::Right, "first chapter")]
    {
        assert_eq!(history.key(key), After::Stay);
        let screen = draw(&mut history, 21, 4);
        assert!(screen.text()[3].contains(&format!("↳ {tail}")));
        assert!(active_row(&screen, "first chapter"));
    }
}
