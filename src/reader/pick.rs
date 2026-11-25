//! A line cursor over a handed-over patch, for choosing what a note is about.
//!
//! *Picking* is moving that cursor, alone or with a range held from an anchor.
//! It reads the lines on screen and changes nothing but where the view is.

use std::ops::RangeInclusive;

use super::changes::Row;
use super::{Body, Reader};
use crate::diff::{Line, LineKind};
use crate::keys::Key;
use crate::screen::{Screen, Style, Wash, YELLOW};

/// The most lines of a pick that are kept to quote.
pub const QUOTED: usize = 12;
/// The most characters of one line that are kept to quote.
const QUOTE_WIDTH: usize = 200;

#[derive(Clone, Copy)]
pub(super) struct Pick {
    /// The line the cursor is on, counted through the lines shown, removed ones included.
    cursor: usize,
    /// The other end of a range, while one is held.
    anchor: Option<usize>,
}

/// One line as it was shown when it was picked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PickedLine {
    pub kind: LineKind,
    /// The line's number in the file now, or in the patch's base for a removed line.
    pub number: u32,
    pub text: String,
}

/// The lines under a pick, as the review showed them.
pub struct Picked {
    /// Where the first line is among the lines shown.
    pub at: usize,
    /// How many lines were picked; `lines` holds at most the first `QUOTED`.
    pub len: usize,
    pub lines: Vec<PickedLine>,
    /// First and last number among the picked lines the file has now.
    pub kept: Option<(u32, u32)>,
    /// First and last number, in the patch's base, among the picked lines it removed.
    pub removed: Option<(u32, u32)>,
}

/// Where a reader was showing its file, to come back to.
#[derive(Clone, Copy)]
pub struct Viewed {
    top: usize,
    pan: usize,
}

fn quoted(line: &Line) -> PickedLine {
    let mut text: String = line.text.chars().take(QUOTE_WIDTH).collect();
    if text.len() < line.text.len() {
        text.push('…');
    }
    PickedLine { kind: line.kind, number: line.number, text }
}

fn span(numbers: impl Iterator<Item = u32>) -> Option<(u32, u32)> {
    numbers.fold(None, |span, n| Some(span.map_or((n, n), |(first, last): (u32, u32)| (first.min(n), last.max(n)))))
}

impl Reader {
    /// Puts the cursor on a line in view: the change last jumped to, else the
    /// first changed line on screen, else the first line on screen. False
    /// where there are no lines to pick.
    pub fn start_pick(&mut self) -> bool {
        let Body::Text(text) = &self.body else { return false };
        if self.markdown_body().is_some() && !self.markdown_source {
            return false;
        }
        let folds = self.folds();
        let total = folds.row_count(text.lines.len());
        let end = (self.top + self.rows.max(1)).min(total);
        let shown = || {
            folds.rows(self.top.min(end)..end).filter_map(|row| match row {
                Row::Line(line) => Some(line),
                Row::Fold(_) => None,
            })
        };
        let jumped = self.current_change.and_then(|change| text.changes.get(change).copied());
        let cursor = jumped
            .filter(|line| shown().any(|shown| shown == *line))
            .or_else(|| shown().find(|&line| text.lines[line].kind != LineKind::Same))
            .or_else(|| shown().next());
        let Some(cursor) = cursor else { return false };
        self.pick = Some(Pick { cursor, anchor: None });
        self.opening = false;
        true
    }

    pub fn end_pick(&mut self) {
        self.pick = None;
    }

    pub fn place(&self) -> Viewed {
        Viewed { top: self.top, pan: self.pan }
    }

    /// Shows the file from where it was shown at `place`.
    pub fn return_to(&mut self, place: Viewed) {
        self.top = place.top.min(self.last_top());
        self.pan = place.pan;
    }

    /// Moves the cursor, or holds or lets go of a range. False for a key that is not one of picking's.
    pub fn pick_key(&mut self, key: Key) -> bool {
        let Some(mut pick) = self.pick else { return false };
        let key = match key {
            Key::Char(' ') => Key::PgDn,
            Key::Char('b') => Key::PgUp,
            key => key,
        };
        match key {
            Key::Char('v') => pick.anchor = if pick.anchor.is_some() { None } else { Some(pick.cursor) },
            Key::Char('n') => pick.cursor = self.change_from(pick.cursor, true).unwrap_or(pick.cursor),
            Key::Char('N') => pick.cursor = self.change_from(pick.cursor, false).unwrap_or(pick.cursor),
            Key::Click(_, y) if y >= 1 && y <= self.rows => match self.line_on_row(self.top + y - 1) {
                Some(line) => pick.cursor = line,
                None => return false,
            },
            _ => {
                let Some(delta) = key.rows(self.rows) else { return false };
                for _ in 0..delta.unsigned_abs().min(self.body.lines().len()) {
                    let Some(next) = self.line_beside(pick.cursor, delta > 0) else { break };
                    pick.cursor = next;
                }
            }
        }
        self.pick = Some(pick);
        self.show_cursor(pick.cursor);
        true
    }

