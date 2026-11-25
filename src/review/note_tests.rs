//! Notes written during a review: what they hold, what is copied, and what stays put.

use super::tests::{drawn, mixed_project};
use super::*;

fn keys(review: &mut Review, keys: &[Key]) {
    for key in keys {
        review.key(*key);
    }
}

fn typed(review: &mut Review, text: &str) {
    for ch in text.chars() {
        assert_eq!(review.key(Key::Char(ch)), After::Stay, "{ch:?} was not taken as text");
    }
}

/// Writes a note on the line the cursor starts on and keeps it for review.
fn note(review: &mut Review, text: &str) {
    drawn(review, 70, 16);
    keys(review, &[Key::Char('i'), Key::Enter]);
    typed(review, text);
    review.key(Key::Enter);
}

/// The text the next frame asks the terminal to copy, if it asks.
fn copied(review: &mut Review) -> Option<String> {
    let mut screen = Screen::sized(70, 16);
    review.draw(&mut screen);
    let sent = screen.emitted();
    let start = sent.windows(7).position(|bytes| bytes == b"\x1b]52;c;")? + 7;
    let end = start + sent[start..].windows(2).position(|bytes| bytes == b"\x1b\\")?;
    let mut bits =
        sent[start..end].iter().filter(|byte| **byte != b'=').fold((0u32, 0, Vec::new()), |mut acc, byte| {
            let six = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                _ => 63,
            };
            acc.0 = acc.0 << 6 | u32::from(six);
            acc.1 += 6;
            if acc.1 >= 8 {
                acc.1 -= 8;
                acc.2.push((acc.0 >> acc.1).to_le_bytes()[0]);
            }
            acc
        });
    Some(String::from_utf8(std::mem::take(&mut bits.2)).unwrap())
}

#[test]
fn a_note_says_whether_its_line_numbers_are_the_files_or_the_removed_lines_old_ones() {
    let (f, patch) = mixed_project();
    let mut review = Review::open(&f.root, &patch, Some("the agent")).unwrap();
    drawn(&mut review, 70, 16);
    // The cursor starts on the change in view: the line the patch removed.
    review.key(Key::Char('i'));
    let rows = drawn(&mut review, 70, 16);
    assert_eq!(rows[4], "›   - line 4");
    assert_eq!(rows[15], " ↑↓ line · v range · n N changes · Esc back · Enter write");
    review.key(Key::Enter);
    let rows = drawn(&mut review, 70, 16);
    assert_eq!(rows[13], " Note on app.txt, removed line 4 (old numbering)");
    // A narrow pane gives up the file's name before it gives up which numbering this is.
    assert_eq!(drawn(&mut review, 48, 16)[13], " Note on app.t…, removed line 4 (old numbering)");
    assert_eq!(rows[15], " Enter keep for review · ^N new line · ^Y copy now · Esc cancel");
    typed(&mut review, "Why remove this? q");
    review.key(Key::Enter);
    assert_eq!(drawn(&mut review, 70, 16)[15], " Kept for review · ? help");

    // A range over the removed line and the one that replaced it names both numberings.
    keys(&mut review, &[Key::Char('i'), Key::Char('v'), Key::Down, Key::Enter]);
    assert_eq!(drawn(&mut review, 70, 16)[13], " Note on app.txt, line 4, removed line 4 (old numbering)");
    typed(&mut review, "Keep the comma.");
    review.key(Key::Ctrl(b'y'));
    let one = "Review note\n\napp.txt, line 4, removed line 4 (old numbering):\n\
               > 4 - line 4\n> 4 + line 4, by the agent\nKeep the comma.\n";
    assert_eq!(copied(&mut review).as_deref(), Some(one));
    // Copying that one note sent nothing else and left the first note waiting.
    assert_eq!(copied(&mut review), None);
    assert_eq!(review.waiting(), 1);
    review.key(Key::Char('p'));
    let rows = drawn(&mut review, 70, 16);
    assert_eq!(rows[0], " Notes · 1 to copy · 1 copied");
    assert!(rows[2].starts_with("  1. app.txt, removed line 4 (old numbering) · Why") && rows[2].ends_with("to copy"));
    assert!(
        rows[3].starts_with("  2. app.txt, line 4, removed line 4 (old numbering)") && rows[3].ends_with(" copied")
    );
}

