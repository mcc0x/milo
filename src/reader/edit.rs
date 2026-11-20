//! Edits a working file inside the reader: a draft that reaches the disk only when saved.
//!
//! The draft is plain text with a cursor. Leaving with unsaved changes asks
//! first, and a reader closed from outside keeps its draft in the state folder.

use std::borrow::Cow;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::buffer::{Buffer, Pos};
use super::content::LIMIT;
use super::disk::{self, Original, SaveError};
use super::draw::LinePainter;
use super::rescue::{self, Draft, Mark};
use super::working_path::{Entry, LookupError};
use crate::diff::{Line, LineKind, TAB};
use crate::help::{self, HelpRow, Page};
use crate::keys::Key;
use crate::screen::{Screen, Style, YELLOW};
use crate::syntax::{Lang, Span, State};
use crate::ui::{self, FAINT};
use crate::width::{char_width, str_width};

const NOTICE: Duration = Duration::from_secs(4);
/// How many lines are looked at to learn how the file indents.
const INDENT_SAMPLE: usize = 400;

/// A question that takes the next key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Prompt {
    /// Leaving with changes that are not saved.
    Unsaved,
    /// The file on disk is not what the edit started from. `close` leaves after the save.
    Changed { close: bool },
    /// A draft was kept for this file when a reader was closed from outside.
    Kept,
}

/// What the reader does after a key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Step {
    Ignored,
    Stay,
    Help,
    /// Write the draft, then go back to reading if `close`.
    Save {
        close: bool,
        overwrite: bool,
    },
    /// Go back to reading; the draft is saved or given up.
    Close,
}

/// Where a reader's unsaved text is kept and announced.
pub(super) struct Safekeeping {
    pub(super) state: PathBuf,
    /// The file's full path.
    pub(super) file: PathBuf,
    /// The Rex block this reader runs in, if any.
    pub(super) block: Option<String>,
}

pub(super) struct Editor {
    buffer: Buffer,
    original: Original,
    cursor: Pos,
    /// The column kept while moving up and down through shorter lines.
    column: Option<usize>,
    top: usize,
    /// Display columns skipped on the left.
    pan: usize,
    /// Bring the cursor into view at the next draw.
    follow: bool,
    /// Rows and columns of text on screen and where they start, set by the last draw.
    rows: usize,
    columns: usize,
    text_x: usize,
    lang: Option<&'static Lang>,
    /// What is open at the start of each line, for as many lines as have been asked about.
    states: Vec<State>,
    spans: Vec<Span>,
    /// What the Tab key inserts.
    indent: &'static str,
    prompt: Option<Prompt>,
    notice: Option<(Cow<'static, str>, Instant)>,
    safekeeping: Option<Safekeeping>,
    mark: Option<Mark>,
    /// The user gave the draft up, so closing must not keep it.
    discarded: bool,
}

/// The display column at which the character at `byte` starts.
fn column_of(text: &str, byte: usize) -> usize {
    text[..byte].chars().fold(0, |col, c| col + shown_width(c, col))
}

/// Columns a character takes when it starts at `col`: a tab runs to the next stop.
fn shown_width(c: char, col: usize) -> usize {
    match c {
        '\t' => TAB - col % TAB,
        c if c.is_control() => 0,
        c => char_width(c),
    }
}

/// The byte of the character shown at `column`, or the end of the line.
fn byte_at(text: &str, column: usize) -> usize {
    let mut col = 0;
    for (byte, c) in text.char_indices() {
        let width = shown_width(c, col);
        if width > 0 && column < col + width {
            return byte;
        }
        col += width;
    }
    text.len()
}

/// A tab if the file indents with tabs, or as many spaces as its smallest indent.
fn indent_of(buffer: &Buffer) -> &'static str {
    const SPACES: &str = "        ";
    let mut least = usize::MAX;
    for row in 0..buffer.len().min(INDENT_SAMPLE) {
        let text = buffer.row(row);
        if text.starts_with('\t') {
            return "\t";
        }
        let spaces = text.bytes().take_while(|&c| c == b' ').count();
        if spaces >= 2 && spaces < text.len() {
            least = least.min(spaces);
        }
    }
    if least > SPACES.len() { &SPACES[..4] } else { &SPACES[..least] }
}

/// Pasted or restored text with the file's own line endings and no control characters.
fn clean(text: &str, newline: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                out.push_str(newline);
            }
            '\n' => out.push_str(newline),
            '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

