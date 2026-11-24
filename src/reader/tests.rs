use std::fs::{self, Permissions};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use super::content::{CommittedTarget, NOT_TEXT};
use super::*;
use crate::fixture::Fixture;
use crate::git::CommitId;
use crate::native::tests::with_fake;

fn drawn(reader: &mut Reader, w: usize, h: usize) -> Vec<String> {
    let mut screen = Screen::sized(w, h);
    reader.draw(&mut screen);
    screen.text()
}

fn finish_picture_preparation(reader: &mut Reader) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut screen = Screen::sized(1, 1);
    while let Some(delay) = reader.next_frame() {
        assert!(Instant::now() < deadline, "image preview did not finish");
        std::thread::sleep(delay);
        reader.draw(&mut screen);
    }
}

#[test]
fn reader_hints_say_close_and_keep_help_last() {
    let f = Fixture::folder();
    f.write("page.txt", "a file\n");
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    let rows = drawn(&mut reader, 80, 5);
    assert_eq!(rows[4].contains("^O open · ^R reveal"), cfg!(target_os = "macos"));
    assert!(rows[4].ends_with("↑↓ scroll · e edit · q close · ? help"));
    let help = reader.help();
    help.assert_rows_fit(26);
    let help = help.text();
    assert_eq!(help.contains("^O  open in default app"), cfg!(target_os = "macos"));
    assert_eq!(help.contains("^R  reveal in Finder"), cfg!(target_os = "macos"));
    assert_eq!(drawn(&mut reader, 40, 5)[4], " ↑↓ scroll · e edit · q close · ? help");
    assert_eq!(drawn(&mut reader, 28, 5)[4], " e edit · q close · ? help");
    assert_eq!(drawn(&mut reader, 20, 5)[4], " q close · ? help");
}

#[test]
fn native_actions_refuse_commit_and_handed_over_views_of_a_working_file() {
    let f = Fixture::new();
    f.write("page.txt", "before\n");
    f.commit("before");
    f.write("page.txt", "after\n");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let history = repo.history(1).unwrap();
    let id = CommitId::parse(&history[0].hash).unwrap();
    let committed = Reader::at_commit(&f.root.join("page.txt"), id).unwrap();
    let patch = crate::handoff::Patch::parse(
        "diff --git a/page.txt b/page.txt\n--- a/page.txt\n+++ b/page.txt\n@@ -1 +1 @@\n-before\n+after\n",
    )
    .unwrap();
    let handed = Reader::handed_over(&repo, &patch, "page.txt", Handed { from: None, number: 1, total: 1 });
    for (mut reader, why) in [(committed, "this is a commit"), (handed, "this shows a patch")] {
        assert!(!reader.help().text().contains("^O"));
        let ((), opened) = with_fake(Ok(()), || {
            for key in [Key::Ctrl(b'o'), Key::Ctrl(b'r')] {
                assert_eq!(reader.key(key), After::Stay);
                assert_eq!(drawn(&mut reader, 60, 4)[3], format!(" Unavailable: {why} · ? help"));
            }
        });
        assert!(opened.is_empty(), "{opened:?}");
    }
}

#[test]
fn a_native_action_keeps_the_original_root_even_when_its_path_is_reused() {
    let f = Fixture::folder();
    f.write("page.txt", "original folder\n");
    let file = f.root.join("page.txt");
    let mut reader = Reader::open(&file).unwrap();
    let (after, opened) = with_fake(Ok(()), || reader.key(Key::Ctrl(b'o')));
    assert_eq!(after, After::Stay);
    assert!(matches!(opened.as_slice(), [(Action::Open, path)] if path == &file));

    fs::rename(&f.root, f.root.with_file_name("moved")).unwrap();
    f.write("page.txt", "replacement folder\n");
    let (after, opened) = with_fake(Ok(()), || reader.key(Key::Ctrl(b'o')));
    assert_eq!(after, After::Stay);
    assert!(opened.is_empty(), "{opened:?}");
    assert!(!drawn(&mut reader, 60, 4)[3].contains("Opened in default app."));
}

#[test]
fn an_empty_file_uses_the_body_row_not_the_hint_row() {
    let f = Fixture::folder();
    f.write("empty.txt", "");
    let mut reader = Reader::open(&f.root.join("empty.txt")).unwrap();
    let rows = drawn(&mut reader, 40, 3);
    assert_eq!(&rows[1..], ["  Empty.", " e edit · q close · ? help"]);
}

#[test]
fn panning_reaches_a_diff_annotation_without_treating_it_as_source_code() {
    let f = Fixture::new();
    let stem = "界".repeat(22);
    f.write("page.rs", &format!("// {stem}e\n"));
    f.commit("before");
    f.write("page.rs", &format!("// {stem}e\u{301}\n"));

    let mut reader = Reader::open(&f.root.join("page.rs")).unwrap();
    assert!(!drawn(&mut reader, 40, 5)[2].contains("[combining acute"));
    for _ in 0..5 {
        reader.key(Key::Right);
        drawn(&mut reader, 40, 5);
    }
    let mut screen = Screen::sized(40, 5);
    reader.draw(&mut screen);
    assert!(screen.text()[2].ends_with("e\u{301} [combining acute U+0301]"));
    let out = String::from_utf8(screen.emitted()).unwrap();
    assert!(out.contains("\x1b[0;3;2;7me\u{301}\x1b"), "{out:?}");
    assert!(out.contains("\x1b[0;1;7;32m [combining acute U+0301]"), "{out:?}");
}

