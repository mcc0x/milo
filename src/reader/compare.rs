//! A read-only, in-memory comparison between one save attempt's disk snapshot and the draft.

use std::ops::Range;

use super::super::disk::Snapshot;
use super::super::draw::LinePainter;
use crate::diff::{Line, LineKind};
use crate::keys::Key;
use crate::screen::Screen;
use crate::syntax::State;
use crate::ui::{self, ADD, DEL};

/// Store byte offsets, not copies of the lines. File size is limited by the editor.
struct Rows(Vec<u32>);

impl Rows {
    fn new(text: &str) -> Self {
        let mut end = 0;
        Self(
            text.split_inclusive('\n')
                .map(|line| {
                    end += u32::try_from(line.len()).expect("editor text is bounded");
                    end
                })
                .collect(),
        )
    }

    fn at<'a>(&self, text: &'a str, i: usize) -> &'a str {
        let start = if i == 0 { 0 } else { self.0[i - 1] as usize };
        &text[start..self.0[i] as usize]
    }
}

struct Run {
    /// Offset in the combined view, including lines removed from the disk snapshot.
    first: usize,
    rows: Range<usize>,
    kind: LineKind,
}

pub(super) struct Comparison {
    disk: Snapshot,
    draft: String,
    disk_rows: Rows,
    draft_rows: Rows,
    runs: Vec<Run>,
    len: usize,
    top: usize,
    rows: usize,
    pan: usize,
    pan_limit: usize,
    shown: Vec<Line>,
    shown_top: usize,
    widest: usize,
}

impl Comparison {
    pub(super) fn new(disk: Snapshot, draft: String) -> Self {
        let on_disk = disk.text();
        let disk_rows = Rows::new(on_disk);
        let draft_rows = Rows::new(&draft);
        let (n, m) = (disk_rows.0.len(), draft_rows.0.len());
        let pair = |x, y| (disk_rows.at(on_disk, x), draft_rows.at(&draft, y));
        let equal = |x, y| {
            let (a, b) = pair(x, y);
            a == b
        };
        let prefix = (0..n.min(m)).take_while(|&i| equal(i, i)).count();
        let suffix = (0..n.min(m) - prefix).take_while(|&i| equal(n - 1 - i, m - 1 - i)).count();
        let (old_end, new_end) = (n - suffix, m - suffix);
        let middle = matches(old_end - prefix, new_end - prefix, |x, y| pair(x + prefix, y + prefix));
        let mut runs = Vec::new();
        let mut len = 0;
        let mut push = |kind, rows: Range<usize>| {
            if !rows.is_empty() {
                let first = len;
                len += rows.len();
                runs.push(Run { first, rows, kind });
            }
        };
        push(LineKind::Same, 0..prefix);
        let (mut x, mut y) = (prefix, prefix);
        for (old, new, same) in middle.into_iter().rev() {
            let (old, new) = (prefix + old, prefix + new);
            push(LineKind::Del, x..old);
            push(LineKind::Add, y..new);
            push(LineKind::Same, new..new + same);
            (x, y) = (old + same, new + same);
        }
        push(LineKind::Del, x..old_end);
        push(LineKind::Add, y..new_end);
        push(LineKind::Same, new_end..m);
        let top = prefix.saturating_sub(3);
        Self {
            disk,
            draft,
            disk_rows,
            draft_rows,
            runs,
            len,
            top,
            rows: 0,
            pan: 0,
            pan_limit: 0,
            shown: Vec::new(),
            shown_top: 0,
            widest: 0,
        }
    }

    pub(super) fn into_snapshot(self) -> Snapshot {
        self.disk
    }

    pub(super) fn key(&mut self, key: Key) -> bool {
        let before = (self.top, self.pan);
        match key {
            Key::Left | Key::Char('h') => self.pan = self.pan.saturating_sub(8),
            Key::Right | Key::Char('l') => self.pan = (self.pan + 8).min(self.pan_limit),
            _ => {
                let key = match key {
                    Key::Char(' ') => Key::PgDn,
                    Key::Char('b') => Key::PgUp,
                    _ => key,
                };
                if let Some(rows) = key.rows(self.rows) {
                    self.top = self.top.saturating_add_signed(rows).min(self.last_top());
                }
            }
        }
        before != (self.top, self.pan)
    }

    fn last_top(&self) -> usize {
        self.len.saturating_sub(self.rows.max(1))
    }

    pub(super) fn legend(s: &mut Screen) -> usize {
        let mut right = s.w.saturating_sub(1);
        right -= s.put_right(right, 0, "+ Draft", ADD.bold());
        right = right.saturating_sub(2);
        right - s.put_right(right, 0, "- Disk", DEL)
    }

