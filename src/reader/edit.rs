//! Edits a working file inside the reader: a draft that reaches the disk only when saved.
//!
//! The draft is plain text with a cursor. Leaving with unsaved changes asks
//! first, and a reader closed from outside keeps its draft in the state folder.
//! A formatter changes the draft, never the file, and undo takes it back.

#[path = "compare.rs"]
mod compare;

use std::borrow::Cow;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::buffer::{Buffer, Pos};
use super::content::LIMIT;
use super::disk::{self, BOM, Original, SaveError, Snapshot};
use super::draw::LinePainter;
use super::rescue::{self, Draft, Drafts};
use super::working_path::{Entry, LookupError};
use crate::diff::{Line, LineKind, TAB};
use crate::format::{self, Support, Tool};
use crate::help::{self, HelpRow, Page};
use crate::keys::Key;
use crate::screen::{Screen, Style, YELLOW};
use crate::syntax::{Lang, Span, State};
use crate::ui::{self, FAINT};
use crate::width::{char_width, str_width};

use self::compare::Comparison;

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

/// Where the file being edited is, and what else on this machine an edit of it uses.
pub(super) struct Place {
    /// The file's full path.
    pub(super) file: PathBuf,
    /// The project's folder.
    pub(super) root: PathBuf,
    /// Milo's state folder, where a draft is kept if the reader is closed from outside.
    pub(super) state: Option<PathBuf>,
    /// The folders searched for a formatter, as `PATH` lists them.
    pub(super) tools: Option<OsString>,
}

impl Place {
    fn drafts(&self) -> Option<Drafts> {
        Some(Drafts::new(self.state.clone()?, self.file.clone()))
    }

    fn name(&self) -> &str {
        self.file.file_name().and_then(|name| name.to_str()).unwrap_or_default()
    }

    fn support(&self) -> Support {
        format::support(self.name(), self.tools.as_deref())
    }
}

/// A formatter the user asked for. It starts once a frame has said so,
/// because nothing can be drawn while it runs.
struct Formatting {
    tool: &'static Tool,
    program: PathBuf,
    announced: bool,
}

/// What is open at the start of each line, for as many lines as have been asked about.
struct Open {
    /// Right for the lines before `known`. Those from `kept` on were right
    /// before an edit above them, and are again if the line at `kept` still
    /// starts as it did; the ones between only hold their places.
    states: Vec<State>,
    known: usize,
    kept: usize,
}

impl Default for Open {
    fn default() -> Self {
        Self { states: vec![State::Code], known: 1, kept: 1 }
    }
}

impl Open {
    /// Reads on from the last line already known, and no further than an
    /// edit reaches: a line can be megabytes long, and so can those below it.
    fn at(&mut self, row: usize, lang: &Lang, buffer: &Buffer) -> State {
        while self.known <= row {
            let mut state = self.states[self.known - 1].clone();
            lang.advance(buffer.row(self.known - 1), &mut state);
            if self.known == self.kept {
                if self.states.get(self.known) == Some(&state) {
                    self.known = self.states.len();
                    self.kept = self.known;
                    continue;
                }
                self.states.truncate(self.known);
            }
            match self.states.get_mut(self.known) {
                Some(place) => *place = state,
                None => self.states.push(state),
            }
            self.known += 1;
            self.kept = self.kept.max(self.known);
        }
        self.states[row].clone()
    }

    /// Notes that lines `first..=last` took the place of lines `first..=old_last`.
    fn rewrote(&mut self, first: usize, old_last: usize, last: usize) {
        let kept = if self.kept < self.states.len() { self.kept } else { old_last + 1 };
        if old_last < kept && kept < self.states.len() {
            self.states.splice(first + 1..old_last + 1, std::iter::repeat_n(State::Code, last - first));
            self.kept = kept + last - old_last;
            self.known = self.known.min(first + 1);
        } else {
            self.states.truncate(self.known.min(first + 1));
            self.known = self.states.len();
            self.kept = self.known;
        }
    }
}