#[test]
fn text_with_a_picture_extension_still_shows_its_edits() {
    let f = Fixture::new();
    f.write("notes.png", "initial notes\n");
    f.commit("first");
    f.write("notes.png", "new notes\n");
    let file = f.root.join("notes.png");
    let mut working = Reader::open(&file).unwrap();
    assert!(working.next_frame().is_some());
    finish_picture_preparation(&mut working);
    assert_eq!(&drawn(&mut working, 60, 6)[1..3], ["   - initial notes", " 1 + new notes"]);

    // Status-only updates and draw ticks use the already classified source.
    use std::os::unix::fs::FileExt as _;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    static SLOTS: AtomicUsize = AtomicUsize::new(0);
    let (entered, at_read) = mpsc::channel();
    media_load::intercept_next_read(&SLOTS, Box::new(move |file, bytes, offset| {
        entered.send(()).unwrap();
        file.read_at(bytes, offset)
    }));
    f.git(&["add", "--", "notes.png"]);
    working.filesystem_changed();
    assert!(working.check());
    finish_picture_preparation(&mut working);
    assert_eq!(at_read.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert_eq!(&drawn(&mut working, 60, 6)[1..3], ["   - initial notes", " 1 + new notes"]);
    f.write("notes.png", "changed again\n");
    assert!(working.check());
    finish_picture_preparation(&mut working);
    at_read.try_recv().unwrap();

    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    let mut committed = Reader::at_commit(&file, id).unwrap();
    assert_eq!(drawn(&mut committed, 60, 6)[1], " 1 + initial notes");
}

#[test]
fn committed_file_paths_normalize_dot_components_and_repeated_slashes() {
    let f = Fixture::new();
    f.write("sub/deep/page.txt", "stored content\n");
    f.commit("first");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    let sub = f.root.join("sub");
    fs::remove_dir_all(sub.join("deep")).unwrap();
    let absolute = f.root.join("sub//deep/./page.txt");

    for (cwd, path) in [
        (f.root.as_path(), Path::new("./sub//./deep///page.txt")),
        (sub.as_path(), Path::new("deep/page.txt")),
        (f.root.as_path(), absolute.as_path()),
        (f.root.parent().unwrap(), absolute.as_path()),
    ] {
        let mut reader = CommittedTarget::resolve_from(cwd, path, id).unwrap().load();
        assert_eq!(reader.path, "sub/deep/page.txt");
        assert_eq!(drawn(&mut reader, 60, 6)[1], " 1 + stored content");
    }
}

#[test]
fn committed_file_paths_reject_parent_components() {
    let f = Fixture::new();
    f.write("sub/keep.txt", "keep the directory\n");
    f.write("page.txt", "stored content\n");
    f.commit("first");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    let absolute = f.root.join("sub/../page.txt");

    for path in [Path::new("sub/../page.txt"), absolute.as_path()] {
        assert_eq!(
            CommittedTarget::resolve_from(&f.root, path, id).err(),
            Some("File paths at a commit cannot contain '..'.")
        );
    }
}

#[test]
fn an_empty_picture_file_is_empty_not_a_broken_picture() {
    let f = Fixture::folder();
    f.write("empty.png", "");
    let mut reader = Reader::open(&f.root.join("empty.png")).unwrap();
    finish_picture_preparation(&mut reader);
    assert_eq!(drawn(&mut reader, 40, 3)[1], "  Empty.");
}

#[test]
fn a_narrow_reader_keeps_a_file_name_and_shows_its_text_when_there_is_a_body_row() {
    let f = Fixture::folder();
    f.write("test.txt", "hi\n");
    let mut reader = Reader::open(&f.root.join("test.txt")).unwrap();
    assert!(drawn(&mut reader, 40, 1)[0].ends_with("test.txt"));
    assert!(drawn(&mut reader, 40, 2)[1].ends_with("hi"));
    let narrow = drawn(&mut reader, 10, 20);
    assert!(narrow[0].ends_with("test.txt") && narrow[1].ends_with("hi"), "{narrow:?}");
}

#[test]
fn a_long_name_keeps_its_extension_in_a_narrow_heading() {
    let f = Fixture::folder();
    let file = "a-very-long-file-name-with-界.txt";
    f.write(file, "hi\n");
    let mut reader = Reader::open(&f.root.join(file)).unwrap();
    let rows = drawn(&mut reader, 28, 20);
    assert!(rows[0].contains('…') && rows[0].ends_with(".txt"), "{rows:?}");
}

#[test]
fn a_long_run_of_combining_marks_does_not_hide_later_text_on_the_same_line() {
    let f = Fixture::folder();
    let text = format!("a{}after", "\u{301}".repeat(300));
    f.write("page.rs", &format!("{text}\n"));
    let mut reader = Reader::open(&f.root.join("page.rs")).unwrap();
    assert!(drawn(&mut reader, 80, 24)[1].ends_with(&text));
}

#[test]
fn committed_pictures_and_other_binary_files_have_a_plain_note() {
    let f = Fixture::new();
    fs::write(f.root.join("logo.png"), Picture::sample_png(64, 32)).unwrap();
    fs::write(f.root.join("data.bin"), b"\0one\0two").unwrap();
    f.commit("binary files");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    f.write("logo.png", "not the saved picture");
    f.write("data.bin", "not the saved data");
    for file in ["logo.png", "data.bin"] {
        let mut reader = Reader::at_commit(&f.root.join(file), id).unwrap();
        let mut screen = Screen::sized(45, 8);
        screen.set_pictures(true);
        reader.draw(&mut screen);
        assert!(screen.picture_at().is_none());
        assert_eq!(screen.text()[2], format!("  {NOT_TEXT}"));
    }
}

#[test]
fn working_diffs_forced_to_text_do_not_show_binary_bytes() {
    let f = Fixture::new();
    f.write(".gitattributes", "data.bin diff\n");
    f.write("data.bin", "before\0");
    f.commit("binary files");
    f.write("data.bin", "after\0");
    let mut reader = Reader::open(&f.root.join("data.bin")).unwrap();
    assert_eq!(drawn(&mut reader, 45, 8)[2].trim_start(), "This file is not text.");
}

#[test]
fn invalid_text_bytes_are_not_drawn_even_without_a_nul() {
    let f = Fixture::new();
    f.write("file.rs", "fn main() {}\n");
    f.commit("text file");
    for file in ["file.rs", "new.rs"] {
        fs::write(f.root.join(file), b"fn \xff() {}\n").unwrap();
        let mut reader = Reader::open(&f.root.join(file)).unwrap();
        assert_eq!(drawn(&mut reader, 45, 8)[2].trim_start(), NOT_TEXT);
    }
}

#[test]
fn replacing_a_binary_file_with_text_does_not_look_like_an_unchanged_file() {
    let f = Fixture::new();
    f.write(".gitattributes", "data.bin diff\n");
    f.write("data.bin", "before\0");
    f.commit("binary file");
    f.write("data.bin", "ordinary text now\n");
    let mut reader = Reader::open(&f.root.join("data.bin")).unwrap();
    assert_eq!(drawn(&mut reader, 45, 8)[2].trim_start(), "This file is not text.");
}

#[test]
fn a_narrow_reader_wraps_a_note_into_the_available_body_rows() {
    let f = Fixture::folder();
    let mut reader = Reader::open(&f.root.join("absent")).unwrap();
    assert_eq!(&drawn(&mut reader, 10, 20)[2..5], ["  This", "  file is", "  gone."]);
}

#[test]
fn scrolling_into_a_nested_comment_keeps_its_colour_until_the_outer_comment_ends() {
    let f = Fixture::folder();
    let source = format!("/* outer\n/* inner */\n{}*/\nlet outside = true;\n", "let inside = true;\n".repeat(30));
    f.write("page.rs", &source);
    let mut reader = Reader::open(&f.root.join("page.rs")).unwrap();
    let mut screen = Screen::sized(50, 6);
    reader.draw(&mut screen);
    reader.key(Key::Char('G'));
    reader.draw(&mut screen);
    let out = String::from_utf8(screen.emitted()).unwrap();
    assert!(out.contains("\x1b[0;3;2mlet inside = true;"), "{out:?}");
    assert!(out.contains("\x1b[0;35mlet"), "{out:?}");
}

#[test]
fn unknown_extensions_and_conflicting_shebangs_do_not_colour_code() {
    let f = Fixture::folder();
    for (name, source, coloured) in [
        ("page.unknown", "fn main() {}", false),
        ("page.rs", "#!/usr/bin/env -S python3 -u\nreturn True", false),
        ("other.rs", "#!/usr/bin/ruby\nreturn true", false),
        ("page.py", "#!/usr/bin/env -S python3 -u\nreturn True", true),
        ("page.ts", "#!/usr/bin/env tsx\nreturn true", true),
        ("other.ts", "#!/usr/bin/env -S ts-node --esm\nreturn true", true),
        ("page.mjs", "#!/usr/bin/env zx\nreturn true", true),
        ("page.sh", "#!/bin/dash\nif true; then exit; fi", true),
        ("script.rs", "#!/usr/bin/env cargo\nfn main() {}", true),
        ("tool.lua", "#!/usr/bin/env lua5.4\nreturn true", true),
        ("other.lua", "#!/usr/bin/env python3\nreturn True", false),
        ("tool.swift", "#!/usr/bin/swift\nreturn true", true),
        ("rules.mk", "#!/usr/bin/make -f\ninclude more.mk", true),
    ] {
        f.write(name, source);
        let mut reader = Reader::open(&f.root.join(name)).unwrap();
        let mut screen = Screen::sized(60, 6);
        reader.draw(&mut screen);
        assert_eq!(screen.emitted().windows(7).any(|seq| seq == b"\x1b[0;35m"), coloured, "{name}");
        assert!(screen.text().iter().any(|row| row.ends_with(source.lines().last().unwrap())));
    }
}

#[test]
fn both_jump_keys_work_in_a_committed_file() {
    let f = Fixture::new();
    let file = |changed| {
        (1..=60)
            .map(|n| format!("row {n}{}\n", if changed && matches!(n, 8 | 45) { " edited" } else { "" }))
            .collect::<String>()
    };
    f.write("page.txt", &file(false));
    f.commit("original");
    f.write("page.txt", &file(true));
    f.commit("edit two lines");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    let mut reader = Reader::at_commit(&f.root.join("page.txt"), id).unwrap();
    for (line, key, notice) in [(8, 'n', None), (45, 'N', Some("Change 2 of 2")), (8, 'n', Some("Change 1 of 2"))] {
        let rows = drawn(&mut reader, 45, 12);
        assert!(rows.iter().any(|row| row.contains(&format!("+ row {line} edited"))), "{rows:?}");
        if let Some(notice) = notice {
            assert!(rows[11].contains(notice) && rows[11].ends_with("? help"), "{rows:?}");
        }
        reader.key(Key::Char(key));
    }
}

fn review_fixture(f: &Fixture) -> String {
    let original = (1..=60).map(|n| format!("line {n}\n")).collect::<String>();
    f.write("page.txt", &original);
    f.commit("before");
    original.replace("line 11\n", "edited 11\n").replace("line 41\n", "edited 41\n")
}

#[test]
fn changes_only_keeps_context_and_jump_position_in_working_and_committed_diffs() {
    let f = Fixture::new();
    let edited = review_fixture(&f);
    f.write("page.txt", &edited);
    let path = f.root.join("page.txt");
    let working = Reader::open(&path).unwrap();
    f.commit("after");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    let committed = Reader::at_commit(&path, id).unwrap();
    for mut reader in [working, committed] {
        let full = drawn(&mut reader, 60, 24);
        assert!(full[23].contains("c changes only"), "{full:?}");
        assert_eq!(reader.key(Key::Char('c')), After::Stay);
        let compact = drawn(&mut reader, 60, 24);
        for label in [
            "… 7 unchanged lines",
            "… 23 unchanged lines",
            "… 16 unchanged lines",
            "line 8",
            "line 14",
            "line 38",
            "line 44",
            "+ edited 11",
            "+ edited 41",
            "c full file",
        ] {
            assert!(compact.iter().any(|row| row.contains(label)), "{label}: {compact:?}");
        }
        for hidden in ["line 7", "line 15", "line 37", "line 45"] {
            assert!(!compact.iter().any(|row| row.ends_with(hidden)), "{hidden}: {compact:?}");
        }

        assert_eq!(reader.key(Key::Char('n')), After::Stay);
        let next = drawn(&mut reader, 20, 4);
        assert!(next[..3].iter().any(|row| row.contains("- line 41")), "{next:?}");
        assert!(next[3].contains("Change 2 of 2"), "{next:?}");
        assert_eq!(reader.key(Key::Char('c')), After::Stay);
        let next_full = drawn(&mut reader, 20, 4);
        assert!(next_full[..3].iter().any(|row| row.contains("- line 41")), "{next_full:?}");
        assert_eq!(reader.key(Key::Char('N')), After::Stay);
        let previous = drawn(&mut reader, 20, 4);
        assert!(previous[..3].iter().any(|row| row.contains("- line 11")), "{previous:?}");
        assert!(previous[3].contains("Change 1 of 2"), "{previous:?}");
    }
}

#[test]
fn folds_expand_individually_with_tab_enter_and_a_mouse_click() {
    let f = Fixture::new();
    let edited = review_fixture(&f);
    f.write("page.txt", &edited);
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    drawn(&mut reader, 60, 24);
    assert_eq!(reader.key(Key::Char('c')), After::Stay);
    let first = drawn(&mut reader, 60, 24);
    assert!(first[1].contains("… 7 unchanged lines · Enter"), "{first:?}");

    assert_eq!(reader.key(Key::Ctrl(b'i')), After::Stay);
    let second = drawn(&mut reader, 60, 24);
    let fold = second.iter().position(|row| row.contains("… 23 unchanged lines · Enter")).unwrap();
    assert_eq!(reader.key(Key::Enter), After::Stay);
    let expanded = drawn(&mut reader, 60, 24);
    assert!(expanded[fold].contains("15   line 15"), "{expanded:?}");
    assert!(expanded[1].contains("… 7 unchanged lines"), "{expanded:?}");
    assert_eq!(reader.key(Key::Click(59, 1)), After::Ignored); // scrollbar
    assert_eq!(reader.key(Key::Click(6, 1)), After::Stay);
    let clicked = drawn(&mut reader, 60, 24);
    assert!(clicked[1].contains("1   line 1"), "{clicked:?}");
    assert!(clicked.iter().any(|row| row.ends_with("line 15")), "{clicked:?}");

    drawn(&mut reader, 15, 9);
    assert_eq!(reader.key(Key::Char('G')), After::Stay);
    let narrow = drawn(&mut reader, 15, 9);
    assert!(narrow[7].contains("… 16 hidden"), "{narrow:?}");
    assert_eq!(reader.key(Key::Click(1, 7)), After::Stay);
    let last = drawn(&mut reader, 15, 9);
    assert!(last[7].contains("line 45"), "{last:?}");
}

#[test]
fn reopening_the_full_file_keeps_a_scrolled_anchor_inside_a_fold() {
    let f = Fixture::new();
    let edited = review_fixture(&f);
    f.write("page.txt", &edited);
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    drawn(&mut reader, 60, 9);
    for _ in 0..20 {
        reader.key(Key::Down);
    }
    let full = drawn(&mut reader, 60, 9);
    assert!(full[1].contains("line 27"), "{full:?}");
    assert_eq!(reader.key(Key::Char('c')), After::Stay);
    let folded = drawn(&mut reader, 60, 9);
    assert!(folded[1].contains("… 23 unchanged lines"), "{folded:?}");
    assert_eq!(reader.key(Key::Char('c')), After::Stay);
    assert_eq!(drawn(&mut reader, 60, 9), full);
}

#[test]
fn a_changed_working_patch_rebuilds_folds_without_losing_the_file_anchor() {
    let f = Fixture::new();
    let edited = review_fixture(&f);
    f.write("page.txt", &edited);
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    drawn(&mut reader, 60, 9);
    for _ in 0..20 {
        reader.key(Key::Down);
    }
    assert_eq!(reader.key(Key::Char('c')), After::Stay);
    f.write("page.txt", &edited.replace("line 26\n", "new edit\n"));
    reader.filesystem_changed();
    reader.check();
    let compact = drawn(&mut reader, 60, 24);
    assert!(compact.iter().any(|row| row.contains("+ new edit")), "{compact:?}");
    assert!(compact.iter().any(|row| row.contains("… 8 unchanged lines")), "{compact:?}");
    assert!(compact[23].contains("c full file"), "{compact:?}");
    assert_eq!(reader.key(Key::Char('c')), After::Stay);
    let full = drawn(&mut reader, 60, 9);
    assert!(full[1].contains("line 27"), "{full:?}");
}

#[test]
fn folding_skips_to_the_right_syntax_state_after_each_hidden_range() {
    let f = Fixture::new();
    let original = format!(
        "{}/* hidden opener\n{}let inside_fold = true;\nold comment\n{}\n*/\n{}let after_fold = true;\nlet old = false;\n",
        "// before\n".repeat(10),
        "foo();\n".repeat(10),
        "bar();\n".repeat(15),
        "// after\n".repeat(10),
    );
    f.write("page.rs", &original);
    f.commit("before");
    f.write("page.rs", &original.replace("old comment", "new comment").replace("let old", "let new"));
    let mut reader = Reader::open(&f.root.join("page.rs")).unwrap();
    drawn(&mut reader, 60, 24);
    assert_eq!(reader.key(Key::Char('c')), After::Stay);
    let mut screen = Screen::sized(60, 24);
    reader.draw(&mut screen);
    let out = String::from_utf8(screen.emitted()).unwrap();
    assert!(out.contains("\x1b[0;3;2mlet inside_fold = true;"), "{out:?}");
    assert!(runs(&out).iter().any(|(codes, text)| *codes == "0;35" && text.starts_with("let")), "{out:?}");
    assert!(screen.text().iter().any(|row| row.ends_with("let after_fold = true;")));
}

#[test]
fn the_reader_counts_added_and_removed_lines_and_numbers_only_the_current_file() {
    let f = Fixture::new();
    f.write("file.txt", "one\nbefore\nfour\n");
    f.commit("first");
    f.write("file.txt", "one\nafter\nthree\nfour\n");

    let mut reader = Reader::open(&f.root.join("file.txt")).unwrap();
    let rows = drawn(&mut reader, 40, 8);
    assert!(rows[0].ends_with("+2 −1"), "{rows:?}");
    assert_eq!(&rows[1..6], [" 1   one", "   - before", " 2 + after", " 3 + three", " 4   four"]);
}

#[test]
fn line_numbers_align_when_they_have_different_widths() {
    let f = Fixture::folder();
    f.write("file.txt", &"line\n".repeat(10));

    let mut reader = Reader::open(&f.root.join("file.txt")).unwrap();
    let rows = drawn(&mut reader, 40, 12);
    assert_eq!(&rows[9..11], ["  9   line", " 10   line"]);
}

#[test]
fn every_line_of_an_untracked_file_is_added() {
    let f = Fixture::new();
    f.write("file.txt", "first\nlast without a newline");

    let mut reader = Reader::open(&f.root.join("file.txt")).unwrap();
    let rows = drawn(&mut reader, 45, 6);
    assert!(rows[0].contains("new") && rows[0].ends_with("+2"), "{rows:?}");
    assert_eq!(&rows[1..3], [" 1 + first", " 2 + last without a newline"]);
}

#[test]
fn the_reader_opens_a_deleted_file_in_a_deleted_directory() {
    let f = Fixture::new();
    f.write("sub/file.txt", "removed content\n");
    f.commit("first");
    fs::remove_dir_all(f.root.join("sub")).unwrap();

    let mut reader = Reader::open(&f.root.join("sub/file.txt")).unwrap();
    let rows = drawn(&mut reader, 60, 6);
    assert!(rows[0].contains("removed") && rows[0].ends_with("−1"), "{rows:?}");
    assert_eq!(rows[1], "   - removed content");
}

#[test]
fn repeated_edits_to_a_tracked_file_use_one_fresh_git_status_each() {
    const PROJECT: &str = "MILO_TEST_READER_STATUS_PROJECT";
    if let Some(root) = std::env::var_os(PROJECT) {
        let root = PathBuf::from(root);
        let path = root.join("page.txt");
        let trace = root.join(".git/reader-trace");
        let mut reader = Reader::open(&path).unwrap();
        for text in ["first edit", "second edit"] {
            // Replacing the file distinguishes its stamp even on a coarse clock.
            let replacement = root.join(".git/replacement");
            fs::write(&replacement, format!("{text}\n")).unwrap();
            fs::rename(replacement, &path).unwrap();
            fs::write(&trace, "").unwrap();
            assert!(reader.check());
            assert!(drawn(&mut reader, 60, 6).iter().any(|row| row.contains(text)));
            assert_git_calls(&trace, [1, 1, 1]);
        }

        fs::create_dir(root.join("unrelated")).unwrap();
        fs::write(root.join("unrelated/.gitattributes"), "* -diff\n").unwrap();
        fs::write(&trace, "").unwrap();
        assert!(reader.check());
        assert_git_calls(&trace, [1, 1, 0]);
        fs::write(&trace, "").unwrap();
        reader.refresh(true);
        assert_git_calls(&trace, [1, 1, 1]);
        return;
    }

    let f = Fixture::new();
    f.write("page.txt", "base\n");
    f.commit("base");
    let trace = f.root.join(".git/reader-trace");
    let config = f.root.join(".git/.gitconfig");
    f.git(&["config", "--file", config.to_str().unwrap(), "trace2.eventTarget", trace.to_str().unwrap()]);
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "reader::tests::repeated_edits_to_a_tracked_file_use_one_fresh_git_status_each"])
        .env(PROJECT, &f.root)
        .env("HOME", config.parent().unwrap())
        .env("XDG_CONFIG_HOME", f.root.join(".git/test-xdg"))
        .env("GIT_CONFIG_GLOBAL", config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && stdout.contains("1 passed"), "{stdout}");
}

