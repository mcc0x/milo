use std::fs::{self, Permissions};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::PathBuf;

use super::*;
use crate::fixture::Fixture;
use crate::git::CommitId;

fn drawn(reader: &mut Reader, w: usize, h: usize) -> Vec<String> {
    let mut screen = Screen::sized(w, h);
    reader.draw(&mut screen);
    screen.text()
}

/// A state folder inside the fixture, so that a kept draft never reaches the developer's own.
fn state(f: &Fixture) -> PathBuf {
    f.root.parent().unwrap().join("state")
}

fn open(f: &Fixture, path: &str) -> Reader {
    let mut reader = Reader::open(&f.root.join(path)).unwrap();
    reader.state = Some(state(f));
    reader
}

fn editing(f: &Fixture, path: &str) -> Reader {
    let mut reader = open(f, path);
    reader.key(Key::Char('e'));
    assert!(reader.editing(), "{:?}", drawn(&mut reader, 60, 4));
    reader
}

fn typed(reader: &mut Reader, text: &str) {
    for c in text.chars() {
        reader.key(Key::Char(c));
    }
}

fn draft(reader: &Reader) -> String {
    reader.editor.as_ref().unwrap().text()
}

/// The words `e` answers with when the file stays read-only.
fn refusal(reader: &mut Reader) -> String {
    reader.key(Key::Char('e'));
    assert!(!reader.editing());
    drawn(reader, 60, 4)[3].clone()
}

#[test]
fn an_edit_is_a_draft_until_saved_and_the_reader_returns_to_a_fresh_diff() {
    let f = Fixture::new();
    f.write("notes.txt", "one\ntwo\n");
    f.commit("first");
    let file = f.root.join("notes.txt");
    let mut reader = open(&f, "notes.txt");
    assert_eq!(drawn(&mut reader, 44, 6)[5], " ↑↓ scroll · e edit · q close · ? help");

    assert_eq!(reader.key(Key::Char('e')), After::Stay);
    let rows = drawn(&mut reader, 44, 6);
    assert!(rows[0].ends_with("  editing"), "{rows:?}");
    assert_eq!(rows[5], " ^S save · ^Z undo · Esc done · ^G help");

    // Keys that close or explain the reader are text here.
    typed(&mut reader, "zero? q");
    reader.key(Key::Enter);
    let rows = drawn(&mut reader, 44, 6);
    assert!(rows[0].ends_with("  unsaved"), "{rows:?}");
    assert_eq!(&rows[1..5], [" 1   zero? q", " 2   one", " 3   two", " 4"]);
    assert_eq!(fs::read_to_string(&file).unwrap(), "one\ntwo\n");

    assert_eq!(reader.key(Key::Ctrl(b's')), After::Stay);
    assert_eq!(fs::read_to_string(&file).unwrap(), "zero? q\none\ntwo\n");
    let rows = drawn(&mut reader, 44, 6);
    assert!(rows[0].ends_with("  editing"), "{rows:?}");
    assert_eq!(rows[5], " Saved");

    // Esc leaves a saved draft without asking, and the diff is of the saved file.
    assert_eq!(reader.key(Key::Esc), After::Stay);
    assert!(!reader.editing());
    let rows = drawn(&mut reader, 44, 6);
    assert!(rows[0].ends_with("edited  +1"), "{rows:?}");
    assert_eq!(&rows[1..4], [" 1 + zero? q", " 2   one", " 3   two"]);
    assert_eq!(reader.key(Key::Esc), After::Leave);
    assert!(!state(&f).exists());
}

#[test]
fn leaving_unsaved_edits_asks_and_discarding_leaves_the_file_alone() {
    let f = Fixture::folder();
    f.write("notes.txt", "one\n");
    let file = f.root.join("notes.txt");
    let before = fs::metadata(&file).unwrap();
    let mut reader = editing(&f, "notes.txt");
    typed(&mut reader, "draft ");

    // Esc, Ctrl-C and Ctrl-Q all ask, and Esc at the question goes back to the draft.
    for leave in [Key::Esc, Key::Ctrl(b'c'), Key::Ctrl(b'q')] {
        assert_eq!(reader.key(leave), After::Stay);
        assert_eq!(drawn(&mut reader, 60, 4)[3], " Unsaved edits: s save · d discard · Esc back");
        assert_eq!(reader.key(Key::Char('x')), After::Ignored);
        assert_eq!(reader.key(Key::Esc), After::Stay);
        assert!(reader.editing());
        assert_eq!(draft(&reader), "draft one\n");
    }
    // A narrow pane keeps every choice, and a pane with no hint row asks in the heading.
    reader.key(Key::Esc);
    assert_eq!(drawn(&mut reader, 34, 4)[3], " s save · d discard · Esc back");
    assert_eq!(drawn(&mut reader, 28, 4)[3], " s save d discard Esc back");
    assert_eq!(drawn(&mut reader, 60, 2)[0], " Unsaved edits: s save · d discard · Esc back");

    assert_eq!(reader.key(Key::Char('d')), After::Stay);
    assert!(!reader.editing());
    assert_eq!(drawn(&mut reader, 60, 4)[1], " 1   one");
    let after = fs::metadata(&file).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "one\n");
    assert_eq!((before.mtime(), before.mtime_nsec(), before.ino()), (after.mtime(), after.mtime_nsec(), after.ino()));
    // A draft given up is not kept.
    assert!(!state(&f).exists());
}