impl Editor {
    /// Starts editing with the cursor on `row`, counted from the top of the file.
    pub(super) fn open(
        original: Original,
        lang: Option<&'static Lang>,
        row: usize,
        safekeeping: Option<Safekeeping>,
    ) -> Self {
        let buffer = Buffer::new(original.text());
        let lang = lang.filter(|lang| lang.agrees_with_shebang(buffer.row(0)));
        let row = row.min(buffer.len() - 1);
        let kept = safekeeping.as_ref().is_some_and(|keep| keep.draft().exists());
        Self {
            indent: indent_of(&buffer),
            buffer,
            original,
            cursor: Pos { row, col: 0 },
            column: None,
            top: row,
            pan: 0,
            follow: true,
            rows: 0,
            columns: 0,
            text_x: 0,
            lang,
            states: vec![State::Code],
            spans: Vec::new(),
            prompt: kept.then_some(Prompt::Kept),
            notice: None,
            safekeeping,
            mark: None,
            discarded: false,
        }
    }

    /// The line at the top of the view, counted from the top of the file.
    pub(super) fn top(&self) -> usize {
        self.top
    }

    #[cfg(test)]
    pub(super) fn dirty(&self) -> bool {
        self.buffer.dirty()
    }

    #[cfg(test)]
    pub(super) fn text(&self) -> String {
        self.buffer.text()
    }

    pub(super) fn next_frame(&self, now: Instant) -> Option<Duration> {
        self.notice.as_ref().map(|(_, until)| until.saturating_duration_since(now))
    }

    pub(super) fn say(&mut self, text: impl Into<Cow<'static, str>>, now: Instant) {
        self.notice = Some((text.into(), now + NOTICE));
    }

    /// Writes the draft over the file it was read from.
    pub(super) fn save_to(&mut self, entry: Result<&Entry<'_>, LookupError>, overwrite: bool) -> Result<(), SaveError> {
        disk::save(entry, &mut self.original, &self.buffer.text(), overwrite)
    }

    pub(super) fn saved(&mut self, now: Instant) {
        self.buffer.mark_saved();
        self.sync_mark();
        if let Some(keep) = &self.safekeeping {
            keep.draft().forget();
        }
        self.say("Saved", now);
    }

    pub(super) fn ask_overwrite(&mut self, close: bool) {
        self.prompt = Some(Prompt::Changed { close });
    }

    /// Announces unsaved edits to other panes for as long as there are any.
    fn sync_mark(&mut self) {
        if !self.buffer.dirty() {
            self.mark = None;
        } else if self.mark.is_none()
            && let Some(Safekeeping { state, block: Some(block), .. }) = &self.safekeeping
        {
            self.mark = Mark::set(state, block);
        }
    }

    fn row(&self) -> &str {
        self.buffer.row(self.cursor.row)
    }

    /// Records where an edit left the cursor and forgets the colours below its first line.
    fn edited(&mut self, first_row: usize, cursor: Pos) -> Step {
        self.cursor = cursor;
        self.column = None;
        self.follow = true;
        self.states.truncate(first_row + 1);
        self.sync_mark();
        Step::Stay
    }

    fn replace(&mut self, from: Pos, to: Pos, text: &str) -> Step {
        let end = self.buffer.edit(from, to, text, self.cursor);
        self.edited(from.row, end)
    }

    fn insert(&mut self, text: &str) -> Step {
        self.replace(self.cursor, self.cursor, text)
    }

    fn newline(&mut self) -> Step {
        let before = &self.row()[..self.cursor.col];
        let indent = &before[..before.len() - before.trim_start_matches([' ', '\t']).len()];
        let text = format!("{}{indent}", self.buffer.newline());
        self.insert(&text)
    }

    /// The place one character before the cursor, across a line break at the start of a line.
    fn before(&self) -> Option<Pos> {
        let Pos { row, col } = self.cursor;
        match self.row()[..col].chars().next_back() {
            Some(c) => Some(Pos { row, col: col - c.len_utf8() }),
            None if row > 0 => Some(Pos { row: row - 1, col: self.buffer.row(row - 1).len() }),
            None => None,
        }
    }

    fn after(&self) -> Option<Pos> {
        let Pos { row, col } = self.cursor;
        match self.row()[col..].chars().next() {
            Some(c) => Some(Pos { row, col: col + c.len_utf8() }),
            None if row + 1 < self.buffer.len() => Some(Pos { row: row + 1, col: 0 }),
            None => None,
        }
    }

    fn move_to(&mut self, to: Option<Pos>) -> Step {
        self.follow = true;
        let Some(to) = to.filter(|&to| to != self.cursor) else { return Step::Ignored };
        self.cursor = to;
        self.column = None;
        Step::Stay
    }