#[test]
fn waiting_notes_are_copied_as_one_message_and_only_when_asked() {
    let (f, patch) = mixed_project();
    let before = std::fs::read(f.root.join("app.txt")).unwrap();
    let mut review = Review::open(&f.root, &patch, None).unwrap();
    note(&mut review, "First.");
    review.key(Key::Char(']'));
    // What is pasted into a note cannot carry a terminal control into what is copied.
    drawn(&mut review, 70, 16);
    keys(&mut review, &[Key::Char('i'), Key::Enter]);
    typed(&mut review, "Second,");
    review.paste(Some("\u{1b} on a new file."));
    review.key(Key::Enter);
    assert_eq!(copied(&mut review), None);
    review.key(Key::Char('r'));
    assert!(drawn(&mut review, 90, 6)[5].ends_with("i note · p 2 notes · ? help"));

    review.key(Key::Char('p'));
    let rows = drawn(&mut review, 70, 16);
    assert_eq!(rows[1], "› All 2 waiting notes, as one message");
    assert_eq!(&rows[4..7], [" Copied as", " Review notes (2)", ""]);
    assert_eq!(rows[15], " y copy all · ↑↓ choose · Esc back · ? help");
    review.key(Key::Char('y'));
    let batch = "Review notes (2)\n\n1. app.txt, removed line 4 (old numbering):\n> 4 - line 4\nFirst.\n\n\
                 2. new.txt, line 1:\n> 1 + made by the agent\nSecond, on a new file.\n";
    assert_eq!(copied(&mut review).as_deref(), Some(batch));
    let rows = drawn(&mut review, 70, 16);
    assert_eq!(rows[0], " Notes · 0 to copy · 2 copied");
    assert_eq!(rows[15], " Copied the waiting notes as one message · ? help");
    // The copied notes can still be listed, and nothing waits, so the review closes without a question.
    assert_eq!(review.key(Key::Esc), After::Stay);
    assert!(drawn(&mut review, 90, 6)[5].ends_with("i note · p notes · ? help"));
    assert_eq!(review.key(Key::Char('q')), After::Leave);
    assert_eq!(std::fs::read(f.root.join("app.txt")).unwrap(), before);
}

#[test]
fn writing_or_cancelling_a_note_leaves_the_file_where_it_was_being_read() {
    let (f, patch) = mixed_project();
    let mut review = Review::open(&f.root, &patch, None).unwrap();
    drawn(&mut review, 70, 8);
    keys(&mut review, &[Key::Down, Key::Down]);
    let reading = drawn(&mut review, 70, 8);
    assert!(reading[1].ends_with("line 3") && reading[6].ends_with("line 7"), "{reading:?}");
    let body = |rows: &[String]| {
        rows[1..7].iter().map(|row| row[row.char_indices().nth(1).unwrap().0..].to_owned()).collect::<Vec<_>>()
    };

    // The cursor goes below what the panel leaves in view, and the note is thrown away.
    keys(&mut review, &[Key::Char('i'), Key::Down, Key::Down, Key::Down, Key::Enter]);
    typed(&mut review, "never mind");
    let writing = drawn(&mut review, 70, 8);
    assert!(writing[1..5].iter().any(|row| row.starts_with('›')), "{writing:?}");
    assert_eq!(writing[5], " Note on app.txt, line 6");
    assert_eq!(review.key(Key::Esc), After::Stay);
    assert_eq!(drawn(&mut review, 70, 8)[7], " Discard this note? y discard · n keep writing");
    review.key(Key::Char('y'));
    assert_eq!(drawn(&mut review, 70, 8), reading);
    assert!(review.notes.is_empty());

    // Kept, the same: only the mark of the note and the hint differ.
    keys(&mut review, &[Key::Char('i'), Key::Down, Key::Down, Key::Down, Key::Enter]);
    typed(&mut review, "kept");
    review.key(Key::Enter);
    let kept = drawn(&mut review, 70, 8);
    assert_eq!(body(&kept), body(&reading));
    assert_eq!(kept[5], "• 6   line 6");
}

#[test]
fn a_note_keeps_the_lines_it_was_written_about_when_the_file_moves_on() {
    let (f, patch) = mixed_project();
    let mut review = Review::open(&f.root, &patch, None).unwrap();
    drawn(&mut review, 70, 16);
    keys(&mut review, &[Key::Char('i'), Key::Down, Key::Enter]);
    typed(&mut review, "Say why.");
    review.key(Key::Enter);
    assert_eq!(drawn(&mut review, 70, 16)[5], "• 4 + line 4, by the agent");

    // A person adds a line above. The patch still fits, one line further down.
    let now = std::fs::read_to_string(f.root.join("app.txt")).unwrap();
    f.write("app.txt", &format!("a new first line\n{now}"));
    review.key(Key::Char('r'));
    let rows = drawn(&mut review, 70, 16);
    assert!(rows.iter().any(|row| row == "  5 + line 4, by the agent"), "{rows:?}");
    assert!(!rows.iter().any(|row| row.starts_with('•')), "{rows:?}");
    review.key(Key::Char('p'));
    let said = "Review note\n\napp.txt, line 4:\n> 4 + line 4, by the agent\nSay why.\n";
    review.key(Key::Char('y'));
    assert_eq!(copied(&mut review).as_deref(), Some(said));
}