#[test]
fn the_unsaved_question_can_save_and_close() {
    let f = Fixture::folder();
    f.write("notes.txt", "one\n");
    let mut reader = editing(&f, "notes.txt");
    typed(&mut reader, "draft ");
    reader.key(Key::Esc);
    assert_eq!(reader.key(Key::Char('s')), After::Stay);
    assert!(!reader.editing());
    assert_eq!(fs::read_to_string(f.root.join("notes.txt")).unwrap(), "draft one\n");
    let rows = drawn(&mut reader, 60, 4);
    assert_eq!((rows[1].as_str(), rows[3].as_str()), (" 1   draft one", " Saved · ? help"));
}

#[test]
fn a_file_changed_on_disk_is_not_overwritten_without_asking() {
    let f = Fixture::folder();
    f.write("notes.txt", "one\n");
    let file = f.root.join("notes.txt");
    let mut reader = editing(&f, "notes.txt");
    typed(&mut reader, "mine ");

    // Another program rewrites the file in place, then replaces it with a new file.
    fs::write(&file, "theirs\n").unwrap();
    reader.key(Key::Ctrl(b's'));
    assert_eq!(drawn(&mut reader, 60, 4)[3], " Changed on disk: o overwrite · Esc back");
    assert_eq!(fs::read_to_string(&file).unwrap(), "theirs\n");
    reader.key(Key::Esc);
    assert!(reader.editing() && draft(&reader) == "mine one\n");

    let other = f.root.join("other");
    fs::write(&other, "one\n").unwrap();
    fs::rename(&other, &file).unwrap();
    reader.key(Key::Ctrl(b's'));
    assert_eq!(drawn(&mut reader, 60, 4)[3], " Changed on disk: o overwrite · Esc back");
    assert_eq!(fs::read_to_string(&file).unwrap(), "one\n");

    // Only the explicit answer writes over it.
    reader.key(Key::Char('o'));
    assert_eq!(fs::read_to_string(&file).unwrap(), "mine one\n");
    assert_eq!(drawn(&mut reader, 60, 4)[3], " Saved");
    // The next save starts from what was just written.
    typed(&mut reader, "!");
    reader.key(Key::Ctrl(b's'));
    assert_eq!(fs::read_to_string(&file).unwrap(), "mine !one\n");
}

#[test]
fn saving_and_closing_a_changed_file_asks_first_and_then_closes() {
    let f = Fixture::folder();
    f.write("notes.txt", "one\n");
    let file = f.root.join("notes.txt");
    let mut reader = editing(&f, "notes.txt");
    typed(&mut reader, "mine ");
    fs::write(&file, "theirs\n").unwrap();
    reader.key(Key::Esc);
    reader.key(Key::Char('s'));
    assert!(reader.editing());
    assert_eq!(drawn(&mut reader, 60, 4)[3], " Changed on disk: o overwrite · Esc back");
    reader.key(Key::Char('o'));
    assert!(!reader.editing());
    assert_eq!(fs::read_to_string(&file).unwrap(), "mine one\n");
}

#[test]
fn a_save_keeps_every_byte_it_did_not_edit() {
    let f = Fixture::folder();
    // A byte-order mark, mixed line endings, a tab, a lone carriage return and no final newline.
    let before = "\u{feff}first\r\nsecond\n\tthird \u{e9}\rstill third\r\n\r\nlast";
    let file = f.root.join("mixed.txt");
    fs::write(&file, before).unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o750)).unwrap();
    let linked = f.root.join("linked.txt");
    fs::hard_link(&file, &linked).unwrap();
    let inode = fs::metadata(&file).unwrap().ino();

    let mut reader = editing(&f, "mixed.txt");
    // Saving an untouched draft writes the same bytes.
    reader.key(Key::Ctrl(b's'));
    assert_eq!(fs::read(&file).unwrap(), before.as_bytes());

    // A new line takes the ending most of the file uses.
    reader.key(Key::Down);
    reader.key(Key::End);
    reader.key(Key::Enter);
    typed(&mut reader, "new");
    reader.key(Key::Ctrl(b's'));
    let after = "\u{feff}first\r\nsecond\r\nnew\n\tthird \u{e9}\rstill third\r\n\r\nlast";
    assert_eq!(fs::read(&file).unwrap(), after.as_bytes());

    let metadata = fs::metadata(&file).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o750);
    // The same file was written, so its other name still reads the same text.
    assert_eq!(metadata.ino(), inode);
    assert_eq!(fs::read(&linked).unwrap(), after.as_bytes());

    // Undoing back to the first text and saving restores the first bytes.
    reader.key(Key::Ctrl(b'z'));
    reader.key(Key::Ctrl(b'z'));
    reader.key(Key::Ctrl(b's'));
    assert_eq!(fs::read(&file).unwrap(), before.as_bytes());
}