pub(super) struct Editor {
    buffer: Buffer,
    original: Original,
    place: Place,
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
    open: Open,
    spans: Vec<Span>,
    /// What the Tab key inserts.
    indent: &'static str,
    prompt: Option<Prompt>,
    /// The exact disk version offered for comparison or confirmed for overwrite.
    conflict: Option<Snapshot>,
    comparison: Option<Comparison>,
    notice: Option<(Cow<'static, str>, Instant)>,
    formatting: Option<Formatting>,
    /// The user gave the draft up, so closing must not keep it.
    discarded: bool,
    /// The kept copy offered by the prompt.
    kept: Option<Draft>,
    /// The kept copy whose text was put in place, if one was.
    restored: Option<Restored>,
}

struct Restored {
    /// The change that put the kept text in place.
    change: u64,
    /// The kept copy, until the restored text is successfully saved.
    draft: Option<Draft>,
    /// The file held other text than the draft was written over, or it cannot be told: a save
    /// would replace text that whoever wrote the draft never saw.
    over_other_text: bool,
    /// Whether the kept draft had a structural mark, independent of the file on disk.
    marked: bool,
}

impl Restored {
    fn unconfirmed(&self) -> Option<&Draft> {
        self.draft.as_ref().filter(|_| self.over_other_text)
    }
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

/// Where the part of a line that is read to draw it up to `column` ends: at
/// the second character to start there or further on, so that the first is
/// whole and keeps what is attached to it.
fn read_end(text: &str, column: usize) -> usize {
    // Most lines are no longer than this, and are read whole without being measured.
    if text.len() <= column {
        return text.len();
    }
    let (mut col, mut reached) = (0, false);
    for (byte, c) in text.char_indices() {
        let width = shown_width(c, col);
        if width > 0 {
            if reached {
                return byte;
            }
            reached = col >= column;
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

/// Rows for a help page that say whether this file can be formatted here, and with what.
pub(super) fn format_help(name: &str, tools: Option<&std::ffi::OsStr>) -> Vec<HelpRow> {
    use help::note;

    match format::support(name, tools) {
        Support::Ready(tool, _) => vec![vec![("  ", Style::PLAIN), ("Formatter: ", FAINT), (tool.name, Style::PLAIN)]],
        Support::Missing(names) => vec![note("No formatter installed:"), note(names)],
        Support::Unknown => vec![note("No formatter for this"), note("kind of file.")],
    }
}

impl Editor {
    /// Starts editing with the cursor on `row`, counted from the top of the file.
    pub(super) fn open(original: Original, lang: Option<&'static Lang>, row: usize, place: Place) -> Self {
        let buffer = Buffer::new(original.text());
        let lang = lang.filter(|lang| lang.agrees_with_shebang(buffer.row(0)));
        let row = row.min(buffer.len() - 1);
        let kept = place.drafts().and_then(|drafts| drafts.first());
        Self {
            indent: indent_of(&buffer),
            buffer,
            original,
            place,
            cursor: Pos { row, col: 0 },
            column: None,
            top: row,
            pan: 0,
            follow: true,
            rows: 0,
            columns: 0,
            text_x: 0,
            lang,
            open: Open::default(),
            spans: Vec::new(),
            prompt: kept.is_some().then_some(Prompt::Kept),
            conflict: None,
            comparison: None,
            notice: None,
            formatting: None,
            discarded: false,
            kept,
            restored: None,
        }
    }

    /// The line at the top of the view, counted from the top of the file.
    pub(super) fn top(&self) -> usize {
        self.top
    }

    /// Whether the draft differs from what was last saved.
    pub(super) fn dirty(&self) -> bool {
        self.buffer.dirty()
    }

    /// Whether a question is waiting for its answer.
    pub(super) fn asking(&self) -> bool {
        self.prompt.is_some()
    }

    #[cfg(test)]
    pub(super) fn text(&self) -> String {
        self.buffer.text()
    }

    /// How long the frame on screen stays right: until a message goes, or no
    /// time at all once it has said that a formatter is about to run.
    pub(super) fn next_frame(&self, now: Instant) -> Option<Duration> {
        if self.formatting.as_ref().is_some_and(|formatting| formatting.announced) {
            return Some(Duration::ZERO);
        }
        self.notice.as_ref().map(|(_, until)| until.saturating_duration_since(now))
    }

    pub(super) fn say(&mut self, text: impl Into<Cow<'static, str>>, now: Instant) {
        self.notice = Some((text.into(), now + NOTICE));
    }

    /// Writes the draft over the file it was read from.
    pub(super) fn save_to(&mut self, entry: Result<&Entry<'_>, LookupError>, overwrite: bool) -> Result<(), SaveError> {
        let expected = self.conflict.take().filter(|_| overwrite);
        if overwrite && expected.is_none() {
            return Err(SaveError::Refused("Not saved: confirm the disk version first."));
        }
        let restored = self.restored();
        let basis = match &expected {
            Some(confirmed) => disk::Basis::Confirmed(confirmed),
            None if restored.and_then(Restored::unconfirmed).is_some() => disk::Basis::Unconfirmed,
            None => disk::Basis::Original,
        };
        let marked = self.write_mark(restored);
        self.conflict = disk::save(entry, &mut self.original, || disk::with_mark(self.buffer.text(), marked), basis)?;
        if self.conflict.is_some() { Err(SaveError::Changed) } else { Ok(()) }
    }

    /// A kept draft stays until the user deletes it or saves the text restored
    /// from it. Saving something else, or the file again after undoing the
    /// restore, is not an answer to that question.
    pub(super) fn saved(&mut self, now: Instant) {
        self.buffer.mark_saved();
        if let Some(restored) = &mut self.restored
            && self.buffer.holds(restored.change)
            && let Some(draft) = restored.draft.take()
        {
            draft.forget();
        }
        self.say("Saved", now);
    }

    pub(super) fn ask_overwrite(&mut self, close: bool) {
        self.prompt = Some(Prompt::Changed { close });
    }

    fn row(&self) -> &str {
        self.buffer.row(self.cursor.row)
    }

    /// Records an edit that put lines `first_row..=last_row` where lines
    /// `first_row..=old_last_row` were, and where it left the cursor.
    fn edited(&mut self, first_row: usize, old_last_row: usize, last_row: usize, cursor: Pos) -> Step {
        self.cursor = cursor;
        self.column = None;
        self.follow = true;
        self.open.rewrote(first_row, old_last_row, last_row);
        Step::Stay
    }

    fn replace(&mut self, from: Pos, to: Pos, text: &str) -> Step {
        let end = self.buffer.edit(from, to, text, self.cursor);
        self.edited(from.row, to.row, end.row, end)
    }

    fn insert(&mut self, text: &str) -> Step {
        self.replace(self.cursor, self.cursor, text)
    }

    /// The spaces and tabs before the cursor that start its line: what a new line begins with.
    fn indent_before_cursor(&self) -> &str {
        let before = &self.row()[..self.cursor.col];
        &before[..before.len() - before.trim_start_matches([' ', '\t']).len()]
    }

    fn newline(&mut self) -> Step {
        let text = format!("{}{}", self.buffer.newline(), self.indent_before_cursor());
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
            Some(replay) => self.edited(replay.first_row, replay.old_last_row, replay.last_row, replay.cursor),
            None => {
                self.say("Nothing to undo", now);
                Step::Stay
            }
        }
    }

    fn redo(&mut self, now: Instant) -> Step {
        match self.buffer.redo() {
            Some(replay) => self.edited(replay.first_row, replay.old_last_row, replay.last_row, replay.cursor),
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
            (Prompt::Changed { .. }, Key::Char('c' | 'C')) => {
                if let Some(snapshot) = self.conflict.take() {
                    let draft = disk::with_mark(self.buffer.text(), self.write_mark(self.restored()));
                    self.comparison = Some(Comparison::new(snapshot, draft));
                }
                return Step::Stay;
            }
            (Prompt::Kept, Key::Char('r' | 'R')) => {
                self.prompt = None;
                return self.restore(now);
            }
            (Prompt::Kept, Key::Char('d' | 'D')) => {
                if let Some(draft) = self.kept.take() {
                    draft.forget();
                }
                Step::Stay
            }
            _ if back => {
                self.conflict = None;
                Step::Stay
            }
            _ => return Step::Ignored,
        };
        self.prompt = None;
        step
    }

    /// The change that restored text, if it is still part of the edit.
    fn restored(&self) -> Option<&Restored> {
        self.restored.as_ref().filter(|restored| self.buffer.holds(restored.change))
    }

    /// A matching restore resumes the source editor's encoding, even after its first save
    /// removes the kept copy. Undoing it or restoring over a different base uses the current file.
    fn write_mark(&self, restored: Option<&Restored>) -> bool {
        restored.filter(|restored| !restored.over_other_text).map_or(self.original.marked(), |restored| restored.marked)
    }

    /// Puts a kept draft in place of the text as one change, so that undo shows the file again.
    fn restore(&mut self, now: Instant) -> Step {
        let Some(draft) = self.kept.take() else { return Step::Stay };
        let Some(kept) = draft.read() else {
            self.say("The kept draft cannot be read.", now);
            return Step::Stay;
        };
        let original = self.original.read();
        let over_other_text = !draft.written_over(original);
        let unmarked = kept.strip_prefix(BOM).filter(|_| draft.marked(original, !over_other_text));
        let step = self.replace(Pos::default(), self.buffer.end(), unmarked.unwrap_or(&kept));
        // The kept copy stays until this text is saved: undoing the restore must not be the end of it.
        let marked = unmarked.is_some();
        self.restored =
            Some(Restored { change: self.buffer.last_change(), draft: Some(draft), over_other_text, marked });
        self.cursor = Pos { row: self.top.min(self.buffer.len() - 1), col: 0 };
        let over = if over_other_text { " over a changed file" } else { "" };
        self.say(format!("Draft restored{over} · ^Z undoes"), now);
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
        self.format_pending();
        match text {
            Some(text) => {
                let text = clean(text, self.buffer.newline());
                self.notice = None;
                if text.is_empty() {
                    Step::Ignored
                } else if self.has_room(text.len(), now) {
                    self.insert(&text)
                } else {
                    Step::Stay
                }
            }
            None => {
                self.say("That paste was too long to take.", now);
                Step::Stay
            }
        }
    }

    /// Finds the formatter for this file, or says why there is none to run.
    fn ask_format(&mut self, now: Instant) -> Step {
        match self.place.support() {
            Support::Ready(tool, program) => self.formatting = Some(Formatting { tool, program, announced: false }),
            Support::Missing(names) => self.say(format!("Not installed: {names}."), now),
            Support::Unknown => self.say("No formatter for this kind of file.", now),
        }
        Step::Stay
    }

    /// Runs the formatter that the last frame announced.
    pub(super) fn format_if_announced(&mut self) {
        if self.formatting.as_ref().is_some_and(|formatting| formatting.announced) {
            self.format_pending();
        }
    }

    /// Runs the formatter that was asked for, if one still is, and puts its
    /// text in place of the draft as one change. A failure leaves the draft as
    /// it was. Whatever is typed after Ctrl-F acts on the formatted text, so
    /// this runs before the next key even when no frame has announced it.
    fn format_pending(&mut self) {
        let Some(Formatting { tool, program, .. }) = self.formatting.take() else { return };
        let text = self.buffer.text();
        let dir = self.place.file.parent().unwrap_or(Path::new("."));
        let draft = format::Draft { text: &text, name: self.place.name(), dir, root: &self.place.root };
        let result = format::run(tool, &program, &draft);
        // What is said stays its full time from when the formatter ended, however long it took.
        let now = Instant::now();
        let formatted = match result {
            Ok(formatted) => formatted,
            Err(failure) => return self.say(failure.sentence(tool.name), now),
        };
        // A formatter that writes its own line endings does not get to change the file's.
        let formatted = match self.buffer.newline() {
            "\r\n" if !formatted.contains("\r\n") => formatted.replace('\n', "\r\n"),
            _ => formatted,
        };
        if formatted == text {
            self.say(format!("Already formatted ({})", tool.name), now);
        } else if formatted.len() as u64 + 3 > LIMIT {
            self.say("The formatted file would be too large.", now);
        } else {
            let Pos { row, col } = self.cursor;
            self.replace(Pos::default(), self.buffer.end(), &formatted);
            let row = row.min(self.buffer.len() - 1);
            let line = self.buffer.row(row);
            let col = (0..=col.min(line.len())).rev().find(|&at| line.is_char_boundary(at)).unwrap_or(0);
            self.cursor = Pos { row, col };
            self.say(format!("Formatted with {} · ^Z undoes", tool.name), now);
        }
    }

    pub(super) fn key(&mut self, key: Key, now: Instant) -> Step {
        if matches!(key, Key::Focus(_)) {
            return Step::Ignored;
        }
        if let Some(comparison) = &mut self.comparison {
            return match key {
                Key::Esc | Key::Ctrl(b'c') => {
                    self.conflict = self.comparison.take().map(Comparison::into_snapshot);
                    Step::Stay
                }
                Key::Char('?') | Key::Ctrl(b'g') => Step::Help,
                _ if comparison.key(key) => Step::Stay,
                _ => Step::Ignored,
            };
        }
        self.format_pending();
        if let Some(prompt) = self.prompt {
            return self.answer(prompt, key, now);
        }
        let had_notice = self.notice.take().is_some();
        let adds = match key {
            Key::Char(c) => c.len_utf8(),
            Key::Enter => self.buffer.newline().len() + self.indent_before_cursor().len(),
            Key::Ctrl(b'i') => self.indent.len(),
            _ => 0,
        };
        // A key that adds nothing must work in a file that is already as large as one can be.
        if adds > 0 && !self.has_room(adds, now) {
            return Step::Stay;
        }
        let page = isize::try_from(self.rows.max(2) - 1).unwrap_or(isize::MAX);
        let step = match key {
            // A terminal can send a control character as text; it is not something to type.
            Key::Char(c) if c.is_control() => Step::Ignored,
            Key::Char(c) => {
                let end = self.buffer.type_char(self.cursor, c);
                self.edited(self.cursor.row, self.cursor.row, self.cursor.row, end)
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
            Key::Ctrl(b'f') => self.ask_format(now),
            Key::Ctrl(b'g') => Step::Help,
            Key::Esc | Key::Ctrl(b'c' | b'q') => self.leave(),
            _ => Step::Ignored,
        };
        if step == Step::Ignored && had_notice { Step::Stay } else { step }
    }

    pub(super) fn help(&self) -> Page {
        use help::{heading, key, note};

        if self.comparison.is_some() {
            use crate::ui::{ADD, DEL};

            return Page::new(vec![
                heading("Compare"),
                help::sample("-", DEL, "on disk at save"),
                help::sample("+", ADD.bold(), "your draft"),
                note("Nothing is written."),
                HelpRow::new(),
                key("↑↓       ", "scroll"),
                key("←→       ", "pan"),
                key("PgUp PgDn", "page"),
                key("g G      ", "top / bottom"),
                key("Esc      ", "save choices"),
            ]);
        }

        let mut rows: Vec<HelpRow> = vec![
            heading("Editing"),
            note("Changes stay a draft"),
            note("until you save them."),
            HelpRow::new(),
            heading("Keys"),
            key("^S       ", "save"),
            key("^Z ^Y    ", "undo, redo"),
            key("^F       ", "format"),
            key("Esc      ", "stop editing"),
            key("←→↑↓     ", "move"),
            key("Home End ", "line ends"),
            key("PgUp PgDn", "move a page"),
            key("Tab      ", "indent"),
            key("^G       ", "this help"),
            HelpRow::new(),
            note("^ is the Ctrl key."),
            note("If the disk changed,"),
            note("c compares disk/draft."),
            note("Click to place the"),
            note("cursor; paste works."),
            HelpRow::new(),
            heading("Formatting"),
        ];
        rows.extend(format_help(self.place.name(), self.place.tools.as_deref()));
        rows.extend([note("It changes the draft,"), note("not the file. ^Z undoes.")]);
        Page::new(rows)
    }

    /// What is open at the start of `row`.
    fn state_at(&mut self, row: usize) -> State {
        let Some(lang) = self.lang else { return State::Code };
        self.open.at(row, lang, &self.buffer)
    }

    /// Moves the view just far enough to show the cursor.
    fn reveal(&mut self) {
        if self.cursor.row < self.top {
            self.top = self.cursor.row;
        } else if self.cursor.row >= self.top + self.rows {
            self.top = self.cursor.row + 1 - self.rows;
        }
        let text = self.buffer.row(self.cursor.row);
        let column = column_of(text, self.cursor.col);
        if column < self.columns {
            // A line that fits is shown from its start, whatever a longer one needed.
            self.pan = 0;
        } else if column < self.pan {
            self.pan = column;
        } else if column >= self.pan + self.columns {
            self.pan = column + 1 - self.columns;
        }
        // A view that starts inside a wide character would draw this line one column off its cursor.
        let mut start = 0;
        for c in text.chars() {
            let end = start + shown_width(c, start);
            if start < self.pan && self.pan < end {
                self.pan = if column >= end { end } else { start };
            }
            if end >= self.pan {
                break;
            }
            start = end;
        }
    }

    /// Draws the lines under the heading and the cursor among them.
    pub(super) fn draw_body(&mut self, s: &mut Screen, rows: usize) {
        if let Some(comparison) = &mut self.comparison {
            comparison.draw(s, rows);
            return;
        }
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
        let mut painter =
            LinePainter { x: self.text_x, width: self.columns, pan: self.pan, lang: self.lang, spans: &mut spans };
        let reach = painter.reach();
        for i in 0..shown {
            let row = self.top + i;
            // An empty line has nothing to colour. The last line of a file that ends with a
            // line break is one, and what it starts in is found by reading all of the line above.
            let state = if self.buffer.row(row).is_empty() { State::Code } else { self.state_at(row) };
            let number = u32::try_from(row + 1).unwrap_or(u32::MAX);
            // A line can be megabytes long; only what can be drawn of it is made ready to draw.
            let text = self.buffer.row(row);
            let line = Line::new(LineKind::Same, number, &text[..read_end(text, reach)]);
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
        if self.comparison.is_some() {
            return Comparison::legend(s);
        }
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
            Some(Prompt::Changed { .. }) => Some(("Changed on disk".into(), &["c compare", "o overwrite", "Esc back"])),
            Some(Prompt::Kept) => Some(("Kept draft found".into(), &["r restore", "d delete", "Esc later"])),
            None => {
                let text = match &mut self.formatting {
                    Some(formatting) => {
                        formatting.announced = true;
                        format!("Formatting with {}…", formatting.tool.name).into()
                    }
                    None => self.notice.as_ref()?.0.clone(),
                };
                Some((text, &[]))
            }
        }
    }

    /// Draws a question or message on row `y`, or returns false if there is none.
    pub(super) fn draw_message(&mut self, s: &mut Screen, y: usize, now: Instant) -> bool {
        if self.comparison.is_some() {
            return false;
        }
        let Some((label, choices)) = self.message(now) else { return false };
        let style = if self.prompt.is_some() { Style::fg(YELLOW) } else { Style::PLAIN };
        // Keep every choice on screen before the words that explain them, and before the margins.
        let inset = s.w.saturating_sub(2);
        let text = if choices.is_empty() {
            label.into_owned()
        } else {
            let all = choices.join(" · ");
            [format!("{label}: {all}"), all].into_iter().find(|text| str_width(text) <= inset).unwrap_or_else(|| {
                if matches!(self.prompt, Some(Prompt::Changed { .. })) {
                    ["c compare o overwrite Esc", "c compare o write Esc", "c cmp o write Esc", "c/o/Esc"]
                        .into_iter()
                        .find(|text| text.len() <= s.w)
                        .unwrap_or("c/o/Esc")
                        .to_owned()
                } else {
                    choices.join(" ")
                }
            })
        };
        let margin = usize::from(str_width(&text) <= inset);
        for x in 0..s.w {
            s.put(x, y, 1, " ", Style::PLAIN);
        }
        s.put_fit(margin, y, s.w - margin * 2, &text, style);
        true
    }

    pub(super) fn draw_hint(&self, s: &mut Screen) {
        if let Some(comparison) = &self.comparison {
            comparison.hint(s);
            return;
        }
        let middle: &[_] = if format::known(self.place.name()) {
            &["^Z undo", "^F format", "Esc done"]
        } else {
            &["^Z undo", "Esc done"]
        };
        help::draw_hint(s, "^S save", middle, "^G help");
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        // Closed from outside with changes: the terminal went away, or the pane was ended.
        if !self.buffer.dirty() || self.discarded {
            return;
        }
        let restored = self.restored();
        let unconfirmed = restored.and_then(|restored| restored.unconfirmed().map(|draft| (draft, restored.marked)));
        // Keeping an unconfirmed edit again preserves the source base and encoding.
        let (base, marked) = match unconfirmed {
            Some((draft, marked)) => (draft.base().map(Cow::Borrowed), marked),
            None => (Some(Cow::Owned(rescue::base(self.original.read()))), self.write_mark(restored)),
        };
        let text = self.buffer.text();
        let predecessor = restored.and_then(|restored| restored.draft.as_ref());
        let note = match self.place.drafts() {
            Some(drafts) => drafts.keep(&text, marked, base.as_deref(), predecessor),
            None => format!("unsaved edits to {} were lost: milo has no state folder.", self.place.name()),
        };
        rescue::tell(note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use crate::reader::Reader;

    #[test]
    fn history_keeps_the_syntax_prefix_and_rescans_a_changed_line_ending() {
        let f = Fixture::folder();
        let row = 100;
        let file = f.root.join("edit.rs");
        f.write("edit.rs", &("// prefix\n".repeat(row) + "/\n*\rX\nlet answer = 42;\n*/\n"));
        let mut editor = Reader::open(&file).unwrap().open_editor().unwrap();
        let now = Instant::now();
        assert_eq!(editor.state_at(row + 2), State::Code);
        let from = Pos { row, col: 1 };
        editor.cursor = Pos { row: row + 1, col: 3 };
        editor.replace(from, editor.cursor, "*\r");
        assert_eq!(editor.buffer.row(row), "/*");
        assert!(matches!(editor.state_at(row + 1), State::Comment(..)));

        editor.undo(now);
        assert_eq!(editor.open.known, row + 1);
        assert_eq!(editor.buffer.row(row + 1), "*\rX");
        assert_eq!(editor.state_at(row + 2), State::Code);
        editor.redo(now);
        assert_eq!(editor.open.known, row + 1);
        assert_eq!(editor.cursor, Pos { row, col: 2 });
        assert!(matches!(editor.state_at(row + 1), State::Comment(..)));
    }

    /// What every line starts in, read afresh from the top of the draft.
    fn fresh_states(editor: &Editor) -> Vec<State> {
        let lang = editor.lang.unwrap();
        let mut state = State::Code;
        (0..editor.buffer.len())
            .map(|row| {
                let before = state.clone();
                lang.advance(editor.buffer.row(row), &mut state);
                before
            })
            .collect()
    }

    #[test]
    fn colours_below_an_edit_are_those_of_reading_the_whole_draft_again() {
        const PIECES: [&str; 10] =
            ["/*", "*/", "let a = 1;", "let s = r#\"", "\"#;", "// note", "\"text", "", "/* /* */", "x */ y"];
        let f = Fixture::folder();
        f.write("edit.rs", &PIECES.join("\n").repeat(6));
        let mut editor = Reader::open(&f.root.join("edit.rs")).unwrap().open_editor().unwrap();
        let now = Instant::now();
        let mut seed = 0x4f6c_dd1d_usize;
        let mut next = |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % below
        };
        for step in 0..4000 {
            let rows = editor.buffer.len();
            match next(10) {
                0 => drop(editor.undo(now)),
                1 => drop(editor.redo(now)),
                // What a frame asks: some of the lines, not always those just edited.
                2..=4 => {
                    let row = next(rows);
                    assert_eq!(editor.state_at(row), fresh_states(&editor)[row], "step {step}, row {row}");
                }
                _ => {
                    let from = Pos { row: next(rows), col: 0 };
                    let last = (from.row + next(3)).min(rows - 1);
                    let to = Pos { row: last, col: next(editor.buffer.row(last).len() + 1) };
                    let text: Vec<_> = (0..next(4)).map(|_| PIECES[next(PIECES.len())]).collect();
                    editor.replace(from, to, &text.join("\n"));
                }
            }
            if step % 64 == 0 {
                let fresh = fresh_states(&editor);
                let read: Vec<_> = (0..fresh.len()).rev().map(|row| editor.state_at(row)).collect();
                assert!(read.iter().rev().eq(&fresh), "step {step}");
            }
        }
    }

    #[test]
    fn typing_reads_no_further_than_the_line_whose_start_it_changes() {
        let f = Fixture::folder();
        f.write("edit.rs", &"let a = 1;\n".repeat(200));
        let mut editor = Reader::open(&f.root.join("edit.rs")).unwrap().open_editor().unwrap();
        editor.state_at(150);
        editor.cursor = Pos { row: 20, col: 0 };
        editor.key(Key::Char('x'), Instant::now());
        assert_eq!(editor.open.known, 21);
        editor.state_at(21);
        assert_eq!(editor.open.known, 151);
        // An opened comment changes what the lines below start in, so they are read again.
        editor.key(Key::Char('/'), Instant::now());
        editor.key(Key::Char('*'), Instant::now());
        assert!(matches!(editor.state_at(22), State::Comment(..)));
        assert_eq!(editor.open.known, 23);
    }

    #[test]
    fn an_empty_line_is_drawn_without_reading_the_line_above_it_for_what_it_starts_in() {
        let f = Fixture::folder();
        f.write("one.rs", "let a = 1;\n");
        f.write("open.rs", "/* open\n\nstill a comment\n");
        let mut screen = Screen::sized(40, 6);
        let mut editor = Reader::open(&f.root.join("one.rs")).unwrap().open_editor().unwrap();
        editor.draw_body(&mut screen, 4);
        assert_eq!(editor.open.known, 1);

        // A line below an empty one is still coloured by what is open above them both.
        let mut editor = Reader::open(&f.root.join("open.rs")).unwrap().open_editor().unwrap();
        editor.draw_body(&mut screen, 4);
        let out = String::from_utf8(screen.emitted()).unwrap();
        assert!(out.contains("\x1b[0;3;2mstill a comment"), "{out:?}");
    }

    #[test]
    fn a_line_drawn_from_the_part_that_is_read_looks_as_the_whole_line_does() {
        const PIECES: [&str; 14] = [
            "\t", "a", " ", "let x = \"s\"; ", "界", "e\u{301}", "\u{200b}", "\x07", "r#\"raw\"# ", "// c ", "call(",
            "🟢", "\"open ", "'c'",
        ];
        let mut seed = 0x2545_f491_usize;
        let mut next = |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % below
        };
        let langs = [None, crate::syntax::for_file("a.rs"), crate::syntax::for_file("a.py")];
        for case in 0..3000 {
            let text: String = (0..next(400)).map(|_| PIECES[next(PIECES.len())]).collect();
            let (width, pan, lang) = (1 + next(30), next(120), langs[next(langs.len())]);
            let frames = [false, true].map(|part| {
                let mut spans = Vec::new();
                let mut painter = LinePainter { x: 5, width, pan, lang, spans: &mut spans };
                let end = if part { read_end(&text, painter.reach()) } else { text.len() };
                let mut screen = Screen::sized(5 + width + 1, 2);
                painter.draw_line(&mut screen, 1, &Line::new(LineKind::Same, 7, &text[..end]), &State::Code);
                (end, screen.emitted())
            });
            assert_eq!(frames[0].1, frames[1].1, "case {case}: {width} wide at {pan}, {text:?}");
            // And no more is read than the columns asked for and one character past them.
            let read = column_of(&text, frames[1].0);
            assert!(frames[1].0 == text.len() || read <= pan + width + 260 + 2 * TAB, "case {case}");
        }
    }
}