fn assert_git_calls(trace: &Path, expected: [usize; 3]) {
    let trace = fs::read_to_string(trace).unwrap();
    let calls = ["config", "status", "diff"].map(|name| {
        let name = format!("\"name\":\"{name}\"");
        trace.lines().filter(|line| line.contains("\"event\":\"cmd_name\"") && line.contains(&name)).count()
    });
    assert_eq!(calls, expected, "counts for config, status and diff");
}

#[test]
fn a_file_replaced_with_the_same_length_and_mtime_refreshes_then_can_disappear() {
    let f = Fixture::folder();
    f.write("file.txt", "before");
    let path = f.root.join("file.txt");
    let mut reader = Reader::open(&path).unwrap();
    assert!(drawn(&mut reader, 60, 6)[1].ends_with("before"));

    f.write("replacement", "edited");
    let replacement = f.root.join("replacement");
    let modified = path.metadata().unwrap().modified().unwrap();
    fs::File::open(&replacement).unwrap().set_modified(modified).unwrap();
    fs::rename(replacement, &path).unwrap();
    assert!(reader.check());
    assert!(drawn(&mut reader, 60, 6)[1].ends_with("edited"));

    fs::remove_file(path).unwrap();
    assert!(reader.check());
    assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), "This file is gone.");
}

#[test]
fn a_directory_replaced_by_a_link_while_open_does_not_show_its_target() {
    let f = Fixture::folder();
    let outside = Fixture::folder();
    f.write("sub/file.txt", "before");
    outside.write("file.txt", "outside content");
    let dir = f.root.join("sub");
    let mut reader = Reader::open(&dir.join("file.txt")).unwrap();
    assert!(drawn(&mut reader, 60, 6)[1].ends_with("before"));

    fs::rename(&dir, f.root.join("previous")).unwrap();
    symlink(&outside.root, dir).unwrap();
    assert!(reader.check());
    assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), "This path now leads outside the project.");
}

#[test]
fn moved_project_readers_keep_reading_the_original_text_and_picture() {
    use crate::pane::{check_watched_pane, watch_pane};

    let f = Fixture::new();
    f.write("sub/file.txt", "original text\n");
    fs::write(f.root.join("sub/logo.png"), Picture::sample_png(16, 8)).unwrap();
    f.commit("original project");
    let mut text = Reader::open(&f.root.join("sub/file.txt")).unwrap();
    let mut picture = Reader::open(&f.root.join("sub/logo.png")).unwrap();
    assert!(drawn(&mut text, 60, 6)[1].ends_with("original text"));
    finish_picture_preparation(&mut picture);
    assert!(drawn(&mut picture, 60, 6)[0].ends_with("16×8"));
    let (mut watcher, mut pace) = watch_pane(&mut text, false);
    assert_eq!(watcher.scope().roots.as_slice(), std::slice::from_ref(&f.root));

    let moved = f.root.with_file_name("moved-project");
    fs::rename(&f.root, &moved).unwrap();
    fs::create_dir(&f.root).unwrap();
    f.git(&["init", "-q", "-b", "main"]);
    f.write("sub/file.txt", "replacement project text\n");
    fs::write(f.root.join("sub/logo.png"), Picture::sample_png(64, 32)).unwrap();
    f.commit("replacement project");
    f.write("sub/file.txt", "replacement project edit\n");
    f.write(".git/index.lock", "");

    fs::write(moved.join("sub/file.txt"), "moved project edit\n").unwrap();
    fs::write(moved.join("sub/logo.png"), Picture::sample_png(32, 16)).unwrap();
    assert!(check_watched_pane(&mut text, &mut watcher, &mut pace, false));
    assert!(!text.git_busy() && !picture.git_busy());
    assert!(watcher.scope().roots.is_empty() && !watcher.scope().complete);
    assert!(picture.check());
    assert_eq!(drawn(&mut text, 60, 6)[1], " 1   moved project edit");
    finish_picture_preparation(&mut picture);
    assert!(drawn(&mut picture, 60, 6)[0].ends_with("32×16"));

    // Still poll the original, and reinstall path-based coverage if it comes back.
    fs::write(moved.join("sub/file.txt"), "another moved edit\n").unwrap();
    assert!(check_watched_pane(&mut text, &mut watcher, &mut pace, false));
    assert_eq!(drawn(&mut text, 60, 6)[1], " 1   another moved edit");
    fs::rename(&f.root, f.root.with_file_name("replacement")).unwrap();
    fs::rename(moved, &f.root).unwrap();
    assert!(check_watched_pane(&mut text, &mut watcher, &mut pace, false));
    assert_eq!(watcher.scope().roots.as_slice(), std::slice::from_ref(&f.root));
}