    /// The lines under the pick, as they are shown now.
    pub fn picked(&self) -> Option<Picked> {
        let range = self.pick_range()?;
        let lines = &self.body.lines()[range.clone()];
        let numbers = |removed: bool| {
            span(lines.iter().filter(|line| (line.kind == LineKind::Del) == removed).map(|line| line.number))
        };
        Some(Picked {
            at: *range.start(),
            len: lines.len(),
            lines: lines.iter().take(QUOTED).map(quoted).collect(),
            kept: numbers(false),
            removed: numbers(true),
        })
    }

    /// Whether the lines shown from `at` are still `lines`, word for word.
    pub fn holds(&self, at: usize, lines: &[PickedLine]) -> bool {
        self.body.lines().get(at..at + lines.len()).is_some_and(|now| now.iter().map(quoted).eq(lines.iter().cloned()))
    }

    /// Marks the lines that notes were written about. Sorted ranges of the lines shown.
    pub fn mark_noted(&mut self, noted: Vec<RangeInclusive<usize>>) {
        self.noted = noted;
    }

    /// How many notes wait to be copied, for the hints to say, or `None` while there are no notes.
    pub fn count_notes(&mut self, waiting: Option<usize>) {
        self.waiting_notes = waiting;
    }

    /// Says `text` in the hints for a while, as the reader says where a jump landed.
    pub fn tell(&mut self, text: &'static str) {
        self.say(text, std::time::Instant::now());
    }

    fn pick_range(&self) -> Option<RangeInclusive<usize>> {
        let pick = self.pick?;
        let other = pick.anchor.unwrap_or(pick.cursor);
        Some(pick.cursor.min(other)..=pick.cursor.max(other))
    }

    pub(super) fn pick_cursor(&self) -> Option<usize> {
        self.pick.map(|pick| pick.cursor)
    }

    fn line_on_row(&self, row: usize) -> Option<usize> {
        let folds = self.folds();
        if row >= folds.row_count(self.body.lines().len()) {
            return None;
        }
        match folds.rows(row..row + 1).next()? {
            Row::Line(line) => Some(line),
            Row::Fold(_) => None,
        }
    }

    /// The nearest line shown below or above `line`, passing over folded stretches.
    fn line_beside(&self, line: usize, below: bool) -> Option<usize> {
        let folds = self.folds();
        let total = folds.row_count(self.body.lines().len());
        let mut row = folds.row_at(line);
        loop {
            row = if below { row + 1 } else { row.checked_sub(1)? };
            if row >= total {
                return None;
            }
            if let Some(line) = self.line_on_row(row) {
                return Some(line);
            }
        }
    }

    /// The start of the next or previous change after `line`, going round at the ends.
    fn change_from(&self, line: usize, next: bool) -> Option<usize> {
        let Body::Text(text) = &self.body else { return None };
        let after = text.changes.partition_point(|&start| start <= line);
        let before = text.changes.partition_point(|&start| start < line);
        if next {
            text.changes.get(after).or(text.changes.first()).copied()
        } else {
            before.checked_sub(1).map(|i| text.changes[i]).or(text.changes.last().copied())
        }
    }

    pub(super) fn show_cursor(&mut self, line: usize) {
        let row = self.folds().row_at(line);
        let rows = self.rows.max(1);
        let top = if row < self.top {
            row
        } else if row >= self.top + rows {
            row + 1 - rows
        } else {
            return;
        };
        self.top = top;
        self.review.moved();
        self.clear_change();
    }
}

/// A line being drawn, and whether its row has a gutter.
pub(super) struct Shown {
    pub(super) line: usize,
    pub(super) gutter: bool,
}

/// Lays the pick and the marks of earlier notes over a line just drawn on row `y`.
/// The marks go in the gutter's first column, so a pane too narrow for a gutter shows only the bar.
pub(super) fn draw_marks(pick: Option<Pick>, noted: &[RangeInclusive<usize>], s: &mut Screen, y: usize, line: Shown) {
    let Shown { line, gutter } = line;
    let before = noted.partition_point(|range| *range.end() < line);
    if gutter && noted.get(before).is_some_and(|range| range.contains(&line)) {
        s.put(0, y, 1, "•", Style::fg(YELLOW).bold());
    }
    let Some(pick) = pick else { return };
    let other = pick.anchor.unwrap_or(pick.cursor);
    if (pick.cursor.min(other)..=pick.cursor.max(other)).contains(&line) {
        s.wash(0, y, s.w.saturating_sub(1), Wash::Bar);
    }
    if gutter && line == pick.cursor {
        s.put(0, y, 1, "›", Style::PLAIN.bold());
        s.wash(0, y, 1, Wash::Bar);
    }
}
