//! Holds the text being edited, line by line, with every change undoable.
//!
//! Each line keeps the ending it was read with, so a file that mixes line
//! endings is written back with the same bytes wherever it was not edited.

/// How a line ends. Only the last line of a file has no ending.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Eol {
    Lf,
    CrLf,
    None,
}

impl Eol {
    pub(super) fn text(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::None => "",
        }
    }
}

struct Row {
    text: String,
    eol: Eol,
}

impl Row {
    fn byte_len(&self) -> usize {
        self.text.len() + self.eol.text().len()
    }

    fn normalize_eol(&mut self) {
        if self.eol == Eol::Lf && self.text.ends_with('\r') {
            self.text.pop();
            self.eol = Eol::CrLf;
        }
    }
}

/// A place in the text: a line and a byte within it, always on a character boundary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub(super) struct Pos {
    pub(super) row: usize,
    pub(super) col: usize,
}

/// One replacement of text, kept so that it can be taken back or made again.
/// The byte offset is relative to the first affected row. It can land inside
/// a CRLF in one version of the text if the CR is a lone character in the other.
struct Change {
    /// A number no other change of this buffer has.
    id: u64,
    from: Pos,
    removed: String,
    inserted: String,
    /// Where the inserted text ends.
    end: Pos,
    /// Where the cursor was before the change.
    cursor: Pos,
}

pub(super) struct Replay {
    pub(super) first_row: usize,
    pub(super) cursor: Pos,
}

/// The lines of `text` with their endings. The last has no ending and may be empty.
fn pieces(text: &str) -> impl Iterator<Item = (&str, Eol)> {
    let mut rest = Some(text);
    std::iter::from_fn(move || {
        let text = rest?;
        let Some(at) = text.find('\n') else {
            rest = None;
            return Some((text, Eol::None));
        };
        rest = Some(&text[at + 1..]);
        Some(match text[..at].strip_suffix('\r') {
            Some(line) => (line, Eol::CrLf),
            None => (&text[..at], Eol::Lf),
        })
    })
}

pub(super) struct Buffer {
    /// Never empty: a file with no text is one line with no ending.
    rows: Vec<Row>,
    /// The ending given to a new line: the one most of the file uses.
    newline: Eol,
    undo: Vec<Change>,
    redo: Vec<Change>,
    /// How many changes the saved text has, unless undoing can no longer reach it.
    saved: Option<usize>,
    /// The last change was a typed character, which the next one may join.
    typing: bool,
    /// The length of the whole text in bytes.
    size: usize,
    /// How many changes have been recorded.
    changes: u64,
}

impl Buffer {
    pub(super) fn new(text: &str) -> Self {
        let rows: Vec<Row> = pieces(text).map(|(line, eol)| Row { text: line.to_owned(), eol }).collect();
        let crlf = rows.iter().filter(|row| row.eol == Eol::CrLf).count();
        let lf = rows.iter().filter(|row| row.eol == Eol::Lf).count();
        let newline = if crlf > lf { Eol::CrLf } else { Eol::Lf };
        let size = text.len();
        Self { rows, newline, undo: Vec::new(), redo: Vec::new(), saved: Some(0), typing: false, size, changes: 0 }
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    /// How many bytes the whole text takes.
    pub(super) fn size(&self) -> usize {
        self.size
    }

    pub(super) fn row(&self, row: usize) -> &str {
        &self.rows[row].text
    }

    pub(super) fn end(&self) -> Pos {
        let row = self.rows.len() - 1;
        Pos { row, col: self.rows[row].text.len() }
    }

    /// The ending a new line gets.
    pub(super) fn newline(&self) -> &'static str {
        self.newline.text()
    }

    /// The whole text, with every line's own ending.
    pub(super) fn text(&self) -> String {
        let mut text = String::with_capacity(self.size);
        for row in &self.rows {
            text.push_str(&row.text);
            text.push_str(row.eol.text());
        }
        text
    }

    pub(super) fn dirty(&self) -> bool {
        self.saved != Some(self.undo.len())
    }