    /// Moves up or down, staying in the column the movement started from where a line reaches it.
    fn move_rows(&mut self, rows: isize) -> Step {
        self.follow = true;
        let row = self.cursor.row.saturating_add_signed(rows).min(self.buffer.len() - 1);
        if row == self.cursor.row {
            return Step::Ignored;
        }
        let column = *self.column.get_or_insert_with(|| column_of(self.buffer.row(self.cursor.row), self.cursor.col));
        self.cursor = Pos { row, col: byte_at(self.buffer.row(row), column) };
        Step::Stay
    }

    fn scroll(&mut self, rows: isize) -> Step {
        let last = self.buffer.len().saturating_sub(self.rows.max(1));
        let top = self.top.saturating_add_signed(rows).min(last);
        if top == self.top {
            return Step::Ignored;
        }
        self.top = top;
        self.follow = false;
        Step::Stay
    }

    fn click(&mut self, x: usize, y: usize) -> Step {
        if y == 0 || y > self.rows || x < self.text_x {
            return Step::Ignored;
        }
        let row = (self.top + y - 1).min(self.buffer.len() - 1);
        let col = byte_at(self.buffer.row(row), self.pan + x - self.text_x);
        self.move_to(Some(Pos { row, col }))
    }

    fn undo(&mut self, now: Instant) -> Step {
        match self.buffer.undo() {
            Some((cursor, row)) => self.edited(row, cursor),
            None => {
                self.say("Nothing to undo", now);
                Step::Stay
            }
        }
    }

    fn redo(&mut self, now: Instant) -> Step {
        match self.buffer.redo() {
            Some((cursor, row)) => self.edited(row, cursor),
            None => {
                self.say("Nothing to redo", now);
                Step::Stay
            }
        }
    }

    /// Asks before leaving unsaved work.
    fn leave(&mut self) -> Step {
        if !self.buffer.dirty() {
            return Step::Close;
        }
        self.prompt = Some(Prompt::Unsaved);
        Step::Stay
    }

    fn answer(&mut self, prompt: Prompt, key: Key, now: Instant) -> Step {
        let back = matches!(key, Key::Esc | Key::Ctrl(b'c'));
        let step = match (prompt, key) {
            (Prompt::Unsaved, Key::Char('s' | 'S') | Key::Ctrl(b's')) => Step::Save { close: true, overwrite: false },
            (Prompt::Unsaved, Key::Char('d' | 'D')) => {
                self.discarded = true;
                Step::Close
            }
            (Prompt::Changed { close }, Key::Char('o' | 'O')) => Step::Save { close, overwrite: true },
            (Prompt::Kept, Key::Char('r' | 'R')) => {
                self.prompt = None;
                return self.restore(now);
            }
            (Prompt::Kept, Key::Char('d' | 'D')) => {
                if let Some(keep) = &self.safekeeping {
                    keep.draft().forget();
                }
                Step::Stay
            }
            _ if back => Step::Stay,
            _ => return Step::Ignored,
        };
        self.prompt = None;
        step
    }

    /// Puts a kept draft in place of the text as one change, so that undo shows the file again.
    fn restore(&mut self, now: Instant) -> Step {
        let Some(draft) = self.safekeeping.as_ref().map(Safekeeping::draft) else { return Step::Stay };
        let Some(kept) = draft.read() else {
            self.say("The kept draft cannot be read.", now);
            return Step::Stay;
        };
        let kept = kept.strip_prefix('\u{feff}').unwrap_or(&kept);
        let step = self.replace(Pos::default(), self.buffer.end(), kept);
        draft.forget();
        self.cursor = Pos { row: self.top.min(self.buffer.len() - 1), col: 0 };
        self.say("Draft restored · ^Z undoes", now);
        step
    }

    /// Whether the draft can grow by `more` bytes and still be a file milo shows. Says so if not.
    fn has_room(&mut self, more: usize, now: Instant) -> bool {
        // Three more bytes allow for a byte-order mark, which the draft does not hold.
        let fits = (self.buffer.size() + more + 3) as u64 <= LIMIT;
        if !fits {
            self.say("The file would be too large to edit.", now);
        }
        fits
    }

    pub(super) fn paste(&mut self, text: Option<&str>, now: Instant) -> Step {
        if self.prompt.is_some() {
            return Step::Ignored;
        }
        match text {
            Some(text) => {
                let text = clean(text, self.buffer.newline());
                self.notice = None;
                if self.has_room(text.len(), now) { self.insert(&text) } else { Step::Stay }
            }
            None => {
                self.say("That paste was too long to take.", now);
                Step::Stay
            }
        }
    }