#[test]
fn reader_follows_internal_directory_links_without_following_the_final_link() {
    let f = Fixture::new();
    f.write("original/sub/file.txt", "first branch\n");
    f.write("other/file.txt", "second branch\n");
    symlink("../other/file.txt", f.root.join("other/link.txt")).unwrap();
    f.commit("shared project files");
    let sub = f.root.join("original/sub");
    let shortcut = f.root.join("shortcut");
    symlink("original/sub", &shortcut).unwrap();
    let mut text = Reader::open(&shortcut.join("file.txt")).unwrap();
    assert!(drawn(&mut text, 60, 6)[1].ends_with("first branch"));

    fs::rename(&sub, f.root.join("previous")).unwrap();
    symlink("../other", &sub).unwrap();
    assert!(text.check());
    assert_eq!(drawn(&mut text, 60, 6)[1], " 1   second branch");

    fs::remove_file(&sub).unwrap();
    symlink(f.root.join("previous"), &sub).unwrap();
    assert!(text.check());
    assert_eq!(drawn(&mut text, 60, 6)[1], " 1   first branch");

    fs::remove_file(&sub).unwrap();
    symlink(f.root.join("other"), &sub).unwrap();
    let mut link = Reader::open(&shortcut.join("link.txt")).unwrap();
    assert_eq!(drawn(&mut link, 60, 6)[1], " 1   ../other/file.txt");
}

#[test]
fn a_reader_follows_retargets_of_the_original_directory_alias() {
    let f = Fixture::new();
    let outside = Fixture::folder();
    f.write("first/report.txt", "first report\n");
    f.write("second/report.txt", "second report\n");
    outside.write("report.txt", "outside report\n");
    f.commit("reports");

    let shortcut = f.root.join("current");
    symlink("first", &shortcut).unwrap();
    let mut reader = Reader::open(&shortcut.join("report.txt")).unwrap();
    let rows = drawn(&mut reader, 60, 6);
    assert!(rows[0].contains("current") && rows[1].ends_with("first report"), "{rows:?}");

    let replacement = f.root.join("new-link");
    let retarget = |target: &Path| {
        symlink(target, &replacement).unwrap();
        fs::rename(&replacement, &shortcut).unwrap();
    };
    retarget(Path::new("second"));
    assert!(reader.check());
    assert_eq!(drawn(&mut reader, 60, 6)[1], " 1   second report");

    retarget(&outside.root);
    assert!(reader.check());
    assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), "This path now leads outside the project.");

    retarget(Path::new("first"));
    assert!(reader.check());
    assert_eq!(drawn(&mut reader, 60, 6)[1], " 1   first report");
}

#[test]
fn a_pipe_replacing_a_tracked_file_is_never_read_as_one() {
    let f = Fixture::new();
    f.write("file.txt", "before");
    f.commit("text file");
    let path = f.root.join("file.txt");
    let mut reader = Reader::open(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `name` is a live, NUL-terminated path in the fixture.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

    assert!(reader.check());
    let mut opened = Reader::open(&path).unwrap();
    for pane in [&mut reader, &mut opened] {
        assert_eq!(drawn(pane, 60, 6)[2].trim_start(), "Not a file, so there is nothing to show.");
    }
}

#[test]
fn an_unreadable_tracked_file_explains_why_it_cannot_be_read() {
    let f = Fixture::new();
    f.write("file.txt", "before");
    f.commit("text file");
    f.write("file.txt", "new bytes");
    let path = f.root.join("file.txt");
    fs::set_permissions(&path, Permissions::from_mode(0o0)).unwrap();
    if fs::File::open(&path).is_ok() {
        return;
    }
    let mut reader = Reader::open(&path).unwrap();
    assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), "This file cannot be read.");
}

#[test]
fn next_change_wraps_from_the_last_page_back_to_the_first_change() {
    let f = Fixture::new();
    let lines = |changed| {
        (1..=100)
            .map(|n| format!("line {n}{}\n", if changed && matches!(n, 10 | 95) { " edited" } else { "" }))
            .collect::<String>()
    };
    f.write("file.txt", &lines(false));
    f.commit("first");
    f.write("file.txt", &lines(true));

    let mut reader = Reader::open(&f.root.join("file.txt")).unwrap();
    for n in [10, 95, 10, 95] {
        let rows = drawn(&mut reader, 45, 22);
        let change = format!("+ line {n} edited");
        assert!(rows.iter().any(|row| row.contains(&change)), "{rows:?}");
        reader.key(Key::Char('n'));
    }
}

#[test]
fn opening_and_jumping_to_changes_keeps_the_change_visible_in_a_short_reader() {
    let f = Fixture::new();
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    f.write("page.txt", &original);
    f.commit("original");
    let edited =
        original.replace("line 8\n", "first insertion\nline 8\n").replace("line 16\n", "second insertion\nline 16\n");
    f.write("page.txt", &edited);

    for height in 2..=6 {
        let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
        assert!(drawn(&mut reader, 40, 1)[0].contains("page.txt"));
        for (key, change, notice) in [
            (None, "+ first insertion", ""),
            (Some('n'), "+ second insertion", "Change 2 of 2"),
            (Some('n'), "+ first insertion", "Change 1 of 2"),
            (Some('N'), "+ second insertion", "Change 2 of 2"),
            (Some('N'), "+ first insertion", "Change 1 of 2"),
        ] {
            if let Some(key) = key {
                assert!(matches!(reader.key(Key::Char(key)), After::Stay));
            }
            let rows = drawn(&mut reader, 40, height);
            let notice_row = if height == 2 { 0 } else { height - 1 };
            let end = if height == 2 { height } else { notice_row };
            assert!(rows[1..end].iter().any(|row| row.contains(change)), "height {height}: {rows:?}");
            if key.is_some() {
                assert!(rows[notice_row].contains(notice), "height {height}: {rows:?}");
            }
        }
    }
}

#[test]
fn jumping_wraps_even_when_two_changes_share_the_last_page_top() {
    let mut kinds = [LineKind::Same; 100];
    kinds[9] = LineKind::Del;
    kinds[10] = LineKind::Add;
    kinds[94] = LineKind::Add;
    kinds[96] = LineKind::Del;
    let source = Source::Committed { project: PathBuf::new(), hash: "123abcd".into(), time: 0 };
    let mut reader = Reader::new("page.txt".into(), source, Preferences::default().icons, None);
    reader.body = Body::Text(Text::new(
        (1..).zip(kinds).map(|(number, kind)| Line::new(kind, number, "")).collect(),
        None,
    ));
    drawn(&mut reader, 40, 20);
    assert_eq!(reader.top, 6);
    for (key, top, notice) in [
        ('n', 82, "Change 2 of 3"),
        ('n', 82, "Change 3 of 3"),
        ('n', 6, "Change 1 of 3"),
        ('N', 82, "Change 3 of 3"),
        ('N', 82, "Change 2 of 3"),
        ('N', 6, "Change 1 of 3"),
    ] {
        assert_eq!(reader.key(Key::Char(key)), After::Stay);
        let rows = drawn(&mut reader, 40, 20);
        assert_eq!(reader.top, top);
        assert!(rows[19].starts_with(&format!(" {notice}")), "{rows:?}");
    }
    assert_eq!(reader.key(Key::PgDn), After::Stay);
    let rows = drawn(&mut reader, 40, 20);
    assert!(!rows[19].contains("Change "), "{rows:?}");
    assert_eq!(reader.key(Key::Char('n')), After::Stay);
    let rows = drawn(&mut reader, 40, 20);
    assert_eq!(reader.top, 82);
    assert!(rows[19].contains("Change 2 of 3"), "{rows:?}");
}

#[test]
fn no_changes_appears_once_then_restores_the_reader_chrome() {
    let f = Fixture::folder();
    f.write("page.txt", "unchanged\n");
    let start = Instant::now();
    for height in [2, 5] {
        let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
        let mut screen = Screen::sized(40, height);
        reader.draw_file(&mut screen, start);
        let ordinary = screen.text();
        let notice_row = if height == 2 { 0 } else { height - 1 };
        assert_eq!(reader.input(Key::Char('N'), start), After::Stay);
        let almost_done = start + CHANGE_NOTICE - Duration::from_millis(1);
        assert_eq!(reader.input(Key::Char('n'), almost_done), After::Ignored);
        assert!(reader.next_frame().is_some());
        reader.draw_file(&mut screen, almost_done);
        let showing = screen.text();
        assert!(showing[notice_row].contains("No changes"), "{showing:?}");
        assert_eq!(showing[1], ordinary[1]);

        let expired = start + CHANGE_NOTICE;
        reader.draw_file(&mut screen, expired);
        assert_eq!(screen.text(), ordinary);
        assert_eq!(reader.next_frame(), None);
        assert_eq!(reader.input(Key::Char('n'), expired), After::Stay);
    }
}

#[test]
fn change_feedback_leaves_horizontal_position_and_clamping_to_the_body() {
    let f = Fixture::new();
    let original = (1..=24).map(|n| format!("line {n}\n")).collect::<String>();
    f.write("page.txt", &original);
    f.commit("original");
    let edited = original
        .replace("line 6\n", &format!("first: {}\nline 6\n", "a".repeat(40)))
        .replace("line 12\n", &format!("second: {}\nline 12\n", "b".repeat(40)))
        .replace("line 18\n", "short\nline 18\n");
    f.write("page.txt", &edited);
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    drawn(&mut reader, 24, 2);
    assert_eq!(reader.key(Key::Right), After::Stay);
    let before = reader.pan;
    assert!(before > 0);

    assert_eq!(reader.key(Key::Char('n')), After::Stay);
    let rows = drawn(&mut reader, 24, 2);
    assert!(rows[0].ends_with("Change 2 of 3") && rows[1].contains("+‹bbb"), "{rows:?}");
    assert_eq!(reader.pan, before);
    assert_eq!(drawn(&mut reader, 8, 2)[0], "2 of 3");

    assert_eq!(reader.key(Key::Char('n')), After::Stay);
    let rows = drawn(&mut reader, 24, 2);
    assert!(rows[0].ends_with("Change 3 of 3") && rows[1].contains("+ short"), "{rows:?}");
    assert_eq!(reader.pan, 0);
}

#[test]
fn a_single_change_reports_its_position_without_moving() {
    let f = Fixture::new();
    f.write("page.txt", "original\n");
    f.commit("original");
    f.write("page.txt", "edited\n");
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    let start = Instant::now();
    let mut screen = Screen::sized(40, 5);
    reader.draw_file(&mut screen, start);
    let ordinary = screen.text();
    assert_eq!(reader.input(Key::Char('n'), start), After::Stay);
    reader.draw_file(&mut screen, start);
    assert!(screen.text()[4].contains("Change 1 of 1"));
    assert_eq!(&screen.text()[..4], &ordinary[..4]);
    reader.draw_file(&mut screen, start + CHANGE_NOTICE);
    assert_eq!(screen.text(), ordinary);
}