#[test]
fn a_shorter_draft_does_not_leave_the_old_end_of_the_file() {
    let f = Fixture::folder();
    f.write("notes.txt", "a long first line\nsecond\n");
    let mut reader = editing(&f, "notes.txt");
    for _ in 0..18 {
        reader.key(Key::Delete);
    }
    reader.key(Key::Ctrl(b's'));
    assert_eq!(fs::read_to_string(f.root.join("notes.txt")).unwrap(), "second\n");
}

#[test]
fn commits_links_and_files_that_are_not_text_stay_read_only() {
    let f = Fixture::new();
    f.write("page.txt", "stored\n");
    f.write("binary.dat", "a\0b");
    fs::write(f.root.join("latin.txt"), b"caf\xe9\n").unwrap();
    f.write("big.txt", &"x".repeat(usize::try_from(content::LIMIT).unwrap() + 1));
    symlink("page.txt", f.root.join("link.txt")).unwrap();
    fs::create_dir(f.root.join("real")).unwrap();
    f.write("real/inner.txt", "inner\n");
    symlink("real", f.root.join("alias")).unwrap();
    f.commit("first");

    let mut repo = Repo::open(&f.root).unwrap();
    repo.read();
    let commits = repo.history(1).unwrap();
    let id = CommitId::parse(&commits[0].hash).unwrap();
    let mut committed = Reader::at_commit(&f.root.join("page.txt"), id).unwrap();
    assert_eq!(drawn(&mut committed, 60, 4)[3], " ↑↓ scroll · q close · ? help");
    assert_eq!(refusal(&mut committed), " Read-only: this is a commit · ? help");
    assert!(committed.help().text().contains("Read-only:\n  this is a commit\n"));
    // A reader too short for hints says so in its heading, and a narrow one keeps the reason.
    committed.key(Key::Char('e'));
    assert!(drawn(&mut committed, 60, 2)[0].ends_with("Read-only: this is a commit"));
    assert_eq!(drawn(&mut committed, 20, 4)[3], "this is a commit");

    for (path, why) in [
        ("binary.dat", "not text"),
        ("latin.txt", "not text"),
        ("big.txt", "it cannot be shown"),
        ("link.txt", "this is a link"),
        ("alias/inner.txt", "reached through a link"),
    ] {
        let mut reader = open(&f, path);
        let hint = drawn(&mut reader, 60, 4)[3].clone();
        assert!(!hint.contains("e edit"), "{path}: {hint}");
        assert_eq!(refusal(&mut reader), format!(" Read-only: {why} · ? help"), "{path}");
        reader.help().assert_rows_fit(26);
    }
    assert_eq!(fs::read(f.root.join("latin.txt")).unwrap(), b"caf\xe9\n");

    // A file that was removed still shows its last lines, but there is nothing to edit.
    fs::remove_file(f.root.join("page.txt")).unwrap();
    let mut removed = open(&f, "page.txt");
    assert_eq!(refusal(&mut removed), " Read-only: the file is gone · ? help");
    assert!(!f.root.join("page.txt").exists());
}

#[test]
fn a_file_without_write_permission_says_so_when_asked() {
    let f = Fixture::folder();
    f.write("locked.txt", "kept\n");
    let file = f.root.join("locked.txt");
    fs::set_permissions(&file, Permissions::from_mode(0o444)).unwrap();
    if fs::OpenOptions::new().write(true).open(&file).is_ok() {
        // The administrator can write to anything; there is nothing to check.
        return;
    }
    let mut reader = open(&f, "locked.txt");
    assert_eq!(refusal(&mut reader), " Read-only: no write permission · ? help");
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o444);
}