    pub(super) fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
        self.typing = false;
    }

    /// Replaces bytes between row-relative positions, including positions inside CRLF.
    fn splice(&mut self, from: Pos, to: Pos, text: &str) -> Pos {
        if from.row == to.row && to.col <= self.rows[from.row].text.len() && !text.contains('\n') {
            let row = &mut self.rows[from.row];
            row.text.replace_range(from.col..to.col, text);
            row.normalize_eol();
            return Pos { row: from.row, col: (from.col + text.len()).min(row.text.len()) };
        }

        let last = &mut self.rows[to.row];
        let tail_eol = if to.col > last.text.len() { Eol::Lf } else { last.eol };
        let tail = if to.col < last.text.len() { last.text.split_off(to.col) } else { String::new() };
        let first = &mut self.rows[from.row];
        // The only byte before a position inside an ending is the CR of a CRLF.
        let mut current = std::mem::take(&mut first.text);
        if from.col > current.len() {
            current.push('\r');
        } else {
            current.truncate(from.col);
        }
        let mut added = Vec::new();
        for (line, eol) in pieces(text) {
            current.push_str(line);
            if eol != Eol::None {
                let mut row = Row { text: std::mem::take(&mut current), eol };
                row.normalize_eol();
                added.push(row);
            }
        }
        let col = current.len();
        current.push_str(&tail);
        let mut last = Row { text: current, eol: tail_eol };
        last.normalize_eol();
        let end = Pos { row: from.row + added.len(), col: col.min(last.text.len()) };
        added.push(last);
        self.rows.splice(from.row..=to.row, added);
        end
    }

    /// Copies the replaced text into history, including the original line endings.
    fn removed(&self, from: Pos, to: Pos) -> String {
        if from.row == to.row {
            return self.row(from.row)[from.col..to.col].to_owned();
        }
        let first = &self.rows[from.row];
        let mut removed = first.text[from.col..].to_owned();
        removed.push_str(first.eol.text());
        for row in &self.rows[from.row + 1..to.row] {
            removed.push_str(&row.text);
            removed.push_str(row.eol.text());
        }
        removed.push_str(&self.row(to.row)[..to.col]);
        removed
    }

    fn record(&mut self, change: Change) {
        // The saved text was in the changes just dropped from `redo`.
        if self.saved.is_some_and(|depth| depth > self.undo.len()) {
            self.saved = None;
        }
        self.redo.clear();
        self.undo.push(change);
    }

    /// Replays a replacement using only rows spanned by the edited byte range.
    fn rewrite(&mut self, from: Pos, len: usize, text: &str) {
        let mut to = Pos { col: from.col + len, ..from };
        while to.row + 1 < self.rows.len() {
            let bytes = self.rows[to.row].byte_len();
            if to.col < bytes {
                break;
            }
            to.col -= bytes;
            to.row += 1;
        }
        self.splice(from, to, text);
        self.size = self.size + text.len() - len;
    }

    /// Replaces the text between two places and returns where the new text ends.
    pub(super) fn edit(&mut self, from: Pos, to: Pos, text: &str, cursor: Pos) -> Pos {
        let removed = self.removed(from, to);
        let end = self.splice(from, to, text);
        self.size = self.size + text.len() - removed.len();
        self.changes += 1;
        self.record(Change { id: self.changes, from, removed, inserted: text.to_owned(), end, cursor });
        self.typing = false;
        end
    }

    /// The number of the change made last.
    pub(super) fn last_change(&self) -> u64 {
        self.changes
    }

    /// Whether a change is part of the text now: made, and not undone.
    pub(super) fn holds(&self, change: u64) -> bool {
        self.undo.iter().any(|made| made.id == change)
    }

    /// Inserts one typed character. A run of them, up to the end of a word, is undone together.
    pub(super) fn type_char(&mut self, at: Pos, ch: char) -> Pos {
        let mut bytes = [0; 4];
        let text = ch.encode_utf8(&mut bytes);
        let joins = self.typing
            && self.dirty()
            && self.undo.last().is_some_and(|last| {
                last.end == at && (!ch.is_whitespace() || last.inserted.ends_with(char::is_whitespace))
            });
        if !joins {
            let end = self.edit(at, at, text, at);
            self.typing = true;
            return end;
        }
        let end = self.splice(at, at, text);
        self.size += text.len();
        let last = self.undo.last_mut().expect("a joined change follows another");
        last.inserted.push(ch);
        last.end = end;
        end
    }

    /// Takes back the last change and returns its first row and previous cursor.
    pub(super) fn undo(&mut self) -> Option<Replay> {
        let change = self.undo.pop()?;
        self.rewrite(change.from, change.inserted.len(), &change.removed);
        let replay = Replay { first_row: change.from.row, cursor: change.cursor };
        self.redo.push(change);
        self.typing = false;
        Some(replay)
    }

    /// Makes the last undone change again and returns its first row and resulting cursor.
    pub(super) fn redo(&mut self) -> Option<Replay> {
        let change = self.redo.pop()?;
        self.rewrite(change.from, change.removed.len(), &change.inserted);
        let replay = Replay { first_row: change.from.row, cursor: change.end };
        self.undo.push(change);
        self.typing = false;
        Some(replay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(row: usize, col: usize) -> Pos {
        Pos { row, col }
    }

    #[test]
    fn text_is_returned_byte_for_byte_with_mixed_endings_and_no_final_newline() {
        for text in ["", "\n", "a", "a\n", "a\r\nb\nc\r\n\r\n", "one\rtwo\n\u{feff}é\r\n\tend", "\r\n\r\n"] {
            assert_eq!(Buffer::new(text).text(), text, "{text:?}");
        }
        assert_eq!(Buffer::new("a\nb\n").len(), 3);
        assert_eq!(Buffer::new("a\r\nb").newline(), "\r\n");
        assert_eq!(Buffer::new("a\r\nb\nc\n").newline(), "\n");
        assert_eq!(Buffer::new("").newline(), "\n");
    }

    #[test]
    fn an_edit_touches_only_the_lines_it_names() {
        let mut buffer = Buffer::new("a\r\nbc\nd");
        assert_eq!(buffer.edit(at(1, 1), at(1, 1), "X\r\nY", at(1, 1)), at(2, 1));
        assert_eq!(buffer.text(), "a\r\nbX\r\nYc\nd");
        // Joining two lines removes the first one's ending and keeps the second's.
        assert_eq!(buffer.edit(at(0, 1), at(1, 0), "", at(1, 0)), at(0, 1));
        assert_eq!(buffer.text(), "abX\r\nYc\nd");
        assert_eq!(buffer.edit(at(0, 1), at(2, 1), "-", at(0, 1)), at(0, 2));
        assert_eq!(buffer.text(), "a-");
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn undo_and_redo_restore_every_byte_and_the_cursor() {
        let original = "a\r\nbc\nd\n";
        let mut buffer = Buffer::new(original);
        let mut texts = vec![buffer.text()];
        buffer.edit(at(0, 1), at(2, 0), "1\n2", at(2, 0));
        texts.push(buffer.text());
        buffer.edit(at(0, 0), buffer.end(), "", at(0, 0));
        texts.push(buffer.text());
        buffer.edit(at(0, 0), at(0, 0), "new\r\ntext", at(0, 0));
        texts.push(buffer.text());
        assert_eq!(texts[1..], ["a1\n2d\n", "", "new\r\ntext"]);

        for expected in texts.iter().rev().skip(1) {
            buffer.undo().unwrap();
            assert_eq!(&buffer.text(), expected);
        }
        assert!(buffer.undo().is_none() && !buffer.dirty());
        assert_eq!(buffer.redo().unwrap().cursor, at(1, 1));
        for expected in &texts[2..] {
            buffer.redo().unwrap();
            assert_eq!(&buffer.text(), expected);
        }
        assert!(buffer.redo().is_none());
        buffer.undo();
        buffer.undo();
        assert_eq!(buffer.undo().unwrap().cursor, at(2, 0));
    }

    #[test]
    fn typed_words_are_undone_one_at_a_time() {
        let mut buffer = Buffer::new("");
        let mut cursor = at(0, 0);
        for ch in "one two".chars() {
            cursor = buffer.type_char(cursor, ch);
        }
        assert_eq!(buffer.text(), "one two");
        buffer.undo();
        assert_eq!(buffer.text(), "one");
        buffer.undo();
        assert_eq!(buffer.text(), "");
        // Typing somewhere else starts a change of its own.
        let end = buffer.type_char(at(0, 0), 'a');
        buffer.type_char(at(0, 0), 'b');
        assert_eq!((buffer.text().as_str(), end), ("ba", at(0, 1)));
        buffer.undo();
        assert_eq!(buffer.text(), "a");
    }

    #[test]
    fn a_lone_carriage_return_survives_changes_around_it_and_their_undoing() {
        // Removing what follows the carriage return puts it straight before a line feed.
        let mut buffer = Buffer::new("a\rb\nc");
        let end = buffer.edit(at(0, 2), at(0, 3), "", at(0, 3));
        assert_eq!(buffer.text(), "a\r\nc");
        assert_eq!(buffer.row(0), "a");
        // The whole text is then replaced, as a formatter does, and both changes are taken back.
        buffer.edit(at(0, 0), buffer.end(), "X\n", end);
        let back = buffer.undo().unwrap().cursor;
        assert_eq!(buffer.text(), "a\r\nc");
        assert!(buffer.row(back.row).is_char_boundary(back.col) && back.col <= buffer.row(back.row).len());
        buffer.undo();
        assert_eq!(buffer.text(), "a\rb\nc");
        buffer.redo();
        buffer.redo();
        assert_eq!(buffer.text(), "X\n");

        let mut buffer = Buffer::new("a\r");
        let end = buffer.edit(at(0, 2), at(0, 2), "\n", at(0, 2));
        buffer.edit(at(0, 0), buffer.end(), "Y", end);
        buffer.undo();
        buffer.undo();
        assert_eq!(buffer.text(), "a\r");
        assert_eq!(buffer.end(), at(0, 2));
    }

    #[test]
    fn history_matches_a_byte_splice_at_mixed_endings() {
        let original = "head\né\rb\n\r\nc\r\r\n界\r";
        let positions: Vec<_> = pieces(original)
            .enumerate()
            .scan(0, |offset, (row, (text, eol))| {
                let start = *offset;
                *offset += text.len() + eol.text().len();
                Some(
                    text.char_indices()
                        .map(|(col, _)| col)
                        .chain(std::iter::once(text.len()))
                        .map(move |col| (at(row, col), start + col)),
                )
            })
            .flatten()
            .collect();
        for (i, &(from, start)) in positions.iter().enumerate() {
            for &(to, end) in &positions[i..] {
                for inserted in ["", "\r", "\n", "\r\n", "\r\nλ\r", "λ\r\nx"] {
                    let mut buffer = Buffer::new(original);
                    let mut expected = original.to_owned();
                    expected.replace_range(start..end, inserted);
                    let cursor = buffer.edit(from, to, inserted, to);
                    assert!(buffer.row(cursor.row).is_char_boundary(cursor.col));
                    assert_eq!(buffer.text(), expected);
                    assert_eq!(buffer.size(), expected.len());
                    let undo = buffer.undo().unwrap();
                    assert_eq!((undo.first_row, undo.cursor), (from.row, to));
                    assert_eq!(buffer.text(), original);
                    assert_eq!(buffer.size(), original.len());
                    let redo = buffer.redo().unwrap();
                    assert_eq!((redo.first_row, redo.cursor), (from.row, cursor));
                    assert_eq!(buffer.text(), expected);
                    for (row, (text, eol)) in buffer.rows.iter().zip(pieces(&expected)) {
                        assert_eq!((row.text.as_str(), row.eol), (text, eol));
                    }
                    assert_eq!(buffer.len(), pieces(&expected).count());
                }
            }
        }
    }

    #[test]
    fn history_keeps_rows_outside_the_replacement_in_place() {
        let mut buffer = Buffer::new("head\nx\rX\nz\nfoot");
        let head = buffer.row(0).as_ptr();
        let foot = buffer.row(3).as_ptr();
        buffer.edit(at(1, 2), at(2, 1), "\n", at(2, 1));
        for _ in 0..2 {
            buffer.undo();
            assert_eq!((buffer.row(0).as_ptr(), buffer.row(3).as_ptr()), (head, foot));
            buffer.redo();
            assert_eq!((buffer.row(0).as_ptr(), buffer.row(3).as_ptr()), (head, foot));
        }
    }

    #[test]
    fn the_saved_text_is_clean_again_only_when_undo_or_redo_returns_to_it() {
        let mut buffer = Buffer::new("a");
        assert!(!buffer.dirty());
        let end = buffer.type_char(at(0, 1), 'b');
        buffer.mark_saved();
        // A character typed after a save is not folded into the saved change.
        buffer.type_char(end, 'c');
        assert!(buffer.dirty());
        buffer.undo();
        assert_eq!((buffer.text().as_str(), buffer.dirty()), ("ab", false));
        buffer.undo();
        assert!(buffer.dirty());
        buffer.redo();
        assert!(!buffer.dirty());
        // A different change at the same depth is not the saved text.
        buffer.undo();
        buffer.type_char(at(0, 1), 'x');
        assert_eq!((buffer.text().as_str(), buffer.dirty()), ("ax", true));
        buffer.undo();
        assert!(buffer.dirty());
    }
}