#[test]
fn panning_can_reach_the_end_of_a_line_of_wide_characters() {
    let dir = Fixture::folder();
    let file = dir.root.join("text");
    fs::write(&file, format!("{}最後", "界".repeat(16))).unwrap();
    let mut reader = Reader::open(&file).unwrap();
    let mut screen = Screen::new();
    screen.resize(20, 3);
    for _ in 0..4 {
        reader.key(Key::Right);
        reader.draw(&mut screen);
    }
    assert_eq!(reader.pan, 22);
    assert!(screen.text()[1].ends_with("界界界界界最後"));
}

#[test]
fn panning_a_narrow_reader_does_not_skip_text_or_a_wide_character() {
    let f = Fixture::folder();
    f.write("page.txt", "abc界defghijk\n");
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    assert!(drawn(&mut reader, 10, 3)[1].ends_with("abc"));

    for (key, expected) in [(Key::Right, "界de"), (Key::Right, "efgh"), (Key::Right, "hijk"), (Key::Left, "efgh")] {
        assert!(matches!(reader.key(key), After::Stay));
        let rows = drawn(&mut reader, 10, 3);
        assert!(rows[1].ends_with(expected), "{rows:?}");
    }
}

#[test]
fn panning_left_after_batched_right_repeats_moves_from_the_right_edge() {
    let f = Fixture::folder();
    f.write("page.txt", "0123456789abcdefghijklmnopqrstuvwxyz");
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    assert!(drawn(&mut reader, 20, 3)[1].ends_with("0123456789abcd"));
    for _ in 0..3 {
        assert!(matches!(reader.key(Key::Right), After::Stay));
    }
    assert!(drawn(&mut reader, 20, 3)[1].ends_with("mnopqrstuvwxyz"));

    for _ in 0..2 {
        assert!(matches!(reader.key(Key::Right), After::Ignored));
    }
    assert!(matches!(reader.key(Key::Left), After::Stay));
    assert!(drawn(&mut reader, 20, 3)[1].ends_with("efghijklmnopqr"));
}

#[test]
fn a_link_shows_its_path_even_when_the_target_is_gone() {
    let f = Fixture::new();
    let target = f.root.join("../target.txt");
    let link = f.root.join("link");
    fs::write(&target, "outside the project\n").unwrap();
    symlink("../target.txt", &link).unwrap();
    let mut reader = Reader::open(&link).unwrap();
    let rows = drawn(&mut reader, 60, 6);
    assert!(rows[0].ends_with("+1"), "{rows:?}");
    assert_eq!(rows[1], " 1 + ../target.txt");
    assert!(!rows.iter().any(|row| row.contains("outside the project")));
    fs::remove_file(target).unwrap();
    let mut reader = Reader::open(&link).unwrap();
    assert_eq!(drawn(&mut reader, 60, 6), rows);
}

#[test]
fn a_failed_git_read_does_not_replace_the_file_on_screen() {
    let f = Fixture::new();
    f.write("page.txt", "first\n");
    f.commit("first");
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    let before = drawn(&mut reader, 60, 6);
    let index = f.root.join(".git/index");
    let saved = fs::read(&index).unwrap();
    f.write(".git/index", "corrupt index");
    f.write("page.txt", "edited while git is not answering\n");
    assert!(reader.check());
    let Source::Working(work) = &reader.source else { unreachable!() };
    assert!(!work.repo.ok());
    assert_eq!(drawn(&mut reader, 60, 6), before);

    fs::write(index, saved).unwrap();
    assert!(reader.check());
    let Source::Working(work) = &reader.source else { unreachable!() };
    assert!(work.repo.ok());
    assert!(drawn(&mut reader, 60, 6).iter().any(|row| row.contains("edited while git is not answering")));
}

#[test]
fn a_working_reader_refreshes_after_the_index_lock_clears() {
    use crate::pane::{check_watched_pane, watch_pane};

    let f = Fixture::new();
    f.write("page.txt", "base\n");
    f.commit("base");
    f.write("page.txt", "first edit\n");
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    let before = drawn(&mut reader, 60, 6);
    let (mut watcher, mut pace) = watch_pane(&mut reader, false);

    f.write(".git/index.lock", "");
    f.write(".git/replacement", "latest edit\n");
    fs::rename(f.root.join(".git/replacement"), f.root.join("page.txt")).unwrap();
    check_watched_pane(&mut reader, &mut watcher, &mut pace, false);
    assert_eq!(drawn(&mut reader, 60, 6), before);

    fs::remove_file(f.root.join(".git/index.lock")).unwrap();
    assert!(check_watched_pane(&mut reader, &mut watcher, &mut pace, false));
    assert!(drawn(&mut reader, 60, 6).iter().any(|row| row.contains("latest edit")));
}

#[test]
fn polling_a_working_reader_refreshes_attribute_only_diff_changes() {
    let f = Fixture::new();
    f.write("src/page.txt", "original\n");
    f.write("src/.gitattributes", "page.txt diff\n");
    f.commit("base");
    f.write("src/page.txt", "edited\n");
    let mut reader = Reader::open(&f.root.join("src/page.txt")).unwrap();
    assert_eq!(drawn(&mut reader, 60, 6)[2], " 1 + edited");

    let replace = |rule| {
        f.write(".git/replacement-rule", rule);
        fs::rename(f.root.join(".git/replacement-rule"), f.root.join("src/.gitattributes")).unwrap();
    };
    replace("page.txt -diff\n");
    assert!(reader.check());
    assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), NOT_TEXT);

    replace("page.txt diff\n");
    assert!(reader.check());
    assert_eq!(drawn(&mut reader, 60, 6)[2], " 1 + edited");
}

#[test]
fn polling_a_linked_worktree_reader_checks_shared_info_attributes() {
    let f = Fixture::new();
    f.write("page.txt", "original\n");
    f.commit("base");
    f.git(&["worktree", "add", "-q", "../linked"]);
    let file = f.root.with_file_name("linked").join("page.txt");
    fs::write(&file, "edited\n").unwrap();
    let mut reader = Reader::open(&file).unwrap();
    assert_eq!(drawn(&mut reader, 60, 6)[2], " 1 + edited");

    f.write(".git/info/attributes", "page.txt -diff\n");
    assert!(reader.check());
    assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), NOT_TEXT);
}

#[test]
fn the_inline_reader_refreshes_configured_attributes_after_a_notification() {
    let f = Fixture::new();
    f.write("page.txt", "original\n");
    f.write(".git/custom-attributes", "page.txt diff\n");
    let rules = f.root.join(".git/custom-attributes");
    f.git(&["config", "core.attributesFile", rules.to_str().unwrap()]);
    f.commit("base");
    f.write("page.txt", "edited\n");
    let mut outline = crate::outline::Outline::new(Repo::open(&f.root).unwrap(), false);
    // Use the selectable tree, not the short usage-style strip.
    let mut screen = Screen::sized(60, 12);
    outline.draw(&mut screen);
    outline.key(Key::Enter);
    outline.draw(&mut screen);
    assert_eq!(screen.text()[2], " 1 + edited");
    assert_eq!(outline.key(Key::Char('n')), After::Stay);
    outline.draw(&mut screen);
    assert!(screen.text()[11].contains("Change 1 of 1"));

    f.write(".git/custom-attributes", "page.txt -diff\n");
    outline.filesystem_changed();
    assert!(outline.check());
    outline.draw(&mut screen);
    assert_eq!(screen.text()[2].trim_start(), NOT_TEXT);
    assert!(!screen.text()[11].contains("Change 1 of 1"));
}

#[test]
fn a_picture_is_shown_in_the_middle_at_its_own_size_where_the_terminal_can() {
    let dir = Fixture::folder();
    fs::write(dir.root.join("logo.png"), Picture::sample_png(64, 32)).unwrap();
    let mut reader = Reader::open(&dir.root.join("logo.png")).unwrap();
    finish_picture_preparation(&mut reader);
    let mut screen = Screen::sized(40, 10);

    // The terminal receives no bitmap before advertising Kitty support.
    reader.draw(&mut screen);
    assert!(screen.picture_at().is_none());
    assert!(screen.text()[0].ends_with("64×32"));

    // 64 by 32 pixels in cells of 8 by 16 is 8 columns by 2 rows.
    screen.set_pictures(true);
    reader.draw(&mut screen);
    assert_eq!(screen.picture_at(), Some((16, 4, 8, 2)));

    let mut plain = Screen::sized(40, 10);
    plain.answered();
    reader.draw(&mut plain);
    assert!(plain.picture_at().is_none());
    if cfg!(target_os = "macos") {
        assert_eq!(plain.text()[4], format!("{}{}", " ".repeat(4), "▀".repeat(32)));
        assert_eq!(plain.text()[5], plain.text()[4]);
    } else {
        assert!(plain.text()[2].contains("Kitty"));
    }
}

#[test]
fn a_parked_image_releases_loading_and_decoded_media_until_reopened() {
    let dir = Fixture::new();
    let file = dir.root.join("logo.png");
    fs::write(&file, Picture::sample_png(64, 32)).unwrap();
    let mut reader = Reader::open(&file).unwrap();
    assert!(reader.next_frame().is_some());

    reader.park_media();
    assert!(matches!(&reader.body, Body::Note(_)));
    assert_eq!(reader.next_frame(), None);

    dir.write(".git/index.lock", "");
    assert!(!reader.git_busy());
    assert!(reader.check());
    assert!(reader.next_frame().is_some());
    finish_picture_preparation(&mut reader);
    assert!(matches!(&reader.body, Body::Picture(picture) if picture.width == 64));

    reader.park_media();
    assert!(matches!(&reader.body, Body::Note(_)));
    assert_eq!(reader.next_frame(), None);
}

#[test]
fn canceled_source_reads_keep_their_slots_until_they_return() {
    use std::os::unix::fs::FileExt as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    static SLOTS: AtomicUsize = AtomicUsize::new(0);
    let f = Fixture::folder();
    let open = |path, width| {
        let file = f.root.join(path);
        fs::write(&file, Picture::sample_png(width, 16)).unwrap();
        let (entered, at_read) = mpsc::channel();
        let (release, ready) = mpsc::channel();
        media_load::intercept_next_read(&SLOTS, Box::new(move |file, bytes, offset| {
            entered.send(()).unwrap();
            // Simulate an in-progress read that cannot observe cancellation
            // until I/O returns. Time out only to keep a broken test from hanging.
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            file.read_at(bytes, offset)
        }));
        (Reader::open(&file).unwrap(), at_read, release)
    };

    let (mut first, at_first, release_first) = open("first.png", 64);
    at_first.recv_timeout(Duration::from_secs(3)).unwrap();
    first.park_media();
    assert_eq!(first.next_frame(), None);

    let (second, at_second, release_second) = open("second.png", 48);
    at_second.recv_timeout(Duration::from_secs(3)).unwrap();
    drop(second);

    let (mut last, at_last, release_last) = open("latest.png", 32);
    last.tick_media();
    assert_eq!(at_last.recv_timeout(Duration::from_millis(100)), Err(mpsc::RecvTimeoutError::Timeout));
    assert_eq!(SLOTS.load(Ordering::Relaxed), 2);
    assert!(matches!(&last.body, Body::Note(note) if *note == media_load::PENDING));

    release_first.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while at_last.recv_timeout(Duration::from_millis(10)) == Err(mpsc::RecvTimeoutError::Timeout) {
        assert!(Instant::now() < deadline, "a released source slot was not reused");
        last.tick_media();
    }
    release_last.send(()).unwrap();
    finish_picture_preparation(&mut last);
    assert!(matches!(&last.body, Body::Picture(picture) if picture.width == 32));
    first.tick_media();
    assert!(matches!(&first.body, Body::Note(_)), "a parked reader displayed a late result");
    release_second.send(()).unwrap();
}