#[test]
fn a_save_does_not_follow_a_link_put_in_the_way() {
    let f = Fixture::folder();
    let outside = f.root.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("notes.txt"), "outside\n").unwrap();
    fs::create_dir(f.root.join("dir")).unwrap();
    f.write("dir/notes.txt", "inside\n");

    // The file itself becomes a link to a file outside the project.
    let mut reader = editing(&f, "dir/notes.txt");
    typed(&mut reader, "mine ");
    fs::remove_file(f.root.join("dir/notes.txt")).unwrap();
    symlink(outside.join("notes.txt"), f.root.join("dir/notes.txt")).unwrap();
    reader.key(Key::Ctrl(b's'));
    assert_eq!(drawn(&mut reader, 70, 4)[3], " This is no longer a regular file. Not saved.");
    reader.key(Key::Char(' '));

    // Then its folder becomes a link to a folder outside the project.
    fs::remove_dir_all(f.root.join("dir")).unwrap();
    symlink(&outside, f.root.join("dir")).unwrap();
    reader.key(Key::Ctrl(b's'));
    assert_eq!(drawn(&mut reader, 70, 4)[3], " The path to the file changed. Not saved.");
    assert_eq!(fs::read_to_string(outside.join("notes.txt")).unwrap(), "outside\n");

    // The draft is still there, and answering the question cannot force it through either.
    assert!(reader.editing() && draft(&reader).starts_with("mine"));
    reader.key(Key::Esc);
    reader.key(Key::Char('s'));
    assert!(reader.editing());
    assert_eq!(fs::read_to_string(outside.join("notes.txt")).unwrap(), "outside\n");
}

#[test]
fn a_file_removed_while_editing_is_not_made_again() {
    let f = Fixture::folder();
    f.write("notes.txt", "one\n");
    let file = f.root.join("notes.txt");
    let mut reader = editing(&f, "notes.txt");
    typed(&mut reader, "mine ");
    fs::remove_file(&file).unwrap();
    reader.key(Key::Ctrl(b's'));
    assert_eq!(drawn(&mut reader, 60, 4)[3], " The file is gone, so nothing was saved.");
    assert!(!file.exists());
    assert!(reader.editing() && reader.editor.as_ref().unwrap().dirty());
}

#[test]
fn a_reader_closed_from_outside_keeps_its_draft_for_the_next_edit() {
    let f = Fixture::folder();
    f.write("notes.txt", "one\ntwo\n");
    let file = f.root.join("notes.txt");
    let mut reader = editing(&f, "notes.txt");
    typed(&mut reader, "unsaved ");
    // The terminal went away: nobody answered the question.
    drop(reader);
    assert_eq!(fs::read_to_string(&file).unwrap(), "one\ntwo\n");
    let kept: Vec<_> = fs::read_dir(state(&f).join("drafts")).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(fs::read_to_string(&kept[0]).unwrap(), "unsaved one\ntwo\n");

    // Reading the file again shows it as it is; editing offers the draft.
    let mut reader = open(&f, "notes.txt");
    assert_eq!(drawn(&mut reader, 60, 5)[1], " 1   one");
    reader.key(Key::Char('e'));
    assert_eq!(drawn(&mut reader, 60, 5)[4], " Kept draft found: r restore · d delete · Esc later");
    // The narrowest pane milo is laid out for still lists every key, using its margins.
    assert_eq!(drawn(&mut reader, 28, 5)[4], "r restore d delete Esc later");
    assert_eq!(reader.key(Key::Char('x')), After::Ignored);
    reader.key(Key::Esc);
    assert_eq!(draft(&reader), "one\ntwo\n");
    assert_eq!(reader.key(Key::Esc), After::Stay);
    assert!(!reader.editing() && kept[0].exists());

    reader.key(Key::Char('e'));
    reader.key(Key::Char('r'));
    assert_eq!(draft(&reader), "unsaved one\ntwo\n");
    assert_eq!(drawn(&mut reader, 60, 5)[4], " Draft restored · ^Z undoes");
    assert!(!kept[0].exists());
    // The restored text is one change on top of the file.
    reader.key(Key::Ctrl(b'z'));
    assert_eq!(draft(&reader), "one\ntwo\n");
    reader.key(Key::Ctrl(b'y'));
    reader.key(Key::Ctrl(b's'));
    assert_eq!(fs::read_to_string(&file).unwrap(), "unsaved one\ntwo\n");
    drop(reader);
    assert_eq!(fs::read_dir(state(&f).join("drafts")).unwrap().count(), 0);
}

#[test]
fn a_kept_draft_can_be_deleted_instead() {
    let f = Fixture::folder();
    f.write("notes.txt", "one\n");
    let mut reader = editing(&f, "notes.txt");
    typed(&mut reader, "unsaved ");
    drop(reader);
    let mut reader = open(&f, "notes.txt");
    reader.key(Key::Char('e'));
    reader.key(Key::Char('d'));
    assert_eq!(draft(&reader), "one\n");
    assert_eq!(fs::read_dir(state(&f).join("drafts")).unwrap().count(), 0);
    assert_eq!(reader.key(Key::Esc), After::Stay);
    assert!(!reader.editing());
}

