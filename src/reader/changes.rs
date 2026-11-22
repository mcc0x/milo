//! A compact view of a patch. Positions in the original lines are not renumbered.

use std::ops::Range;

use super::{Body, Reader};
use crate::pane::After;

const MIN_HIDDEN: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Fold {
    start: usize,
    end: usize,
    row: usize,
}

impl Fold {
    pub(super) fn len(self) -> usize {
        self.end - self.start
    }

    /// Number of original lines skipped after this fold, less its visible row.
    fn skipped(self) -> usize {
        self.end - self.row - 1
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Folds(Vec<Fold>);

pub(super) enum Row {
    Line(usize),
    Fold(Fold),
}

impl Folds {
    pub(super) fn push(&mut self, start: usize, end: usize) {
        if end.saturating_sub(start) >= MIN_HIDDEN {
            let row = start - self.0.last().map_or(0, |fold| fold.skipped());
            self.0.push(Fold { start, end, row });
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn row_count(&self, lines: usize) -> usize {
        lines - self.0.last().map_or(0, |fold| fold.skipped())
    }

    pub(super) fn row_at(&self, line: usize) -> usize {
        let before = self.0.partition_point(|fold| fold.start <= line);
        let Some(fold) = self.0[..before].last() else { return line };
        if line < fold.end { fold.row } else { line - fold.skipped() }
    }

    fn line_before(&self, row: usize, before: usize) -> usize {
        row + self.0[..before].last().map_or(0, |fold| fold.skipped())
    }

    fn line_at(&self, row: usize) -> usize {
        let before = self.0.partition_point(|fold| fold.row < row);
        self.line_before(row, before)
    }

    pub(super) fn rows(&self, rows: Range<usize>) -> impl Iterator<Item = Row> + Clone + '_ {
        let before = self.0.partition_point(|fold| fold.row < rows.start);
        let mut folds = self.0[before..].iter().peekable();
        let mut line = self.line_before(rows.start, before);
        rows.map(move |row| {
            if let Some(fold) = folds.next_if(|fold| fold.row == row) {
                line = fold.end;
                Row::Fold(*fold)
            } else {
                let at = line;
                line += 1;
                Row::Line(at)
            }
        })
    }

    fn visible(&self, top: usize, rows: usize) -> &[Fold] {
        let before = self.0.partition_point(|fold| fold.row < top);
        let after = self.0[before..].partition_point(|fold| fold.row < top + rows);
        &self.0[before..before + after]
    }

    fn expand(&mut self, row: usize) -> bool {
        let Ok(index) = self.0.binary_search_by_key(&row, |fold| fold.row) else { return false };
        let extra = self.0.remove(index).len() - 1;
        for fold in &mut self.0[index..] {
            fold.row += extra;
        }
        true
    }
}

#[derive(Default)]
pub(super) struct Review {
    compact: Option<Folds>,
    /// The exact full-file top, even if its fold cannot scroll to the top in the compact view.
    full_top: Option<usize>,
    focused: Option<usize>,
    /// Clicks address the last drawn body; a key may have moved it before the next draw.
    pub(super) click_width: usize,
}

impl Review {
    pub(super) fn moved(&mut self) {
        self.full_top = None;
        self.focused = None;
        self.click_width = 0;
    }

    pub(super) fn is_compact(&self) -> bool {
        self.compact.is_some()
    }

    pub(super) fn folds(&self) -> &Folds {
        static NONE: Folds = Folds(Vec::new());
        self.compact.as_ref().unwrap_or(&NONE)
    }
}

impl Reader {
    pub(super) fn folds(&self) -> &Folds {
        self.review.folds()
    }

    pub(super) fn can_fold(&self) -> bool {
        self.review.is_compact() || matches!(&self.body, Body::Text(text) if !text.folds.is_empty())
    }

    pub(super) fn source_top(&self) -> usize {
        self.folds().line_at(self.top)
    }

    pub(super) fn set_source_top(&mut self, line: usize) {
        self.review.moved();
        self.top = self.folds().row_at(line).min(self.last_top());
        if self.review.is_compact() {
            self.review.full_top = Some(line);
        }
    }

    pub(super) fn replace_body(&mut self, body: Body) {
        let anchor = if self.review.is_compact() {
            let top = self.review.full_top.unwrap_or_else(|| self.source_top());
            let same_anchor = match (&self.body, &body) {
                // A refresh can publish the same patch. Keep its exact row, including
                // deleted lines that a working-file line number cannot represent.
                (Body::Text(old), Body::Text(new)) if old.folds == new.folds => {
                    old.lines.get(top).zip(new.lines.get(top)).is_some_and(|(old, new)| {
                        old.kind == new.kind && old.number == new.number && old.text == new.text
                    })
                }
                (_, Body::Text(new)) => {
                    self.review.compact = Some(new.folds.clone());
                    false
                }
                _ => {
                    self.review.compact = Some(Folds::default());
                    false
                }
            };
            (!same_anchor).then(|| self.line_number_at(top))
        } else {
            None
        };
        self.body = body;
        if let Some(anchor) = anchor {
            self.set_source_top(self.source_row(anchor));
        }
        self.clear_change();
        if !matches!(self.body, Body::Note(_))
            && let Some(line) = self.pending_editor_line.take()
            && matches!(self.body, Body::Text(_))
        {
            self.show_editor_line(line);
        }
    }

    pub(super) fn toggle_changes(&mut self) -> After {
        if !self.can_fold() {
            return After::Ignored;
        }
        let raw = self.source_top();
        let restore = self.review.full_top;
        self.review.moved();
        let top = if self.review.compact.take().is_some() {
            restore.or_else(|| self.change_top()).unwrap_or(raw)
        } else if let Body::Text(text) = &self.body {
            self.review.compact = Some(text.folds.clone());
            self.review.full_top = Some(raw);
            self.change_top().unwrap_or_else(|| self.folds().row_at(raw))
        } else {
            raw
        };
        self.top = top;
        self.clamp_top();
        After::Stay
    }

    fn change_row(&self) -> Option<usize> {
        let (Body::Text(text), Some(change)) = (&self.body, self.current_change) else { return None };
        Some(self.folds().row_at(*text.changes.get(change)?))
    }

    fn change_top(&self) -> Option<usize> {
        self.change_row().map(|row| row.saturating_sub(self.lead()))
    }

    pub(super) fn clamp_top(&mut self) {
        if self.rows != 0
            && let Some(row) = self.change_row()
        {
            self.top = self.top.clamp(row.saturating_sub(self.rows - 1), row);
        }
        self.top = self.top.min(self.last_top());
    }

    pub(super) fn focused_fold(&self) -> Option<usize> {
        let visible = self.folds().visible(self.top, self.rows);
        if let Some(row) = self.review.focused
            && visible.binary_search_by_key(&row, |fold| fold.row).is_ok()
        {
            return Some(row);
        }
        visible.first().map(|fold| fold.row)
    }

    pub(super) fn next_fold(&mut self) -> After {
        let Some(row) = self.focused_fold() else { return After::Ignored };
        let visible = self.folds().visible(self.top, self.rows);
        let next = visible.partition_point(|fold| fold.row <= row) % visible.len();
        let next = visible[next].row;
        self.review.focused = Some(next);
        if next == row { After::Ignored } else { After::Stay }
    }

    pub(super) fn click_fold(&mut self, x: usize, y: usize) -> After {
        if x >= self.review.click_width || y == 0 || y > self.rows {
            return After::Ignored;
        }
        self.expand_fold(Some(self.top + y - 1))
    }

    pub(super) fn expand_fold(&mut self, row: Option<usize>) -> After {
        let (Some(folds), Some(row)) = (&mut self.review.compact, row) else { return After::Ignored };
        if !folds.expand(row) {
            return After::Ignored;
        }
        self.review.moved();
        // Start the next frame on the same rows; the opened fold itself stays in place.
        if self.change_row().is_some_and(|change| change >= self.top + self.rows) {
            self.clear_change();
        }
        After::Stay
    }
}
