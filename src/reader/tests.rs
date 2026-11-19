use std::fs::{self, Permissions};
use std::os::unix::fs::{PermissionsExt, symlink};

use super::content::NOT_TEXT;
use super::*;
use crate::fixture::Fixture;
use crate::git::CommitId;

fn drawn(reader: &mut Reader, w: usize, h: usize) -> Vec<String> {
    let mut screen = Screen::sized(w, h);
    reader.draw(&mut screen);
    screen.text()
}

#[test]
fn reader_hints_say_close_and_keep_help_last() {
    let f = Fixture::folder();
    f.write("page.txt", "a file\n");
    let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
    assert_eq!(drawn(&mut reader, 40, 5)[4], " ↑↓ scroll · q close · ? help");
    assert_eq!(drawn(&mut reader, 20, 5)[4], " q close · ? help");
}

#[test]
fn an_empty_file_uses_the_body_row_not_the_hint_row() {
    let f = Fixture::folder();
    f.write("empty.txt", "");
    let mut reader = Reader::open(&f.root.join("empty.txt")).unwrap();
    let rows = drawn(&mut reader, 40, 3);
    assert_eq!(&rows[1..], ["  Empty.", " q close · ? help"]);
}

#[test]
fn text_with_a_picture_extension_still_shows_its_edits() {
    let f = Fixture::new();
    f.write("notes.png", "initial notes\n");
    f.commit("first");
    f.write("notes.png", "new notes\n");
    let file = f.root.join("notes.png");
    let mut working = Reader::open(&file).unwrap();
    assert_eq!(&drawn(&mut working, 60, 6)[1..3], ["   - initial notes", " 1 + new notes"]);

    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    let mut committed = Reader::at_commit(&file, id).unwrap();
    assert_eq!(drawn(&mut committed, 60, 6)[1], " 1 + initial notes");
}

#[test]
fn an_empty_picture_file_is_empty_not_a_broken_picture() {
    let f = Fixture::folder();
    f.write("empty.png", "");
    let mut reader = Reader::open(&f.root.join("empty.png")).unwrap();
    assert_eq!(drawn(&mut reader, 40, 3)[1], "  Empty.");
}

#[test]
fn a_narrow_reader_keeps_a_file_name_and_shows_its_text_when_there_is_a_body_row() {
    let f = Fixture::folder();
    f.write("test.txt", "hi\n");
    let mut reader = Reader::open(&f.root.join("test.txt")).unwrap();
    assert!(drawn(&mut reader, 40, 1)[0].ends_with("test.txt"));
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
    for (line, key) in [(8, 'n'), (45, 'N'), (8, 'n')] {
        let rows = drawn(&mut reader, 45, 12);
        assert!(rows.iter().any(|row| row.contains(&format!("+ row {line} edited"))), "{rows:?}");
        reader.key(Key::Char(key));
    }
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
fn jumping_wraps_even_when_two_changes_share_the_last_page_top() {
    let mut kinds = [LineKind::Same; 100];
    kinds[9] = LineKind::Del;
    kinds[10] = LineKind::Add;
    kinds[94] = LineKind::Add;
    kinds[96] = LineKind::Del;
    use Direction::{Next, Previous};
    let text = Text::new((1..).zip(kinds).map(|(number, kind)| Line::new(kind, number, "")).collect(), None);
    for (top, direction, expected) in [(6, Next, 82), (82, Next, 6), (6, Previous, 82), (82, Previous, 6)] {
        assert_eq!(text.change_top(top, 82, direction), Some(expected));
    }
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
fn a_picture_is_shown_in_the_middle_at_its_own_size_where_the_terminal_can() {
    let dir = Fixture::folder();
    fs::write(dir.root.join("logo.png"), Picture::sample_png(64, 32)).unwrap();
    let mut reader = Reader::open(&dir.root.join("logo.png")).unwrap();
    let mut screen = Screen::sized(40, 10);

    // Until the terminal has said, nothing is claimed either way.
    reader.draw(&mut screen);
    assert!(screen.picture_at().is_none() && screen.text()[2].is_empty());
    assert!(screen.text()[0].ends_with("64×32"));

    // 64 by 32 pixels in cells of 8 by 16 is 8 columns by 2 rows.
    screen.set_pictures(true);
    reader.draw(&mut screen);
    assert_eq!(screen.picture_at(), Some((16, 4, 8, 2)));

    let mut plain = Screen::sized(40, 10);
    plain.answered();
    reader.draw(&mut plain);
    assert!(plain.picture_at().is_none());
    assert_eq!(plain.text()[2], "  This terminal cannot show pictures.");
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
    assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), "This file cannot be read.");
}
