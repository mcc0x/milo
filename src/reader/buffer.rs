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

/// A place in the text: a line and a byte within it, always on a character boundary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub(super) struct Pos {
    pub(super) row: usize,
    pub(super) col: usize,
}

/// One replacement of text, kept so that it can be taken back or made again.
struct Change {
    at: Pos,
    removed: String,
    inserted: String,
    /// Where the inserted text ends.
    end: Pos,
    /// Where the cursor was before the change.
    cursor: Pos,
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

/// Where `text` ends when it starts at `at`.
fn end_of(at: Pos, text: &str) -> Pos {
    pieces(text).fold(at, |end, (line, eol)| match eol {
        Eol::None => Pos { row: end.row, col: end.col + line.len() },
        _ => Pos { row: end.row + 1, col: 0 },
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
}

impl Buffer {
    pub(super) fn new(text: &str) -> Self {
        let rows: Vec<Row> = pieces(text).map(|(line, eol)| Row { text: line.to_owned(), eol }).collect();
        let crlf = rows.iter().filter(|row| row.eol == Eol::CrLf).count();
        let lf = rows.iter().filter(|row| row.eol == Eol::Lf).count();
        let newline = if crlf > lf { Eol::CrLf } else { Eol::Lf };
        Self { rows, newline, undo: Vec::new(), redo: Vec::new(), saved: Some(0), typing: false, size: text.len() }
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
        let mut text = String::with_capacity(self.rows.iter().map(|row| row.text.len() + 2).sum());
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

    /// Replaces the text between two places, with no record. Returns what was there and where the new text ends.
    fn splice(&mut self, from: Pos, to: Pos, text: &str) -> (String, Pos) {
        let tail = self.rows[to.row].text.split_off(to.col);
        let tail_eol = self.rows[to.row].eol;
        let mut removed = self.rows[from.row].text.split_off(from.col);
        if from.row != to.row {
            removed.push_str(self.rows[from.row].eol.text());
            // The last of these lines keeps only its start; its ending goes on with the tail.
            let last = to.row - from.row - 1;
            for (i, row) in self.rows.drain(from.row + 1..=to.row).enumerate() {
                removed.push_str(&row.text);
                if i != last {
                    removed.push_str(row.eol.text());
                }
            }
        }
        let mut current = std::mem::take(&mut self.rows[from.row].text);
        let mut added = Vec::new();
        for (line, eol) in pieces(text) {
            current.push_str(line);
            if eol != Eol::None {
                added.push(Row { text: std::mem::take(&mut current), eol });
            }
        }
        let end = Pos { row: from.row + added.len(), col: current.len() };
        current.push_str(&tail);
        added.push(Row { text: current, eol: tail_eol });
        self.rows.splice(from.row..=from.row, added);
        self.size = self.size + text.len() - removed.len();
        (removed, end)
    }

    fn record(&mut self, change: Change) {
        // The saved text was in the changes just dropped from `redo`.
        if self.saved.is_some_and(|depth| depth > self.undo.len()) {
            self.saved = None;
        }
        self.redo.clear();
        self.undo.push(change);
    }

    /// Replaces the text between two places and returns where the new text ends.
    pub(super) fn edit(&mut self, from: Pos, to: Pos, text: &str, cursor: Pos) -> Pos {
        let (removed, end) = self.splice(from, to, text);
        self.record(Change { at: from, removed, inserted: text.to_owned(), end, cursor });
        self.typing = false;
        end
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
        let (_, end) = self.splice(at, at, text);
        let last = self.undo.last_mut().expect("a joined change follows another");
        last.inserted.push(ch);
        last.end = end;
        end
    }

    /// Takes back the last change. Returns where the cursor was and the first line that changed.
    pub(super) fn undo(&mut self) -> Option<(Pos, usize)> {
        let change = self.undo.pop()?;
        self.splice(change.at, change.end, &change.removed);
        let back = (change.cursor, change.at.row);
        self.redo.push(change);
        self.typing = false;
        Some(back)
    }

    /// Makes the last undone change again. Returns where its text ends and the first line that changed.
    pub(super) fn redo(&mut self) -> Option<(Pos, usize)> {
        let change = self.redo.pop()?;
        self.splice(change.at, end_of(change.at, &change.removed), &change.inserted);
        let again = (change.end, change.at.row);
        self.undo.push(change);
        self.typing = false;
        Some(again)
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
        assert_eq!(buffer.redo(), Some((at(1, 1), 0)));
        for expected in &texts[2..] {
            buffer.redo().unwrap();
            assert_eq!(&buffer.text(), expected);
        }
        assert!(buffer.redo().is_none());
        buffer.undo();
        buffer.undo();
        assert_eq!(buffer.undo(), Some((at(2, 0), 0)));
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