    fn show(&mut self) {
        let count = self.rows.min(self.len - self.top);
        if self.shown_top == self.top && self.shown.len() == count {
            return;
        }
        self.shown_top = self.top;
        self.shown.clear();
        let first = self.runs.partition_point(|run| run.first <= self.top).saturating_sub(1);
        for run in &self.runs[first..] {
            let from = run.rows.start + self.top.saturating_sub(run.first);
            let to = run.rows.end.min(from + count - self.shown.len());
            let (source, offsets) = if run.kind == LineKind::Del {
                (self.disk.text(), &self.disk_rows)
            } else {
                (&*self.draft, &self.draft_rows)
            };
            self.shown.extend((from..to).map(|row| display_line(run.kind, row, offsets.at(source, row))));
            if self.shown.len() == count {
                break;
            }
        }
        self.widest = self.shown.iter().map(Line::display_width).max().unwrap_or(0);
    }

    pub(super) fn draw(&mut self, s: &mut Screen, rows: usize) {
        self.rows = rows;
        self.top = self.top.min(self.last_top());
        if rows == 0 {
            return;
        }
        self.show();
        let digits = self.draft_rows.0.len().max(1).ilog10() as usize + 1;
        let x = if s.w >= digits + 7 {
            digits + 4
        } else if s.w >= 6 {
            3
        } else {
            0
        };
        let width = s.w - 1 - x;
        self.pan_limit = self.widest.saturating_sub(width);
        self.pan = self.pan.min(self.pan_limit);
        let mut painter = LinePainter { x, width, pan: self.pan, lang: None, spans: &mut Vec::new() };
        for (i, line) in self.shown.iter().enumerate() {
            painter.draw_line(s, i + 1, line, &State::Code);
        }
        ui::thumb(s, s.w - 1, 1, rows, self.len, self.top);
    }

    pub(super) fn hint(&self, s: &mut Screen) {
        let first = if self.runs.iter().all(|run| run.kind == LineKind::Same) {
            "No text differences"
        } else {
            "↑↓ scroll"
        };
        crate::help::draw_hint(s, first, &["←→ pan"], "Esc back");
    }
}

fn display_line(kind: LineKind, row: usize, raw: &str) -> Line {
    let (text, ending) = if let Some(text) = raw.strip_suffix("\r\n") {
        (text, " [CRLF]")
    } else if let Some(text) = raw.strip_suffix('\n') {
        (text, "")
    } else {
        (raw, " [no LF]")
    };
    let bom = (row == 0).then(|| text.strip_prefix('\u{feff}')).flatten();
    let mut line = Line::new(kind, u32::try_from(row + 1).expect("editor text is bounded"), bom.unwrap_or(text));
    if kind != LineKind::Same {
        if bom.is_some() {
            line.annotation.push_str(" [BOM]");
        }
        line.annotation.push_str(ending);
    }
    line
}

/// The equal spans of a shortest edit, in reverse order. Bounded by both edit
/// count and bytes compared; beyond that, colour the unmatched middle as a
/// replacement rather than stalling on two very different files.
fn matches<'a>(n: usize, m: usize, pair: impl Fn(usize, usize) -> (&'a str, &'a str)) -> Vec<(usize, usize, usize)> {
    const MAX_EDITS: usize = 128;
    const UNREACHED: usize = usize::MAX;
    if n == 0 || m == 0 || n.abs_diff(m) > MAX_EDITS {
        return Vec::new();
    }
    let mut budget = 16_usize << 20;
    let mut trace = vec![0]; // The common prefix is already set aside.
    for d in 1..=MAX_EDITS {
        let row = d * (d - 1) / 2;
        for j in 0..=d {
            let Some((start, _)) = step(&trace[row..row + d], j, n, m) else {
                trace.push(UNREACHED);
                continue;
            };
            let (mut x, mut y) = (start, start + d - 2 * j);
            while x < n && y < m {
                let (a, b) = pair(x, y);
                let size = if a.len() == b.len() { a.len() } else { 0 };
                let Some(left) = budget.checked_sub(size + 1) else { return Vec::new() };
                budget = left;
                if a != b {
                    break;
                }
                x += 1;
                y += 1;
            }
            trace.push(x);
            if (x, y) == (n, m) {
                let mut spans = Vec::new();
                for d in (1..=d).rev() {
                    let row = d * (d - 1) / 2;
                    let j = (x + d - y) / 2;
                    let (start, removed) = step(&trace[row..row + d], j, n, m).expect("search followed this path");
                    let other = start + d - 2 * j;
                    if x > start {
                        spans.push((start, other, x - start));
                    }
                    (x, y) = if removed { (start - 1, other) } else { (start, other - 1) };
                }
                return spans;
            }
        }
    }
    Vec::new()
}

/// Start of one Myers diagonal, taking the predecessor that consumed the most old rows.
fn step(previous: &[usize], j: usize, n: usize, m: usize) -> Option<(usize, bool)> {
    let removed = j.checked_sub(1).and_then(|i| previous.get(i)).filter(|&&x| x < n).map(|x| x + 1);
    let added = previous
        .get(j)
        .copied()
        .filter(|&x| x != usize::MAX && (x + previous.len() - 1).checked_sub(2 * j).is_some_and(|y| y < m));
    match (removed, added) {
        (Some(x), Some(y)) if y > x => Some((y, false)),
        (Some(x), _) => Some((x, true)),
        (None, Some(x)) => Some((x, false)),
        (None, None) => None,
    }
}