#[test]
fn closing_asks_first_while_a_note_was_never_copied() {
    let (f, patch) = mixed_project();
    let mut review = Review::open(&f.root, &patch, None).unwrap();
    assert!(!review.holds_drafts());
    note(&mut review, "Unsent.");
    assert!(review.holds_drafts());
    for close in [Key::Char('q'), Key::Esc, Key::Ctrl(b'c')] {
        assert_eq!(review.key(close), After::Stay);
        assert_eq!(drawn(&mut review, 70, 16)[15], " 1 note was not copied. y close anyway · p see them · n stay");
        // Losing focus is not an answer.
        assert_eq!(review.key(Key::Focus(false)), After::Ignored);
        assert_eq!(review.key(Key::Char('n')), After::Stay);
        assert!(!drawn(&mut review, 70, 16)[15].contains("not copied"));
    }
    review.key(Key::Char('q'));
    assert_eq!(review.key(Key::Char('p')), After::Stay);
    assert_eq!(drawn(&mut review, 70, 16)[0], " Notes · 1 to copy · 0 copied");
    keys(&mut review, &[Key::Esc, Key::Char('q')]);
    assert_eq!(review.key(Key::Char('y')), After::Leave);
}

#[test]
fn a_listed_note_can_be_edited_and_removed_only_after_a_yes() {
    let (f, patch) = mixed_project();
    let mut review = Review::open(&f.root, &patch, None).unwrap();
    note(&mut review, "Frist");
    note(&mut review, "Other.");
    keys(&mut review, &[Key::Char('p'), Key::Down, Key::Char('y')]);
    assert!(copied(&mut review).is_some_and(|text| text.ends_with("Frist\n")));
    assert!(review.notes[0].copied);

    // An edit is pasted text and keys; what changed has to be copied again.
    review.key(Key::Char('e'));
    assert!(review.editing());
    keys(&mut review, &[Key::Ctrl(b'u')]);
    assert_eq!(review.paste(Some("First,\nrewritten?")), After::Stay);
    assert_eq!(drawn(&mut review, 70, 16)[12], " Note on app.txt, removed line 4 (old numbering)");
    review.key(Key::Enter);
    assert!(!review.editing() && !review.notes[0].copied);
    assert_eq!(review.notes[0].text, "First,\nrewritten?");
    let rows = drawn(&mut review, 70, 16);
    assert_eq!(rows[15], " Kept for review · ? help");
    assert!(rows[2].starts_with("› 1. app.txt") && rows[2].contains("· First,"), "{rows:?}");

    review.key(Key::Char('d'));
    assert_eq!(drawn(&mut review, 70, 16)[15], " Remove this note? y remove · n keep");
    review.key(Key::Char('n'));
    assert_eq!(review.notes.len(), 2);
    keys(&mut review, &[Key::Char('d'), Key::Char('y')]);
    assert_eq!(review.notes.len(), 1);
    assert_eq!(review.notes[0].text, "Other.");
    keys(&mut review, &[Key::Char('d'), Key::Char('y')]);
    assert_eq!(drawn(&mut review, 70, 16)[15], " No notes left · ? help");
    assert_eq!(review.key(Key::Char('q')), After::Leave);
}

#[test]
fn a_file_with_no_diff_takes_no_note_and_the_smallest_panes_still_draw() {
    let (f, patch) = mixed_project();
    let now = std::fs::read_to_string(f.root.join("app.txt")).unwrap();
    f.write("app.txt", &now.replace("line 4, by the agent", "line 4, rewritten"));
    let mut review = Review::open(&f.root, &patch, None).unwrap();
    drawn(&mut review, 70, 8);
    review.key(Key::Char('i'));
    assert_eq!(drawn(&mut review, 70, 8)[7], " No lines to note here · ? help");
    assert_eq!(review.key(Key::Char('p')), After::Stay);
    assert_eq!(drawn(&mut review, 70, 8)[7], " No notes yet · ? help");

    review.key(Key::Char(']'));
    for (w, h) in [(1, 1), (2, 2), (6, 3), (9, 4), (30, 5)] {
        drawn(&mut review, w, h);
        keys(&mut review, &[Key::Char('i'), Key::Char('v'), Key::Down]);
        drawn(&mut review, w, h);
        review.key(Key::Enter);
        typed(&mut review, "a note that wraps over the rows there are");
        drawn(&mut review, w, h);
        keys(&mut review, &[Key::Enter, Key::Char('p')]);
        drawn(&mut review, w, h);
        keys(&mut review, &[Key::Down, Key::Char('e')]);
        drawn(&mut review, w, h);
        keys(&mut review, &[Key::Esc, Key::Esc, Key::Char('q')]);
        drawn(&mut review, w, h);
        review.key(Key::Char('n'));
    }
    assert_eq!(review.notes.len(), 5);
}

#[test]
fn every_help_page_of_the_review_fits_a_column() {
    let (f, patch) = mixed_project();
    let mut review = Review::open(&f.root, &patch, None).unwrap();
    drawn(&mut review, 70, 16);
    review.help().assert_rows_fit(26);
    assert!(review.help().text().contains("never sent for you"));
    review.key(Key::Char('i'));
    review.help().assert_rows_fit(26);
    review.key(Key::Enter);
    review.help().assert_rows_fit(26);
    typed(&mut review, "x");
    keys(&mut review, &[Key::Enter, Key::Char('p')]);
    review.help().assert_rows_fit(26);
    assert!(review.help().text().contains("Milo sends nothing"));
}
