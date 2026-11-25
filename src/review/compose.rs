//! The few lines of text a note is written in, with a cursor.

use std::ops::Range;

use super::notes::NOTE_LIMIT;
use crate::keys::Key;
use crate::screen::{Screen, Style};
use crate::width::{byte_at_col, char_width, str_width};

/// The words of a note while it is being written.
pub struct Draft {
    text: String,
    /// A byte of `text` that starts a character, or its length.
    cursor: usize,
}

/// The rows `text` takes at `width` columns, as byte ranges without their
/// line breaks. A row that would overflow breaks after its last space, or
/// mid-word where it has none. The last row is where a cursor at the end goes.
pub(super) fn rows(text: &str, width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let (mut start, mut used, mut space) = (0, 0, None);
    for (at, ch) in text.char_indices() {
        if ch == '\n' {
            rows.push(start..at);
            (start, used, space) = (at + 1, 0, None);
            continue;
        }
        let wide = char_width(ch);
        if used + wide > width {
            let cut = space.filter(|_| ch != ' ').unwrap_or(at);
            rows.push(start..cut);
            (start, used, space) = (cut, str_width(&text[cut..at]), None);
        }
        used += wide;
        if ch == ' ' {
            space = Some(at + 1);
        }
    }
    rows.push(start..text.len());
    rows
}

impl Draft {
    pub fn new(text: &str) -> Self {
        Self { text: text.to_owned(), cursor: text.len() }
    }

    /// The note's words, without space around them.
    pub fn text(&self) -> &str {
        self.text.trim()
    }

    /// Takes in pasted text: line breaks stay, and nothing else that is not printable does.
    pub fn paste(&mut self, pasted: &str) {
        let room = NOTE_LIMIT.saturating_sub(self.text.chars().count());
        let clean: String = pasted
            .replace("\r\n", "\n")
            .chars()
            .map(|c| if matches!(c, '\r' | '\t') { ' ' } else { c })
            .filter(|c| *c == '\n' || !c.is_control())
            .take(room)
            .collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    fn before(&self) -> Option<usize> {
        self.text[..self.cursor].char_indices().next_back().map(|(at, _)| at)
    }

    fn after(&self) -> Option<usize> {
        self.text[self.cursor..].chars().next().map(|ch| self.cursor + ch.len_utf8())
    }

    /// The start of the word before the cursor, for deleting back to.
    fn word_before(&self) -> usize {
        let before = self.text[..self.cursor].trim_end();
        before.rfind(char::is_whitespace).map_or(0, |at| at + 1)
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |at| at + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..].find('\n').map_or(self.text.len(), |at| self.cursor + at)
    }

    /// Moves the cursor a row up or down at `width` columns, keeping its column where the row is long enough.
    fn move_rows(&mut self, down: bool, width: usize) {
        let rows = rows(&self.text, width);
        let row = self.row(&rows);
        let Some(to) = (if down { rows.get(row + 1) } else { row.checked_sub(1).map(|row| &rows[row]) }) else {
            return;
        };
        let col = str_width(&self.text[rows[row].start..self.cursor]);
        self.cursor = to.start + byte_at_col(&self.text[to.clone()], col);
    }

    fn row(&self, rows: &[Range<usize>]) -> usize {
        rows.partition_point(|row| row.start <= self.cursor).saturating_sub(1)
    }

    /// Takes a key of the text: a character, a deletion or a move. False for any other key.
    /// `width` is the columns the text was last drawn in.
    pub fn key(&mut self, key: Key, width: usize) -> bool {
        match key {
            Key::Char(ch) if !ch.is_control() => self.paste(ch.encode_utf8(&mut [0; 4])),
            Key::Ctrl(b'n') => self.paste("\n"),
            Key::Backspace | Key::Ctrl(b'h') => {
                if let Some(at) = self.before() {
                    self.text.replace_range(at..self.cursor, "");
                    self.cursor = at;
                }
            }
            Key::Delete => {
                if let Some(end) = self.after() {
                    self.text.replace_range(self.cursor..end, "");
                }
            }
            Key::Ctrl(b'w') => {
                let from = self.word_before();
                self.text.replace_range(from..self.cursor, "");
                self.cursor = from;
            }
            Key::Ctrl(b'u') => {
                let from = self.line_start();
                self.text.replace_range(from..self.cursor, "");
                self.cursor = from;
            }
            Key::Left => self.cursor = self.before().unwrap_or(self.cursor),
            Key::Right => self.cursor = self.after().unwrap_or(self.cursor),
            Key::Home | Key::Ctrl(b'a') => self.cursor = self.line_start(),
            Key::End | Key::Ctrl(b'e') => self.cursor = self.line_end(),
            Key::Up => self.move_rows(false, width),
            Key::Down => self.move_rows(true, width),
            _ => return false,
        }
        true
    }

    /// How many rows the text takes at `width` columns.
    pub fn height(&self, width: usize) -> usize {
        rows(&self.text, width).len()
    }

    /// Draws the text in `height` rows from (x, y), scrolled so that the cursor's row is among them.
    pub fn draw(&self, s: &mut Screen, x: usize, y: usize, width: usize, height: usize) {
        if width == 0 || height == 0 {
            return;
        }
        let rows = rows(&self.text, width);
        let at = self.row(&rows);
        let top = (at + 1).saturating_sub(height);
        for (i, row) in rows[top..].iter().take(height).enumerate() {
            s.put(x, y + i, width, &self.text[row.clone()], Style::PLAIN);
        }
        // A cursor past a full row waits in its last column.
        let col = str_width(&self.text[rows[at].start..self.cursor]).min(width - 1);
        s.emphasise(x + col, y + at - top, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Draft {
        let mut draft = Draft::new("");
        draft.paste(text);
        draft
    }

    #[test]
    fn long_text_wraps_after_a_space_and_a_long_word_is_cut() {
        let text = "rename this helper, abcdefghijklmnop\nthen test";
        let shown: Vec<&str> = rows(text, 12).into_iter().map(|row| &text[row]).collect();
        assert_eq!(shown, ["rename this ", "helper, ", "abcdefghijkl", "mnop", "then test"]);
    }

    #[test]
    fn keys_edit_at_the_cursor_and_move_it_by_wrapped_rows() {
        let mut draft = typed("one two three four");
        // Rows at eight columns: "one two ", "three ", "four".
        draft.key(Key::Up, 8);
        draft.key(Key::Up, 8);
        draft.key(Key::Char('X'), 8);
        assert_eq!(draft.text(), "one Xtwo three four");
        draft.key(Key::End, 8);
        draft.key(Key::Ctrl(b'w'), 8);
        draft.key(Key::Backspace, 8);
        draft.key(Key::Ctrl(b'n'), 8);
        draft.key(Key::Char('é'), 8);
        draft.key(Key::Left, 8);
        draft.key(Key::Delete, 8);
        assert_eq!(draft.text(), "one Xtwo three");
    }

    #[test]
    fn pasted_text_keeps_line_breaks_drops_controls_and_stops_at_the_limit() {
        let mut draft = typed("a\r\nb\tc\u{1b}[31m\u{7}");
        assert_eq!(draft.text(), "a\nb c[31m");
        draft.paste(&"x".repeat(NOTE_LIMIT));
        assert_eq!(draft.text().chars().count(), NOTE_LIMIT);
    }

    #[test]
    fn the_cursors_row_stays_in_view_in_a_short_field() {
        let draft = typed("first\nsecond\nthird");
        let mut screen = Screen::sized(12, 2);
        draft.draw(&mut screen, 0, 0, 12, 2);
        assert_eq!(screen.text(), ["second", "third"]);
    }
}