#[test]
fn the_cursor_moves_by_what_is_shown_and_edits_where_it_is() {
    let f = Fixture::folder();
    f.write("text.txt", "ab\n\twide 界 end\nxy");
    let mut reader = editing(&f, "text.txt");
    let cursor = |reader: &mut Reader| {
        let mut screen = Screen::sized(40, 6);
        reader.draw(&mut screen);
        let out = String::from_utf8(screen.emitted()).unwrap();
        let reversed: Vec<_> =
            out.split("\x1b[").filter_map(|run| run.split_once('m')).filter(|run| run.0 == "0;7").collect();
        assert_eq!(reversed.len(), 1, "{out:?}");
        reversed[0].1.chars().next().unwrap()
    };
    assert_eq!(cursor(&mut reader), 'a');
    reader.key(Key::End);
    reader.key(Key::Down);
    // Column 2 of the second line is inside its tab, which the cursor cannot split.
    assert_eq!(cursor(&mut reader), ' ');
    typed(&mut reader, "X");
    assert_eq!(draft(&reader), "ab\nX\twide 界 end\nxy");
    reader.key(Key::Ctrl(b'z'));
    for _ in 0..6 {
        reader.key(Key::Right);
    }
    assert_eq!(cursor(&mut reader), '界');
    reader.key(Key::Delete);
    reader.key(Key::Backspace);
    assert_eq!(draft(&reader), "ab\n\twide end\nxy");
    // Up and down keep the column they started in, past a shorter line.
    reader.key(Key::Up);
    reader.key(Key::Down);
    typed(&mut reader, "?");
    assert_eq!(draft(&reader), "ab\n\twide? end\nxy");
    reader.key(Key::Down);
    typed(&mut reader, "!");
    assert_eq!(draft(&reader), "ab\n\twide? end\nxy!");

    // Backspace at the start of a line joins it to the one above; right at the end goes on.
    reader.key(Key::Up);
    reader.key(Key::Home);
    reader.key(Key::Backspace);
    assert_eq!(draft(&reader), "ab\twide? end\nxy!");
    reader.key(Key::End);
    reader.key(Key::Right);
    typed(&mut reader, "_");
    assert_eq!(draft(&reader), "ab\twide? end\n_xy!");

    // A click puts the cursor on the character under it: text starts in column 5.
    reader.key(Key::Click(5 + 4, 1));
    assert_eq!(cursor(&mut reader), 'w');
    reader.key(Key::Click(39, 2));
    typed(&mut reader, ".");
    assert_eq!(draft(&reader), "ab\twide? end\n_xy!.");
}

#[test]
fn new_lines_and_tabs_follow_the_indentation_of_the_file() {
    let f = Fixture::folder();
    f.write("spaces.py", "def f():\n  return 1\n");
    f.write("tabs.go", "func f() {\n\treturn\n}\n");
    f.write("flat.txt", "one\n");

    let mut reader = editing(&f, "spaces.py");
    reader.key(Key::Down);
    reader.key(Key::End);
    reader.key(Key::Enter);
    typed(&mut reader, "x");
    reader.key(Key::Enter);
    reader.key(Key::Ctrl(b'i'));
    typed(&mut reader, "y");
    assert_eq!(draft(&reader), "def f():\n  return 1\n  x\n    y\n");

    let mut reader = editing(&f, "tabs.go");
    reader.key(Key::Ctrl(b'i'));
    assert_eq!(draft(&reader), "\tfunc f() {\n\treturn\n}\n");
    let mut reader = editing(&f, "flat.txt");
    reader.key(Key::Ctrl(b'i'));
    assert_eq!(draft(&reader), "    one\n");
}

#[test]
fn pasted_text_takes_the_line_endings_of_the_file_and_is_one_change() {
    let f = Fixture::folder();
    f.write("dos.txt", "one\r\ntwo\r\n");
    let mut reader = open(&f, "dos.txt");
    assert_eq!(reader.paste(Some("ignored")), After::Ignored);
    reader.key(Key::Char('e'));
    assert_eq!(reader.paste(Some("a\nb\rc\r\n\x1b[31m\x07d\te")), After::Stay);
    assert_eq!(draft(&reader), "a\r\nb\r\nc\r\n[31md\teone\r\ntwo\r\n");
    reader.key(Key::Ctrl(b'z'));
    assert_eq!(draft(&reader), "one\r\ntwo\r\n");

    assert_eq!(reader.paste(None), After::Stay);
    assert_eq!(drawn(&mut reader, 60, 4)[3], " That paste was too long to take.");
    // A question is answered with a key, not with whatever is pasted.
    typed(&mut reader, "x");
    reader.key(Key::Esc);
    assert_eq!(reader.paste(Some("d")), After::Ignored);
    assert!(reader.editing());
}