#[test]
fn large_text_with_media_suffixes_keeps_its_patch_without_loading_fallback_source() {
    use crate::read::Content;

    let f = Fixture::new();
    let paths = ["notes.mp4", "notes.png"];
    let line = "x".repeat(1023) + "\n";
    let mut contents = String::with_capacity((3 << 20) + 7);
    contents.push_str("before\n");
    for _ in 0..3 * 1024 {
        contents.push_str(&line);
    }
    for path in paths {
        f.write(path, &contents);
    }
    f.commit("base");

    contents.replace_range(..6, "edited");
    for path in paths {
        f.write(path, &contents);
        let mut reader = Reader::open(&f.root.join(path)).unwrap();
        finish_picture_preparation(&mut reader);
        let rows = drawn(&mut reader, 60, 8).join("\n");
        assert!(rows.contains("- before") && rows.contains("+ edited"), "{path}: {rows}");
        let Source::Working(work) = &reader.source else { unreachable!() };
        assert!(matches!(&work.media, Some(media_load::State::Text { content: Content::TooLarge, .. })));
        assert_eq!(reader.read_only, Some(ReadOnly::TOO_LARGE));
    }
}

#[test]
fn oversize_media_with_a_text_prefix_is_refused_before_git_reads_it() {
    let f = Fixture::new();
    let path = "notes.mp4";
    f.write(path, "before\n");
    f.commit("base");
    f.write(path, &"x".repeat(512));
    let file = f.root.join(path);
    // Make the file look like text to the source sample but binary to Git.
    fs::OpenOptions::new().write(true).open(&file).unwrap().set_len(crate::git::DIFF_LIMIT + 1).unwrap();

    let mut reader = Reader::open(&file).unwrap();
    finish_picture_preparation(&mut reader);
    assert!(matches!(&reader.body, Body::Note(note) if *note == "Too large to show."));
}

#[test]
fn watched_binary_status_changes_do_not_rebuild_the_preview() {
    use crate::pane::{check_watched_pane, watch_pane};

    let f = Fixture::new();
    f.write("keep.txt", "base\n");
    f.commit("base");
    let path = "logo.png";
    let file = f.root.join(path);
    fs::write(&file, Picture::sample_png(16, 8)).unwrap();
    let mut reader = Reader::open(&file).unwrap();
    finish_picture_preparation(&mut reader);
    assert_eq!(reader.kind, Kind::Untracked);
    let picture_id = |reader: &Reader| match &reader.body {
        Body::Picture(picture) => picture.id,
        _ => panic!("binary status replaced the picture"),
    };
    let id = picture_id(&reader);
    let stamp = working_path::Stamp::at(&file);
    let (mut watcher, mut pace) = watch_pane(&mut reader, false);
    f.git(&["add", "--", path]);
    assert!(check_watched_pane(&mut reader, &mut watcher, &mut pace, false));
    assert_eq!(picture_id(&reader), id);
    finish_picture_preparation(&mut reader);
    assert_eq!(reader.kind, Kind::Added);
    f.git(&["commit", "-qm", "add logo"]);
    assert!(check_watched_pane(&mut reader, &mut watcher, &mut pace, false));
    finish_picture_preparation(&mut reader);
    assert_eq!(reader.kind, Kind::None);
    assert!(working_path::Stamp::at(&file) == stamp);
    assert_eq!(picture_id(&reader), id);

    f.git(&["checkout", "-qb", "theirs"]);
    fs::write(&file, Picture::sample_png(24, 8)).unwrap();
    f.commit("their logo");
    f.git(&["checkout", "-q", "main"]);
    fs::write(&file, Picture::sample_png(32, 8)).unwrap();
    f.commit("our logo");
    fs::write(&file, Picture::sample_png(40, 8)).unwrap();
    assert!(check_watched_pane(&mut reader, &mut watcher, &mut pace, false));
    finish_picture_preparation(&mut reader);
    assert_eq!(reader.kind, Kind::Modified);
    let id = picture_id(&reader);
    let stamp = working_path::Stamp::at(&file);
    // Merge only the index; leave the working image and its preview alone.
    f.git(&["read-tree", "-i", "-m", "main^", "main", "theirs"]);
    assert!(check_watched_pane(&mut reader, &mut watcher, &mut pace, false));
    finish_picture_preparation(&mut reader);
    assert!(working_path::Stamp::at(&file) == stamp);
    assert_eq!(reader.kind, Kind::Conflict);
    assert_eq!(picture_id(&reader), id);
}

#[test]
fn a_movie_gets_worktree_status_without_starting_playback() {
    let f = Fixture::new();
    let file = f.root.join("clip.mp4");
    f.write("clip.mp4", "previous version\n");
    f.commit("original");
    fs::write(&file, include_bytes!("../../tests/rex_lab/river-motion.mp4")).unwrap();
    let mut reader = Reader::open(&file).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while reader.kind == Kind::None {
        assert!(Instant::now() < deadline, "movie status did not finish");
        // Advance acquisition and status but do not start FFmpeg/FFprobe.
        reader.tick_media();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(reader.kind, Kind::Modified);
}

#[test]
fn a_replaced_image_cannot_publish_its_old_preview() {
    let dir = Fixture::folder();
    let file = dir.root.join("logo.png");
    let replacement = dir.root.join("replacement.png");
    fs::write(&file, Picture::sample_png(64, 32)).unwrap();
    fs::write(&replacement, Picture::sample_png(32, 16)).unwrap();
    let mut reader = Reader::open(&file).unwrap();
    assert!(reader.next_frame().is_some());
    fs::rename(replacement, file).unwrap();

    // No filesystem check: completion itself must validate the captured stamp.
    finish_picture_preparation(&mut reader);
    assert!(matches!(&reader.body, Body::Picture(picture) if picture.width == 32));
}

#[test]
fn a_link_named_like_a_picture_is_shown_as_where_it_points() {
    let dir = Fixture::folder();
    fs::write(dir.root.join("../outside.png"), Picture::sample_png(8, 8)).unwrap();
    symlink("../outside.png", dir.root.join("logo.png")).unwrap();
    let mut reader = Reader::open(&dir.root.join("logo.png")).unwrap();
    let mut screen = Screen::sized(40, 6);
    screen.set_pictures(true);
    reader.draw(&mut screen);
    assert!(screen.picture_at().is_none());
    assert_eq!(screen.text()[1], " 1   ../outside.png");
    fs::remove_file(dir.root.join("../outside.png")).unwrap();
}

#[test]
fn the_deleted_path_of_a_link_named_like_a_picture_is_still_visible() {
    let f = Fixture::new();
    let link = f.root.join("logo.png");
    symlink("../previous.png", &link).unwrap();
    f.commit("link");
    fs::remove_file(&link).unwrap();
    let mut reader = Reader::open(&link).unwrap();
    let rows = drawn(&mut reader, 40, 6);
    assert_eq!(rows[1], "   - ../previous.png");
}

#[test]
fn a_directory_link_cannot_take_the_reader_out_of_the_project() {
    let project = Fixture::new();
    let outside = Fixture::folder();
    outside.write("page.txt", "outside the project\n");
    let link = project.root.join("shortcut");
    symlink(&outside.root, &link).unwrap();
    assert!(Reader::open(&link.join("page.txt")).is_none());
    assert!(Reader::open(&outside.root.join("page.txt")).is_some());
}

#[test]
fn an_unreadable_file_is_not_reported_as_gone() {
    let dir = Fixture::folder();
    let file = dir.root.join("text");
    fs::write(&file, "content").unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o0)).unwrap();
    // Users who can bypass file permissions can still read this file.
    if fs::File::open(&file).is_ok() {
        return;
    }
    let mut reader = Reader::open(&file).unwrap();
    assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), "This file cannot be read.");
    fs::remove_file(&file).unwrap();
    let mut gone = Reader::open(&file).unwrap();
    assert_eq!(drawn(&mut gone, 60, 6)[2].trim_start(), "This file is gone.");
}

#[test]
fn an_unreadable_picture_explains_why_it_cannot_be_shown() {
    let f = Fixture::folder();
    let file = f.root.join("logo.png");
    fs::write(&file, Picture::sample_png(32, 32)).unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o0)).unwrap();
    if fs::File::open(&file).is_ok() {
        return;
    }
    let mut reader = Reader::open(&file).unwrap();
    finish_picture_preparation(&mut reader);
    assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), "This file cannot be read.");
}

#[test]
fn single_file_commit_reads_distinguish_rename_destinations_deletions_and_unchanged_files() {
    let f = Fixture::new();
    let shared = "first retained line\nsecond retained line\nthird retained line\n";
    f.write("old.txt", &format!("{shared}before\n"));
    f.write("gone.txt", "removed content\n");
    f.write("unchanged.txt", "stored content\n");
    f.commit("first");
    fs::rename(f.root.join("old.txt"), f.root.join("new.txt")).unwrap();
    fs::remove_file(f.root.join("gone.txt")).unwrap();
    f.write("new.txt", &format!("{shared}after\n"));
    f.commit("rename and remove");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();

    for (path, expected) in [
        ("new.txt", &[" 1   first retained line", "   - before", " 4 + after"][..]),
        ("gone.txt", &["   - removed content"][..]),
        ("unchanged.txt", &[" 1   stored content"][..]),
        ("old.txt", &["  There is no file here in this commit."][..]),
    ] {
        let mut reader = Reader::at_commit(&f.root.join(path), id).unwrap();
        let rows = drawn(&mut reader, 60, 8);
        assert!(expected.iter().all(|&row| rows.iter().any(|r| r == row)), "{path}: {rows:?}");
    }
}