    pub(super) fn key(&mut self, key: Key, now: Instant) -> Step {
        if matches!(key, Key::Focus(_)) {
            return Step::Ignored;
        }
        if let Some(prompt) = self.prompt {
            return self.answer(prompt, key, now);
        }
        let had_notice = self.notice.take().is_some();
        let adds = match key {
            Key::Char(c) => c.len_utf8(),
            Key::Enter => self.buffer.newline().len() + self.cursor.col,
            Key::Ctrl(b'i') => self.indent.len(),
            _ => 0,
        };
        if !self.has_room(adds, now) {
            return Step::Stay;
        }
        let page = isize::try_from(self.rows.max(2) - 1).unwrap_or(isize::MAX);
        let step = match key {
            Key::Char(c) => {
                let end = self.buffer.type_char(self.cursor, c);
                self.edited(self.cursor.row, end)
            }
            Key::Enter => self.newline(),
            Key::Ctrl(b'i') => self.insert(self.indent),
            Key::Backspace | Key::Ctrl(b'h') => match self.before() {
                Some(from) => self.replace(from, self.cursor, ""),
                None => Step::Ignored,
            },
            Key::Delete | Key::Ctrl(b'd') => match self.after() {
                Some(to) => self.replace(self.cursor, to, ""),
                None => Step::Ignored,
            },
            Key::Left => self.move_to(self.before()),
            Key::Right => self.move_to(self.after()),
            Key::Up => self.move_rows(-1),
            Key::Down => self.move_rows(1),
            Key::PgUp => self.move_rows(-page),
            Key::PgDn => self.move_rows(page),
            Key::Home | Key::Ctrl(b'a') => self.move_to(Some(Pos { col: 0, ..self.cursor })),
            Key::End | Key::Ctrl(b'e') => self.move_to(Some(Pos { col: self.row().len(), ..self.cursor })),
            Key::WheelUp => self.scroll(-3),
            Key::WheelDown => self.scroll(3),
            Key::Click(x, y) => self.click(x, y),
            Key::Ctrl(b's') => Step::Save { close: false, overwrite: false },
            Key::Ctrl(b'z') => self.undo(now),
            Key::Ctrl(b'y') => self.redo(now),
            Key::Ctrl(b'g') => Step::Help,
            Key::Esc | Key::Ctrl(b'c' | b'q') => self.leave(),
            _ => Step::Ignored,
        };
        if step == Step::Ignored && had_notice { Step::Stay } else { step }
    }

    pub(super) fn help(&self) -> Page {
        use help::{heading, key, note};

        let rows: Vec<HelpRow> = vec![
            heading("Editing"),
            note("Changes stay a draft"),
            note("until you save them."),
            HelpRow::new(),
            heading("Keys"),
            key("^S       ", "save"),
            key("^Z ^Y    ", "undo, redo"),
            key("Esc      ", "stop editing"),
            key("←→↑↓     ", "move"),
            key("Home End ", "line ends"),
            key("PgUp PgDn", "move a page"),
            key("Tab      ", "indent"),
            key("^G       ", "this help"),
            HelpRow::new(),
            note("^ is the Ctrl key."),
            note("Click to place the"),
            note("cursor; paste works."),
        ];
        Page::new(rows)
    }

    /// What is open at the start of `row`, read from the last line already known.
    fn state_at(&mut self, row: usize) -> State {
        let Some(lang) = self.lang else { return State::Code };
        while self.states.len() <= row {
            let known = self.states.len() - 1;
            let mut state = self.states[known].clone();
            lang.advance(self.buffer.row(known), &mut state);
            self.states.push(state);
        }
        self.states[row].clone()
    }

    /// Moves the view just far enough to show the cursor.
    fn reveal(&mut self) {
        if self.cursor.row < self.top {
            self.top = self.cursor.row;
        } else if self.cursor.row >= self.top + self.rows {
            self.top = self.cursor.row + 1 - self.rows;
        }
        let column = column_of(self.row(), self.cursor.col);
        if column < self.pan {
            self.pan = column;
        } else if self.columns > 0 && column >= self.pan + self.columns {
            self.pan = column + 1 - self.columns;
        }
    }