#[test]
fn the_view_follows_the_cursor_and_the_reader_returns_to_the_same_lines() {
    let f = Fixture::new();
    let text: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    f.write("long.txt", &text);
    f.commit("first");
    let mut reader = open(&f, "long.txt");
    drawn(&mut reader, 30, 7);
    for _ in 0..10 {
        reader.key(Key::Down);
    }
    assert_eq!(drawn(&mut reader, 30, 7)[1], " 11   line 11");

    // Editing starts on the line at the top of the view.
    reader.key(Key::Char('e'));
    assert_eq!(drawn(&mut reader, 30, 7)[1], " 11   line 11");
    reader.key(Key::PgDn);
    reader.key(Key::PgDn);
    let rows = drawn(&mut reader, 30, 7);
    assert_eq!((rows[1].as_str(), rows[5].as_str()), (" 15   line 15", " 19   line 19"));
    // The wheel moves the view alone; typing brings the cursor back.
    reader.key(Key::WheelUp);
    assert_eq!(drawn(&mut reader, 30, 7)[1], " 12   line 12");
    typed(&mut reader, "new ");
    assert_eq!(drawn(&mut reader, 30, 7)[5], " 19   new line 19");

    // A long line pans to keep the cursor in view.
    reader.key(Key::End);
    typed(&mut reader, &" and more".repeat(4));
    let rows = drawn(&mut reader, 30, 7);
    assert!(rows[5].starts_with(" 19  ‹") && rows[5].ends_with("and more"), "{rows:?}");

    reader.key(Key::Ctrl(b's'));
    reader.key(Key::Esc);
    let rows = drawn(&mut reader, 30, 7);
    assert_eq!(rows[1], " 15   line 15");
    assert!(rows[0].ends_with("edited  +1 −1"), "{rows:?}");
    reader.key(Key::Char('n'));
    let rows = drawn(&mut reader, 30, 7);
    assert!(rows.iter().any(|row| row.starts_with(" 19 + new line 19")), "{rows:?}");
}

#[test]
fn code_colours_follow_the_draft() {
    let f = Fixture::folder();
    f.write("page.rs", "fn main() {}\nlet x = 1;\n");
    let mut reader = editing(&f, "page.rs");
    let keyword = |reader: &mut Reader| {
        let mut screen = Screen::sized(40, 5);
        reader.draw(&mut screen);
        let out = String::from_utf8(screen.emitted()).unwrap();
        out.contains("\x1b[0;35mlet")
    };
    assert!(keyword(&mut reader));
    // Opening a block comment on the first line greys the second, and closing it brings the colour back.
    typed(&mut reader, "/*");
    assert!(!keyword(&mut reader));
    reader.key(Key::Ctrl(b'z'));
    assert!(keyword(&mut reader));
}

#[test]
fn help_explains_editing_in_both_modes_and_fits_a_narrow_pane() {
    let f = Fixture::folder();
    f.write("notes.txt", "one\n");
    let mut reader = open(&f, "notes.txt");
    let help = reader.help();
    help.assert_rows_fit(26);
    assert!(help.text().contains("  e      edit this file\n"), "{}", help.text());

    reader.key(Key::Char('e'));
    assert_eq!(reader.key(Key::Ctrl(b'g')), After::Help);
    let help = reader.help();
    help.assert_rows_fit(26);
    for row in ["  ^S         save\n", "  ^Z ^Y      undo, redo\n", "  Esc        stop editing\n"] {
        assert!(help.text().contains(row), "{}", help.text());
    }
}

#[test]
fn a_tiny_pane_edits_without_room_for_text() {
    let f = Fixture::folder();
    f.write("notes.txt", "one\n");
    let mut reader = editing(&f, "notes.txt");
    for (w, h) in [(0, 0), (1, 1), (2, 2), (5, 1), (1, 5), (8, 3)] {
        drawn(&mut reader, w, h);
        typed(&mut reader, "x");
        reader.key(Key::Click(0, 0));
        reader.key(Key::PgDn);
    }
    reader.key(Key::Esc);
    assert_eq!(drawn(&mut reader, 9, 2)[0], "s save d…");
}

#[test]
fn a_draft_cannot_outgrow_what_the_reader_can_show() {
    let f = Fixture::folder();
    let limit = usize::try_from(content::LIMIT).unwrap();
    f.write("big.txt", &"x".repeat(limit - 4));
    let mut reader = editing(&f, "big.txt");
    assert_eq!(reader.paste(Some("12345678")), After::Stay);
    assert_eq!(drawn(&mut reader, 60, 4)[3], " The file would be too large to edit.");
    assert!(!reader.editor.as_ref().unwrap().dirty());
    typed(&mut reader, "ab");
    assert_eq!(draft(&reader).len(), limit - 3);
    reader.key(Key::Ctrl(b's'));
    reader.key(Key::Esc);
    assert!(!reader.editing());
    assert!(drawn(&mut reader, 60, 4)[3].contains("e edit"));
}