#[test]
fn a_non_utf8_working_path_never_uses_its_lossy_sibling() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let f = Fixture::new();
    f.write("page\u{fffd}.txt", "original sibling\n");
    f.commit("tracked sibling");
    let sibling = f.root.join("page\u{fffd}.txt");
    let raw = f.root.join(OsStr::from_bytes(b"page\xff.txt"));
    f.write("page\u{fffd}.txt", "edited sibling\n");
    let mut reader = Reader::open(&raw).unwrap();
    let (after, opened) = with_fake(Ok(()), || reader.key(Key::Ctrl(b'o')));
    assert_eq!(after, After::Stay);
    assert!(opened.is_empty());
    for (kind, expected) in [(Kind::Modified, "edited sibling"), (Kind::Deleted, "original sibling")] {
        if kind == Kind::Deleted {
            fs::remove_file(&sibling).unwrap();
            reader.refresh(true);
        }
        let mut sibling_reader = Reader::open(&sibling).unwrap();
        assert_eq!(sibling_reader.kind, kind);
        assert!(drawn(&mut sibling_reader, 60, 6).iter().any(|row| row.ends_with(expected)));
        assert!(matches!(reader.body, Body::Note(_)));
    }
}

#[test]
fn a_non_utf8_commit_path_cannot_select_a_different_file() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let project = Fixture::new();
    project.write("page\u{fffd}.txt", "a different file\n");
    project.commit("valid path");
    let mut repo = Repo::open(&project.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    let invalid = project.root.join(OsStr::from_bytes(b"page\xff.txt"));
    let outside = Fixture::folder();
    for cwd in [&project.root, &outside.root] {
        assert_eq!(
            CommittedTarget::resolve_from(cwd, &invalid, id).err(),
            Some("File paths at a commit must be UTF-8."),
        );
    }
}

#[test]
fn a_single_file_commit_read_does_not_treat_a_shallow_boundary_as_a_root() {
    let f = Fixture::new();
    f.write("page.txt", "already there\n");
    f.commit("first");
    f.write("new.txt", "just added\n");
    f.commit("second");
    f.git(&["clone", "-q", "--depth=1", &format!("file://{}", f.root.display()), "shallow"]);
    let root = f.root.join("shallow");
    let mut repo = Repo::open(&root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();

    assert_eq!(Reader::at_commit(&root.join("page.txt"), id).err(), Some("Git did not answer for this commit."));
}

/// The styled runs of a frame: the codes of each style and the text drawn with it.
fn runs(out: &str) -> Vec<(&str, &str)> {
    let codes = |run: &(&str, &str)| run.0.bytes().all(|b| b.is_ascii_digit() || b == b';');
    out.split("\x1b[").filter_map(|run| run.split_once('m')).filter(codes).collect()
}

/// Whether a style inverts its cells. A 7 inside an extended colour is not the code for that.
fn reversed(codes: &str) -> bool {
    let mut codes = codes.split(';');
    while let Some(code) = codes.next() {
        match code {
            "7" => return true,
            "38" | "48" => {
                let values = if codes.next() == Some("5") { 1 } else { 3 };
                codes.nth(values - 1);
            }
            _ => {}
        }
    }
    false
}

#[test]
fn lua_swift_sql_dockerfiles_and_makefiles_are_coloured_in_working_and_committed_reads() {
    let f = Fixture::new();
    let files = [
        ("init.lua", "local text = [==[\n  long ]] text\n]==]\nreturn text .. 1\n", "local"),
        ("App.swift", "import Foundation\nlet raw = #\"a \"b\" c\"#\nlet count = 1\n", "import"),
        ("db/schema.sql", "select id\n  from users -- all\n limit 1;\n", "select"),
        ("ops.d/Dockerfile", "FROM alpine:3 AS base\n# note\nRUN echo 1\n", "FROM"),
        ("Makefile", "include rules.mk\nall: include/x.h\n\techo 1\n", "include"),
    ];
    for (path, before, _) in files {
        f.write(path, before);
    }
    f.commit("before");
    for (path, before, _) in files {
        f.write(path, &before.replace('1', "2"));
    }
    let check = |reader: &mut Reader, path: &str, before: &str, keyword: &str| {
        let mut screen = Screen::sized(60, 10);
        reader.draw(&mut screen);
        // Git's text is shown as it is, apart from the tab stops every reader expands.
        let rows = screen.text();
        for line in before.lines().chain(before.replace('1', "2").lines()) {
            let line = line.replace('\t', "    ");
            assert!(rows.iter().any(|row| row.ends_with(&line)), "{path}: {line:?} not in {rows:?}");
        }
        let out = String::from_utf8(screen.emitted()).unwrap();
        let runs = runs(&out);
        assert!(runs.contains(&("0;35", keyword)), "{path}: {out:?}");
        // Only the changed number is emphasised, on both the removed and the added line.
        let emphasised: Vec<_> = runs.iter().filter(|run| reversed(run.0)).map(|run| run.1).collect();
        assert_eq!(emphasised, ["1", "2"], "{path}: {out:?}");
    };
    for (path, before, keyword) in files {
        check(&mut Reader::open(&f.root.join(path)).unwrap(), path, before, keyword);
    }

    f.commit("after");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    for (path, before, keyword) in files {
        check(&mut Reader::at_commit(&f.root.join(path), id).unwrap(), path, before, keyword);
    }
}

#[test]
fn markdown_markup_stylesheets_and_more_code_are_coloured_in_working_and_committed_reads() {
    let f = Fixture::new();
    let files = [
        ("README.md", "# Notes\n\nDon't skip `code` in step 1.\n", "# Notes"),
        ("site/index.html", "<main class=\"a\">\n  <p>Don't stop at 1</p>\n</main>\n", "main"),
        ("theme.css", "@media print {\n  a { z-index: 1; }\n}\n", "@media"),
        ("app.rb", "def run\n  puts 1\nend\n", "def"),
        ("Main.kt", "fun main() {\n  println(1)\n}\n", "fun"),
        ("main.tf", "variable \"n\" {\n  default = 1\n}\n", "variable"),
    ];
    for (path, before, _) in files {
        f.write(path, before);
    }
    f.commit("before");
    for (path, before, _) in files {
        f.write(path, &before.replace('1', "2"));
    }
    let check = |reader: &mut Reader, path: &str, keyword: &str| {
        let mut screen = Screen::sized(60, 10);
        reader.draw(&mut screen);
        let out = String::from_utf8(screen.emitted()).unwrap();
        let runs = runs(&out);
        assert!(runs.contains(&("0;35", keyword)), "{path}: {out:?}");
        // The diff still marks only the changed number, on the removed and the added line.
        let emphasised: Vec<_> = runs.iter().filter(|run| reversed(run.0)).map(|run| run.1).collect();
        assert_eq!(emphasised, ["1", "2"], "{path}: {out:?}");
    };
    for (path, _, keyword) in files {
        let mut reader = Reader::open(&f.root.join(path)).unwrap();
        if path == "README.md" {
            let preview = drawn(&mut reader, 60, 10).join("\n");
            assert!(preview.contains("Notes") && !preview.contains("# Notes"), "{preview}");
            assert!(!preview.contains("step 1"), "deleted text leaked into preview: {preview}");
            reader.key(Key::Char('m'));
        }
        check(&mut reader, path, keyword);
    }

    f.commit("after");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    for (path, _, keyword) in files {
        check(&mut Reader::at_commit(&f.root.join(path), id).unwrap(), path, keyword);
    }
}

#[test]
fn each_changed_word_is_emphasised_and_stays_in_place_when_panning() {
    let f = Fixture::new();
    f.write("page.txt", "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj kkkk\n");
    f.commit("before");
    f.write("page.txt", "aaaa BBBB cccc dddd eeee ffff GGGG hhhh iiii jjjj kkkk\n");
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();

    let mut screen = Screen::sized(60, 5);
    reader.draw(&mut screen);
    let out = String::from_utf8(screen.emitted()).unwrap();
    let emphasised = |out: &str| -> Vec<String> {
        runs(out).iter().filter(|run| reversed(run.0)).map(|run| format!("{} {}", run.0, run.1)).collect()
    };
    assert_eq!(emphasised(&out), ["0;7;31 bbbb", "0;7;31 gggg", "0;7;32 BBBB", "0;7;32 GGGG"]);

    // With the first word panned away and the second cut by the left edge, the rest of it keeps its emphasis.
    let mut screen = Screen::sized(24, 5);
    reader.draw(&mut screen);
    for _ in 0..4 {
        reader.key(Key::Right);
        reader.draw(&mut screen);
    }
    let out = String::from_utf8(screen.emitted()).unwrap();
    assert!(screen.text()[2].contains("+‹GG hhhh"), "{:?}", screen.text());
    assert_eq!(emphasised(&out).last().map(String::as_str), Some("0;7;32 GG"), "{out:?}");
}

#[test]
fn a_wide_edit_clipped_at_the_right_edge_does_not_highlight_empty_cells() {
    let f = Fixture::new();
    f.write("page.txt", "abcd界 shared\n");
    f.commit("before");
    f.write("page.txt", "abcd新 shared\n");
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    for (width, expected) in [(11, &[][..]), (12, &["界", "新"][..])] {
        let mut screen = Screen::sized(width, 5);
        reader.draw(&mut screen);
        let out = String::from_utf8(screen.emitted()).unwrap();
        let emphasised: Vec<_> = runs(&out).iter().filter(|run| reversed(run.0)).map(|run| run.1).collect();
        assert_eq!(emphasised, expected, "{out:?}");
    }
}

#[test]
fn text_with_mp4_suffix_keeps_its_working_diff() {
    let f = Fixture::new();
    f.write("notes.mp4", "before\n");
    f.commit("text");
    f.write("notes.mp4", "after\n");
    let mut reader = Reader::open(&f.root.join("notes.mp4")).unwrap();
    finish_picture_preparation(&mut reader);
    let lines = drawn(&mut reader, 60, 8).join("\n");
    assert!(lines.contains("before") && lines.contains("after"), "{lines}");
}

#[test]
fn editing_rendered_markdown_keeps_the_inactive_source_viewport() {
    let f = Fixture::new();
    let notes: String = (0..48).map(|i| format!("## Marker {i:02} with enough text to require horizontal panning\n")).collect();
    f.write("notes.md", &notes);
    let mut reader = Reader::open(&f.root.join("notes.md")).unwrap();
    reader.key(Key::Char('m'));
    drawn(&mut reader, 32, 8);
    reader.key(Key::End);
    reader.key(Key::Right);
    let source = drawn(&mut reader, 32, 8);
    assert!(source.iter().any(|line| line.contains("‹")), "{source:?}");
    reader.key(Key::Char('m'));
    let preview = drawn(&mut reader, 32, 8);
    assert!(preview.iter().any(|line| line.contains("Marker 00")));
    reader.key(Key::Char('e'));
    assert!(reader.editing());
    reader.key(Key::Esc);
    reader.key(Key::Char('m'));
    assert_eq!(drawn(&mut reader, 32, 8), source);
}

#[test]
fn returning_to_suffix_text_keeps_the_draft_top_after_the_diff_loads() {
    use std::os::unix::fs::FileExt as _;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    let f = Fixture::new();
    let notes = (0..90).map(|i| format!("note {i:02}\n")).collect::<String>();
    f.write("notes.png", &notes);
    f.commit("notes");
    let mut reader = Reader::open(&f.root.join("notes.png")).unwrap();
    let loaded = |reader: &mut Reader| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let rows = drawn(reader, 60, 8);
            if !rows.iter().any(|row| row.contains(media_load::PENDING)) {
                return rows;
            }
            assert!(Instant::now() < deadline, "text preview did not finish: {rows:?}");
            std::thread::sleep(Duration::from_millis(30));
        }
    };
    let pause_reload = || {
        static SLOTS: AtomicUsize = AtomicUsize::new(0);
        let (resume, blocked) = mpsc::channel::<()>();
        media_load::intercept_next_read(&SLOTS, Box::new(move |file, bytes, offset| {
            let _ = blocked.recv();
            file.read_at(bytes, offset)
        }));
        resume
    };
    let pending = |reader: &mut Reader, resume: mpsc::Sender<()>| {
        let rows = drawn(reader, 60, 8);
        resume.send(()).unwrap();
        assert!(rows.iter().any(|row| row.contains(media_load::PENDING)), "{rows:?}");
    };

    let rows = loaded(&mut reader);
    assert!(rows[1].contains("note 00"), "{rows:?}");
    for _ in 0..20 {
        reader.key(Key::Down);
    }
    reader.key(Key::Char('e'));
    assert!(reader.editing());
    assert!(drawn(&mut reader, 60, 8)[1].contains("note 20"));
    assert_eq!(reader.key(Key::WheelDown), After::Stay);
    assert!(drawn(&mut reader, 60, 8)[1].contains("note 23"));
    reader.key(Key::Esc);
    assert!(drawn(&mut reader, 60, 8)[1].contains("note 23"));

    reader.key(Key::Char('e'));
    assert!(reader.editing());
    f.write("notes.png", &notes.replace("note 23\n", "outside note 23\n"));
    let resume = pause_reload();
    reader.key(Key::Esc);
    pending(&mut reader, resume);
    let rows = loaded(&mut reader);
    assert!(rows[1].contains("+ outside note 23"), "{rows:?}");

    let mut top = "outside note 23".to_owned();
    for (ch, close_with_save) in [('!', true), ('?', false)] {
        reader.key(Key::Char('e'));
        assert!(reader.editing());
        reader.key(Key::Char(ch));
        top.insert(0, ch);
        assert!(drawn(&mut reader, 60, 8)[1].contains(&top), "{top}");
        let resume = pause_reload();
        if close_with_save {
            reader.key(Key::Esc);
            reader.key(Key::Char('s'));
        } else {
            reader.key(Key::Ctrl(b's'));
            assert!(reader.editing());
            reader.key(Key::Esc);
        }
        assert!(!reader.editing());
        pending(&mut reader, resume);
        let rows = loaded(&mut reader);
        assert!(rows[1].contains(&format!("+ {top}")), "{rows:?}");
    }
}