    /// Draws the lines under the heading and the cursor among them.
    pub(super) fn draw_body(&mut self, s: &mut Screen, rows: usize) {
        let digits = self.buffer.len().ilog10() as usize + 1;
        let gutter = digits + 4;
        self.text_x = if s.w >= gutter + 3 { gutter } else { 0 };
        self.columns = s.w - 1 - self.text_x;
        self.rows = rows;
        if rows == 0 {
            return;
        }
        if self.follow {
            self.reveal();
        }
        self.top = self.top.min(self.buffer.len() - 1);
        let shown = (self.buffer.len() - self.top).min(rows);
        let mut spans = std::mem::take(&mut self.spans);
        for i in 0..shown {
            let row = self.top + i;
            let state = self.state_at(row);
            let number = u32::try_from(row + 1).unwrap_or(u32::MAX);
            let line = Line::new(LineKind::Same, number, self.buffer.row(row));
            let mut painter =
                LinePainter { x: self.text_x, width: self.columns, pan: self.pan, lang: self.lang, spans: &mut spans };
            painter.draw_line(s, i + 1, &line, &state);
        }
        self.spans = spans;
        self.draw_cursor(s);
        ui::thumb(s, s.w - 1, 1, rows, self.buffer.len(), self.top);
    }

    fn draw_cursor(&self, s: &mut Screen) {
        if self.prompt.is_some() || !(self.top..self.top + self.rows).contains(&self.cursor.row) {
            return;
        }
        let text = self.row();
        let column = column_of(text, self.cursor.col);
        let Some(offset) = column.checked_sub(self.pan).filter(|&offset| offset < self.columns) else { return };
        // A wide character takes the cursor across both of its cells; a tab shows it at its start.
        let under = text[self.cursor.col..].chars().next().filter(|&c| c != '\t').map_or(1, char_width);
        let width = under.clamp(1, self.columns - offset);
        s.emphasise(self.text_x + offset, 1 + self.cursor.row - self.top, width);
    }

    /// Says on the heading's right that this is a draft and whether it is saved. Returns where that starts.
    pub(super) fn draw_state(&self, s: &mut Screen) -> usize {
        const NAME_ROOM: usize = 10;
        let (word, style) = if self.buffer.dirty() { ("unsaved", Style::fg(YELLOW)) } else { ("editing", FAINT) };
        let right = s.w - 1;
        if word.len() + 2 <= right.saturating_sub(NAME_ROOM) {
            right - s.put_right(right, 0, word, style)
        } else {
            right
        }
    }

    /// The question or message that takes the place of the key hints, if there is one.
    fn message(&mut self, now: Instant) -> Option<(Cow<'static, str>, &'static [&'static str])> {
        if self.notice.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.notice = None;
        }
        match self.prompt {
            Some(Prompt::Unsaved) => Some(("Unsaved edits".into(), &["s save", "d discard", "Esc back"])),
            Some(Prompt::Changed { .. }) => Some(("Changed on disk".into(), &["o overwrite", "Esc back"])),
            Some(Prompt::Kept) => Some(("Kept draft found".into(), &["r restore", "d delete", "Esc later"])),
            None => self.notice.as_ref().map(|(text, _)| (text.clone(), [].as_slice())),
        }
    }

    /// Draws a question or message on row `y`, or returns false if there is none.
    pub(super) fn draw_message(&mut self, s: &mut Screen, y: usize, now: Instant) -> bool {
        let Some((label, choices)) = self.message(now) else { return false };
        let style = if self.prompt.is_some() { Style::fg(YELLOW) } else { Style::PLAIN };
        let margin = usize::from(s.w >= 12);
        let room = s.w - margin * 2;
        // Keep every choice on screen before the words that explain them.
        let text = if choices.is_empty() {
            label.into_owned()
        } else {
            let all = choices.join(" · ");
            [format!("{label}: {all}"), all]
                .into_iter()
                .find(|text| str_width(text) <= room)
                .unwrap_or_else(|| choices.join(" "))
        };
        for x in 0..s.w {
            s.put(x, y, 1, " ", Style::PLAIN);
        }
        s.put_fit(margin, y, room, &text, style);
        true
    }

    pub(super) fn draw_hint(&self, s: &mut Screen) {
        help::draw_hint(s, "^S save", &["^Z undo", "Esc done"], "^G help");
    }
}

impl Safekeeping {
    fn draft(&self) -> Draft {
        Draft::new(self.state.clone(), self.file.clone())
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        // Closed from outside with changes: the terminal went away, or the pane was ended.
        if self.buffer.dirty()
            && !self.discarded
            && let Some(keep) = &self.safekeeping
        {
            rescue::tell(keep.draft().keep(&self.original.bytes_for(&self.buffer.text())));
        }
    }
}