/// A reader that finds formatters only in the fixture's own folder of stand-ins.
fn with_tools(f: &Fixture, path: &str) -> (Reader, PathBuf) {
    let bin = f.root.parent().unwrap().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let mut reader = open(f, path);
    reader.tools = Some(bin.clone().into_os_string());
    (reader, bin)
}

/// Asks for formatting and returns the hint row of the frame that announces it and of the one after it.
fn format(reader: &mut Reader) -> (String, String) {
    assert_eq!(reader.key(Key::Ctrl(b'f')), After::Stay);
    let announced = drawn(reader, 60, 5)[4].clone();
    (announced, drawn(reader, 60, 5)[4].clone())
}

#[test]
fn formatting_changes_the_draft_and_undo_takes_it_back_before_anything_is_saved() {
    use crate::format::tests::stand_in;

    let f = Fixture::folder();
    f.write("src/main.rs", "fn main() {}\nlet x = 1;\n");
    let file = f.root.join("src/main.rs");
    let (mut reader, bin) = with_tools(&f, "src/main.rs");
    stand_in(&bin, "rustfmt", "touch \"$0.ran\"; tr a-z A-Z");

    // Nothing runs when the file is opened, shown, edited or explained.
    drawn(&mut reader, 60, 5);
    assert!(reader.help().text().contains("  Formatter: rustfmt\n"));
    reader.key(Key::Char('e'));
    assert_eq!(drawn(&mut reader, 60, 5)[4], " ^S save · ^Z undo · ^F format · Esc done · ^G help");
    assert!(
        reader.help().text().contains("  ^F         format\n")
            && reader.help().text().contains("  Formatter: rustfmt\n")
    );
    reader.help().assert_rows_fit(26);
    reader.key(Key::Down);
    reader.key(Key::End);
    assert!(!bin.join("rustfmt.ran").exists());

    let (announced, done) = format(&mut reader);
    assert_eq!(announced, " Formatting with rustfmt…");
    assert_eq!(done, " Formatted with rustfmt · ^Z undoes");
    assert!(bin.join("rustfmt.ran").exists());
    let rows = drawn(&mut reader, 60, 5);
    assert!(rows[0].ends_with("  unsaved"), "{rows:?}");
    assert_eq!(&rows[1..3], [" 1   FN MAIN() {}", " 2   LET X = 1;"]);
    assert_eq!(fs::read_to_string(&file).unwrap(), "fn main() {}\nlet x = 1;\n");
    // The cursor stays on its line, at the end of it.
    typed(&mut reader, "!");
    assert_eq!(draft(&reader), "FN MAIN() {}\nLET X = 1;!\n");

    reader.key(Key::Ctrl(b'z'));
    reader.key(Key::Ctrl(b'z'));
    assert_eq!(draft(&reader), "fn main() {}\nlet x = 1;\n");
    assert!(drawn(&mut reader, 60, 5)[0].ends_with("  editing"));
    reader.key(Key::Ctrl(b'y'));
    reader.key(Key::Ctrl(b's'));
    assert_eq!(fs::read_to_string(&file).unwrap(), "FN MAIN() {}\nLET X = 1;\n");
}

#[test]
fn formatted_text_can_be_discarded_with_the_draft() {
    use crate::format::tests::stand_in;

    let f = Fixture::folder();
    f.write("a.go", "package main\n");
    let (mut reader, bin) = with_tools(&f, "a.go");
    stand_in(&bin, "gofmt", "tr a-z A-Z");
    reader.key(Key::Char('e'));
    format(&mut reader);
    assert_eq!(draft(&reader), "PACKAGE MAIN\n");
    reader.key(Key::Esc);
    assert_eq!(drawn(&mut reader, 60, 5)[4], " Unsaved edits: s save · d discard · Esc back");
    reader.key(Key::Char('d'));
    assert!(!reader.editing());
    assert_eq!(fs::read_to_string(f.root.join("a.go")).unwrap(), "package main\n");
    assert_eq!(drawn(&mut reader, 60, 5)[1], " 1   package main");
}