#[test]
fn editing_a_markdown_preview_keeps_a_deleted_row_at_the_top_of_the_folded_source() {
    let f = Fixture::new();
    let notes = (1..=60).map(|i| format!("## Section {i:02}\n")).collect::<String>();
    f.write("notes.md", &notes);
    f.commit("notes");
    let edited = notes.strip_prefix("## Section 01\n").unwrap().replace("Section 30", "Edited section 30");
    f.write("notes.md", &edited);
    let mut reader = Reader::open(&f.root.join("notes.md")).unwrap();

    assert_eq!(reader.key(Key::Char('m')), After::Stay);
    let full = drawn(&mut reader, 60, 8);
    assert!(full[1].contains("- ## Section 01"), "{full:?}");
    assert_eq!(reader.key(Key::Char('c')), After::Stay);
    let compact = drawn(&mut reader, 60, 8);
    assert!(compact[1].contains("- ## Section 01"), "{compact:?}");
    assert!(compact.iter().any(|row| row.contains("unchanged lines")), "{compact:?}");

    assert_eq!(reader.key(Key::Char('m')), After::Stay);
    let preview = drawn(&mut reader, 60, 8);
    assert!(preview.iter().any(|row| row.contains("Section 02")), "{preview:?}");
    reader.key(Key::Char('e'));
    assert!(reader.editing());
    reader.key(Key::Esc);
    assert_eq!(reader.key(Key::Char('m')), After::Stay);
    assert_eq!(drawn(&mut reader, 60, 8), compact);

    // A notification can rebuild the same patch without changing its row identity.
    reader.filesystem_changed();
    reader.check();
    assert_eq!(drawn(&mut reader, 60, 8), compact);
    reader.key(Key::Char('e'));
    assert!(reader.editing());
    reader.key(Key::Esc);
    assert_eq!(drawn(&mut reader, 60, 8), compact);
    assert_eq!(reader.key(Key::Char('c')), After::Stay);
    assert_eq!(drawn(&mut reader, 60, 8), full);
}

#[test]
fn only_explicit_follow_tracks_the_end_of_working_markdown() {
    let f = Fixture::folder();
    f.write("log.markdown", "");
    let path = f.root.join("log.markdown");
    let mut reader = Reader::open(&path).unwrap();
    assert!(drawn(&mut reader, 40, 5)[1].contains("No Markdown content"));
    reader.key(Key::Char('G'));
    assert!(drawn(&mut reader, 40, 5)[0].contains("Following end"));
    assert!(reader.next_frame().is_none(), "follow should sleep between saves");

    let entries: String = (1..=20).map(|n| format!("- Entry {n}\n")).collect();
    f.write("tmp", &entries);
    fs::rename(f.root.join("tmp"), &path).unwrap();
    reader.check();
    assert!(drawn(&mut reader, 40, 5)[3].contains("Entry 20"));
    assert!(reader.next_frame().is_some(), "a changed block needs an expiry frame");

    // End at the bottom is still manual navigation, even though no row moves.
    assert_eq!(reader.key(Key::End), After::Stay);
    f.write("log.markdown", &format!("{entries}- Entry 21\n"));
    reader.check();
    let rows = drawn(&mut reader, 40, 5);
    assert!(!rows[0].contains("Following") && rows[3].contains("Entry 20"), "{rows:?}");
    reader.key(Key::Char('G'));
    assert!(drawn(&mut reader, 28, 4)[2].contains("Entry 21"));
    reader.key(Key::Char('m'));
    assert!(reader.next_frame().is_none(), "source has no live marks");
    reader.key(Key::Char('m'));
    assert!(!drawn(&mut reader, 40, 5)[0].contains("Following"));
}

#[test]
fn a_truncated_working_markdown_keeps_the_anchor_of_the_last_nonempty_preview() {
    let f = Fixture::new();
    let entries: String = (1..=20).map(|n| format!("## Entry {n:02}\n")).collect();
    f.write("notes.md", &entries);
    f.commit("log");
    let mut reader = Reader::open(&f.root.join("notes.md")).unwrap();
    drawn(&mut reader, 40, 5);
    for _ in 0..8 {
        reader.key(Key::Down);
    }
    assert!(drawn(&mut reader, 40, 5)[1].starts_with("Entry 09"));

    f.write("notes.md", "");
    reader.check();
    let rows = drawn(&mut reader, 40, 5);
    assert_eq!(rows[1], "No Markdown content.");
    assert!(!rows.join("\n").contains("Entry"), "{rows:?}");
    assert!(reader.next_frame().is_none());
    assert_eq!(reader.key(Key::Char('m')), After::Stay);
    assert!(drawn(&mut reader, 40, 5)[1].contains("- ## Entry 01"));
    assert_eq!(reader.key(Key::Char('m')), After::Stay);

    f.write("notes.md", &format!("## Inserted above\n{entries}"));
    reader.check();
    assert!(drawn(&mut reader, 40, 5)[1].starts_with("Entry 09"));
}

#[test]
fn a_missing_or_linked_working_markdown_never_draws_its_cached_preview() {
    let f = Fixture::folder();
    let entries: String = (1..=20).map(|n| format!("## Entry {n:02}\n")).collect();
    f.write("notes.md", &entries);
    let path = f.root.join("notes.md");
    let mut reader = Reader::open(&path).unwrap();
    drawn(&mut reader, 40, 5);
    reader.key(Key::Char('G'));
    f.write("notes.md", &format!("{entries}## Last saved\n"));
    reader.check();
    assert!(drawn(&mut reader, 40, 5)[3].contains("Last saved"));
    assert!(reader.next_frame().is_some());

    fs::remove_file(&path).unwrap();
    reader.check();
    let gone = drawn(&mut reader, 40, 5).join("\n");
    assert!(gone.contains("This file is gone.") && !gone.contains("Following") && !gone.contains("Entry"), "{gone}");
    assert!(reader.next_frame().is_none());

    f.write("other.md", "Not the working document.\n");
    symlink("other.md", &path).unwrap();
    reader.check();
    let link = drawn(&mut reader, 40, 5).join("\n");
    assert!(link.contains("other.md") && !link.contains("Following") && !link.contains("Entry"), "{link}");
    assert!(!link.contains("Not the working document."), "{link}");
    assert!(reader.next_frame().is_none());

    fs::remove_file(&path).unwrap();
    f.write("notes.md", &format!("{entries}## Last saved\n## Restored\n"));
    reader.check();
    let rows = drawn(&mut reader, 40, 5);
    assert!(rows[0].contains("Following end") && rows[3].contains("Restored"), "{rows:?}");
}

#[test]
fn a_markdown_revision_and_a_handed_patch_do_not_follow_the_working_file() {
    let f = Fixture::new();
    f.write("notes.md", "# Before\n");
    f.commit("before");
    let path = f.root.join("notes.md");
    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let history = repo.history(1).unwrap();
    let revision = Reader::at_commit(&path, CommitId::parse(&history[0].hash).unwrap()).unwrap();
    f.write("notes.md", "# After\n");
    let patch =
        crate::handoff::Patch::parse("--- a/notes.md\n+++ b/notes.md\n@@ -1 +1 @@\n-# Before\n+# After\n").unwrap();
    let handed = Reader::handed_over(&repo, &patch, "notes.md", Handed { from: None, number: 1, total: 1 });
    f.write("notes.md", "# Unrelated save\n");
    for (mut reader, expected) in [(revision, "# Before"), (handed, "+ # After")] {
        reader.key(Key::Char('G'));
        reader.filesystem_changed();
        reader.check();
        let frame = drawn(&mut reader, 60, 6).join("\n");
        assert!(
            frame.contains(expected) && !frame.contains("Unrelated save") && !frame.contains("Following"),
            "{frame}"
        );
        assert!(reader.next_frame().is_none());
    }
}