#[test]
fn a_formatter_that_fails_leaves_the_draft_and_the_file_as_they_were() {
    use crate::format::tests::stand_in;

    let f = Fixture::folder();
    f.write("a.go", "package main\n");
    let file = f.root.join("a.go");
    let (mut reader, bin) = with_tools(&f, "a.go");
    stand_in(&bin, "gofmt", "head -c 4; echo ' half'; echo '<standard input>:1:9: expected ;' >&2; exit 2");
    reader.key(Key::Char('e'));
    typed(&mut reader, "mine");
    assert_eq!(reader.key(Key::Char('\u{85}')), After::Ignored);
    let (_, done) = format(&mut reader);
    assert_eq!(done, " gofmt: <standard input>:1:9: expected ;");
    assert_eq!(draft(&reader), "minepackage main\n");
    assert_eq!(fs::read_to_string(&file).unwrap(), "package main\n");
    // The draft is still the user's to undo, and nothing of the failed run is in its history.
    reader.key(Key::Ctrl(b'z'));
    assert_eq!(draft(&reader), "package main\n");
    assert!(!reader.editor.as_ref().unwrap().dirty());

    stand_in(&bin, "gofmt", "cat");
    let (_, done) = format(&mut reader);
    assert_eq!(done, " Already formatted (gofmt)");
    assert!(!reader.editor.as_ref().unwrap().dirty());
}

#[test]
fn a_missing_or_unknown_formatter_is_explained_and_nothing_is_started() {
    use crate::format::tests::stand_in;

    let f = Fixture::folder();
    f.write("a.py", "x=1\n");
    f.write("notes.txt", "x\n");
    // A program in the project is not a formatter, even when the pane was started there.
    stand_in(&f.root, "ruff", "touch \"$0.ran\"; cat");

    let (mut reader, _) = with_tools(&f, "a.py");
    let help = reader.help();
    help.assert_rows_fit(26);
    assert!(help.text().contains("  No formatter installed:\n  ruff or black\n"), "{}", help.text());
    reader.key(Key::Char('e'));
    assert!(drawn(&mut reader, 60, 5)[4].contains("^F format"));
    reader.key(Key::Ctrl(b'f'));
    assert_eq!(drawn(&mut reader, 60, 5)[4], " Not installed: ruff or black.");
    assert_eq!(draft(&reader), "x=1\n");
    reader.help().assert_rows_fit(26);
    assert!(reader.help().text().contains("  No formatter installed:\n  ruff or black\n"));
    for path in [".", ""] {
        reader.editor = None;
        reader.tools = Some(path.into());
        reader.key(Key::Char('e'));
        reader.key(Key::Ctrl(b'f'));
        assert_eq!(drawn(&mut reader, 60, 5)[4], " Not installed: ruff or black.", "{path:?}");
    }
    assert!(!f.root.join("ruff.ran").exists());

    let (mut reader, _) = with_tools(&f, "notes.txt");
    assert!(reader.help().text().contains("  No formatter for this\n  kind of file.\n"));
    reader.key(Key::Char('e'));
    assert_eq!(drawn(&mut reader, 60, 5)[4], " ^S save · ^Z undo · Esc done · ^G help");
    reader.key(Key::Ctrl(b'f'));
    assert_eq!(drawn(&mut reader, 60, 5)[4], " No formatter for this kind of file.");
    reader.help().assert_rows_fit(26);
}

#[test]
fn formatting_keeps_the_line_endings_and_byte_order_mark_of_the_file() {
    use crate::format::tests::stand_in;

    let f = Fixture::folder();
    let file = f.root.join("a.go");
    fs::write(&file, "\u{feff}package  main\r\n\r\nvar x\r\n").unwrap();
    let (mut reader, bin) = with_tools(&f, "a.go");
    // The formatter is given the text without the mark, and answers with bare line feeds.
    stand_in(&bin, "gofmt", "tr -d '\\r' | sed 's/  / /'");
    reader.key(Key::Char('e'));
    let (_, done) = format(&mut reader);
    assert_eq!(done, " Formatted with gofmt · ^Z undoes");
    reader.key(Key::Ctrl(b's'));
    assert_eq!(fs::read_to_string(&file).unwrap(), "\u{feff}package main\r\n\r\nvar x\r\n");
}

#[test]
fn a_question_waits_for_its_answer_before_a_formatter_runs() {
    use crate::format::tests::stand_in;

    let f = Fixture::folder();
    f.write("a.go", "package main\n");
    let (mut reader, bin) = with_tools(&f, "a.go");
    stand_in(&bin, "gofmt", "tr a-z A-Z");
    reader.key(Key::Char('e'));
    typed(&mut reader, "x");
    reader.key(Key::Ctrl(b'f'));
    reader.key(Key::Esc);
    for _ in 0..3 {
        assert_eq!(drawn(&mut reader, 60, 5)[4], " Unsaved edits: s save · d discard · Esc back");
    }
    assert_eq!(draft(&reader), "xpackage main\n");
    reader.key(Key::Esc);
    assert_eq!(drawn(&mut reader, 60, 5)[4], " Formatting with gofmt…");
    assert_eq!(drawn(&mut reader, 60, 5)[4], " Formatted with gofmt · ^Z undoes");
}
