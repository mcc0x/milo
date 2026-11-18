//! Lays out and draws the project heading and file tree or strip.

use std::time::Instant;

use crate::git::{Kind, PendingFile};
use crate::help;
use crate::icons;
use crate::screen::{self, Screen, Style, Wash};
use crate::tree::Node;
use crate::ui::{self, FAINT};
use crate::width::str_width;

use super::{LIVE, Outline, PULSE};

/// The shortest pane with room for the project heading.
const HEADING_ROWS: usize = 4;
/// At or below this height a git project shows as a strip of pending files.
const STRIP_ROWS: usize = 7;
/// From this height the last row can show the keys.
const HINT_ROWS: usize = 12;
/// From this outline width each file also says what happened to it.
const WIDE_COLS: usize = 52;

#[derive(Default)]
pub(super) struct Layout {
    pub(super) strip: bool,
    width: usize,
    body_top: usize,
    pub(super) body_rows: usize,
    heading: bool,
    hint: bool,
    wide: bool,
}

impl Layout {
    /// Last column in a strip containing `files` pending files.
    pub(super) fn last_column(&self, files: usize) -> usize {
        files.div_ceil(self.body_rows).saturating_sub(1)
    }

    /// Which row of the visible tree, if any, is under this screen cell.
    pub(super) fn row_at(&self, x: usize, y: usize) -> Option<usize> {
        if x >= self.width || y >= self.body_top + self.body_rows {
            return None;
        }
        y.checked_sub(self.body_top)
    }
}

fn arrange(w: usize, h: usize, git: bool) -> Layout {
    if w < 12 || h == 0 {
        return Layout::default();
    }
    let strip = git && h <= STRIP_ROWS;
    let heading = h >= HEADING_ROWS;
    let hint = h >= HINT_ROWS;
    let body_top = usize::from(heading);
    let body_rows = h - body_top - usize::from(hint);
    Layout { strip, width: w, body_top, body_rows, heading, hint, wide: w >= WIDE_COLS }
}

/// Rows to keep between the cursor and the edge of the tree, when possible.
pub(super) fn margin(rows: usize) -> usize {
    2.min(rows.saturating_sub(1) / 2)
}

impl Outline {
    pub(super) fn draw_outline(&mut self, s: &mut Screen) {
        s.clear();
        let now = Instant::now();
        self.expire(now);
        let uncommitted = self.pending.partition_point(|file| !file.unpushed);
        let layout = arrange(s.w, s.h, self.repo.git);
        if layout.width == 0 {
            self.painted = layout;
            return;
        }
        if layout.heading {
            self.draw_heading(s, layout.width, uncommitted);
        }
        if layout.strip {
            self.draw_strip(s, &layout);
        } else {
            self.draw_tree(s, &layout, now);
        }
        if let Some((note, _)) = &self.note {
            let y = s.h - 1;
            s.clear_row(y);
            s.put_fit(1, y, s.w - 2, note, Style::fg(screen::YELLOW));
        } else if layout.hint && self.attention.show_controls() {
            self.draw_hint(s);
        }
        self.painted = layout;
    }

    /// Names the project and branch beside the work that is waiting, if it fits.
    fn draw_heading(&self, s: &mut Screen, width: usize, uncommitted: usize) {
        let branch = self.repo.branch();
        let name_width = ui::project_heading_width(&self.repo.name, branch);
        let summary_width = if self.repo.git { self.draw_summary(s, width, name_width, uncommitted) } else { 0 };
        let room = width.saturating_sub(summary_width + 3);
        let x = 1 + ui::section_heading(s, 0, room, &self.repo.name);
        s.put_fit(x + 2, 0, room.saturating_sub(x + 1), branch, Style::fg(screen::MAGENTA));
    }

    fn draw_summary(&self, s: &mut Screen, width: usize, name_width: usize, uncommitted: usize) -> usize {
        let count = |n: usize| if n == 0 { String::new() } else { n.to_string() };
        let (commit, push) = (count(uncommitted), count(self.pending.len() - uncommitted));
        let long = self.summary_parts(&commit, &push, Form::Long);
        let long_width = text_width(&long);
        let (parts, used) = if name_width + long_width <= width {
            (long, long_width)
        } else {
            let compact = self.summary_parts(&commit, &push, Form::Compact);
            let compact_width = text_width(&compact);
            (compact, compact_width)
        };
        if used + 12 > width {
            return 0;
        }
        let mut end = width - 1;
        for (text, style) in parts.into_iter().rev() {
            if !text.is_empty() {
                end -= s.put_right(end, 0, text, style);
            }
        }
        used
    }

    fn summary_parts<'a>(&self, commit: &'a str, push: &'a str, form: Form) -> Summary<'a> {
        let mut parts = [("", Style::PLAIN); 5];
        if !self.repo.ok() {
            let text = match form {
                Form::Long => "git did not answer",
                Form::Compact => "! git",
            };
            parts[0] = (text, ui::DEL);
        } else if commit.is_empty() && push.is_empty() {
            let remote = self.repo.remote().is_some();
            let text = match (form, remote) {
                (Form::Long, true) => "✓ all committed and pushed",
                (Form::Long, false) => "✓ all committed",
                (Form::Compact, _) => "✓ clean",
            };
            parts[0] = (text, Style::fg(screen::GREEN));
            if matches!((form, remote), (Form::Long, false)) {
                parts[1] = (" · no remote", FAINT);
            }
        } else {
            parts = pending_parts(commit, push, form);
        }
        parts
    }

    /// Words the keys for what they would do, with shorter hints in narrow
    /// panes. On screen a pending file is simply one that changed.
    fn draw_hint(&self, s: &mut Screen) {
        let folders = if self.folds.all() { "e undo open all" } else { "e open all" };
        if self.repo.git {
            let filter = if self.pending_only { "c all files" } else { "c changed only" };
            let first = if self.pending.is_empty() { "Enter open" } else { "n next change" };
            help::draw_hint(s, first, &[folders, filter, "q quit"], "? help");
        } else {
            help::draw_hint(s, "Enter open", &[folders, "q quit"], "? help");
        }
    }

    fn draw_tree(&mut self, s: &mut Screen, layout: &Layout, now: Instant) {
        let (y0, rows) = (layout.body_top, layout.body_rows);
        let len = self.tree.rows.len();
        if len == 0 {
            let text = if self.pending_only { "Nothing to commit or push." } else { "No files." };
            s.put_fit(1, y0, layout.width - 2, text, FAINT);
            return;
        }
        self.keep_in_view(rows);
        let columns = self.columns(layout);
        for (y, i) in (y0..).zip(self.top..len.min(self.top + rows)) {
            self.draw_row(s, y, i, &columns, now);
        }
        ui::thumb(s, layout.width - 1, y0, rows, len, self.top);
    }

    fn keep_in_view(&mut self, rows: usize) {
        // A click keeps the rows still under the pointer for a second click.
        let margin = if self.by_click { 0 } else { margin(rows) };
        if self.sel < self.top + margin {
            self.top = self.sel.saturating_sub(margin);
        } else if self.sel + margin >= self.top + rows {
            self.top = self.sel + margin + 1 - rows;
        }
        self.top = self.top.min(self.tree.rows.len().saturating_sub(rows));
    }

    fn columns(&self, layout: &Layout) -> Columns {
        let details = layout.wide.then(|| {
            let (mut word, mut size) = (0, 0);
            for row in &self.tree.rows {
                let node = self.tree.node(row.node);
                if !node.dir {
                    word = word.max(ui::kind_word(node.kind).len());
                }
                size = size.max(ui::size_width(node.add, node.del));
            }
            ChangeColumns { word, size }
        });
        Columns { width: layout.width, details }
    }

    fn draw_row(&self, s: &mut Screen, y: usize, i: usize, columns: &Columns, now: Instant) {
        let row = self.tree.rows[i];
        let node = self.tree.node(row.node);
        let open = node.dir && self.folds.is_open(&node.path);
        let right = columns.draw_details(s, y, node);
        let indent = (usize::from(row.depth) * 2).min(columns.width / 3);
        for guide in (0..indent).step_by(2) {
            s.put(1 + guide, y, 1, "│", FAINT);
        }
        self.draw_name(s, y, 1 + indent..right, node, open);
        self.draw_mark(s, y, node, open, now);
        if i == self.sel && self.attention.show_controls() {
            s.wash(0, y, columns.width - 1, Wash::Bar);
        }
    }

    fn draw_name(&self, s: &mut Screen, y: usize, columns: std::ops::Range<usize>, node: &Node, open: bool) {
        let (mut x, right) = (columns.start, columns.end);
        let name = node.name();
        let icon = if node.dir { icons::dir(open) } else { icons::file(name) };
        x += ui::icon(s, x, y, right.saturating_sub(x), icon);
        let colour = ui::kind_style(node.kind);
        let style = if node.uncommitted {
            colour.bold()
        } else if node.kind == Kind::None && !self.pending.is_empty() {
            colour.faint()
        } else {
            colour
        };
        let room = right.saturating_sub(x);
        if node.dir {
            x += s.put_fit(x, y, room, name, style);
            if !open && node.pending > 0 {
                s.put(x + 1, y, right.saturating_sub(x + 1), &node.pending.to_string(), FAINT);
            }
        } else {
            s.put_middle(x, y, room, name, style);
        }
    }

    /// A closed folder carries the most recent mark among the files it hides.
    fn draw_mark(&self, s: &mut Screen, y: usize, node: &Node, open: bool, now: Instant) {
        let at = if node.dir && !open {
            self.file_marks.iter().filter(|(p, _)| under(p, &node.path)).map(|(_, at)| *at).max()
        } else {
            self.file_marks.get(&node.path).copied()
        };
        let Some(at) = at else { return };
        let age = now.saturating_duration_since(at);
        let live = age < LIVE;
        let glyph = match (live, age.as_millis() / PULSE.as_millis() % 2) {
            (true, 0) => "●",
            (true, _) => "○",
            (false, _) => "•",
        };
        let colour = ui::kind_style(node.kind);
        s.put(0, y, 1, glyph, if live { colour.bold() } else { colour });
    }

    /// Shows pending files, most recent first, filling columns top to bottom.
    fn draw_strip(&mut self, s: &mut Screen, layout: &Layout) {
        let (w, y0, rows) = (layout.width, layout.body_top, layout.body_rows);
        if self.recent.is_empty() {
            if !layout.heading {
                s.put_fit(1, 0, w - 2, "Nothing to commit or push.", FAINT);
            }
            return;
        }
        let first = self.strip_col.min(layout.last_column(self.recent.len()));
        let mut x = 0;
        let mut shown = first * rows;
        let files = &self.pending;
        for column in self.recent.chunks(rows).skip(first) {
            let width = strip_width(column.iter().map(|&i| &files[i])).min(w);
            if x > 0 && x + width > w {
                break;
            }
            for (row, &i) in column.iter().enumerate() {
                ui::file_name(s, x + 1, y0 + row, width - 2, &files[i]);
            }
            x += width;
            shown += column.len();
        }
        if first > 0 {
            s.put(0, y0, 1, "‹", FAINT);
        }
        let after = self.recent.len() - shown;
        if after > 0 {
            s.put_right(w - 1, y0 + rows - 1, &format!("  +{after} more ›"), FAINT);
        }
        self.strip_col = first;
    }
}

#[derive(Clone, Copy)]
enum Form {
    Long,
    Compact,
}

type Summary<'a> = [(&'a str, Style); 5];

fn text_width(parts: &Summary<'_>) -> usize {
    parts.iter().map(|(text, _)| str_width(text)).sum()
}

fn pending_parts<'a>(commit: &'a str, push: &'a str, form: Form) -> Summary<'a> {
    let (to_commit, to_push) = (!commit.is_empty(), !push.is_empty());
    let bold = Style::PLAIN.bold();
    let plain = Style::PLAIN;
    let when = |shown, text| if shown { text } else { "" };
    match form {
        Form::Long => [
            (commit, bold),
            (when(to_commit, " to commit"), bold),
            (when(to_commit && to_push, " · "), FAINT),
            (push, plain),
            (when(to_push, " to push"), plain),
        ],
        Form::Compact => [
            (when(to_commit, "○"), bold),
            (commit, bold),
            (when(to_commit && to_push, " "), FAINT),
            (when(to_push, "↑"), plain),
            (push, plain),
        ],
    }
}

struct Columns {
    width: usize,
    details: Option<ChangeColumns>,
}

struct ChangeColumns {
    word: usize,
    size: usize,
}

impl Columns {
    /// Draws the change and its size when there is room, returning where the name ends.
    fn draw_details(&self, s: &mut Screen, y: usize, node: &Node) -> usize {
        let right = self.width - 1; // The last column belongs to the scroll thumb.
        let Some(details) = &self.details else { return right };
        if node.kind == Kind::None {
            return right;
        }
        ui::size_right(s, right, y, node.add, node.del);
        let word_at = right - details.size - usize::from(details.size > 0) - details.word;
        if !node.dir {
            s.put(word_at, y, details.word, ui::kind_word(node.kind), ui::kind_colour(node.kind));
        }
        word_at - 1
    }
}

fn under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// Each strip column is only as wide as its own names need.
fn strip_width<'a>(files: impl Iterator<Item = &'a PendingFile>) -> usize {
    let longest =
        files.map(|file| str_width(file.path.rsplit_once('/').map_or(&file.path, |(_, name)| name))).max().unwrap_or(0);
    (longest + 12).clamp(16, 34)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shortest_panes_spend_their_rows_on_files() {
        let short = arrange(40, 3, true);
        assert!(!short.heading);
        assert_eq!((short.body_top, short.body_rows), (0, 3));
        let headed = arrange(40, 4, true);
        assert!(headed.heading);
        assert_eq!((headed.body_top, headed.body_rows), (1, 3));
        assert!(arrange(40, 7, true).strip);
        assert!(!arrange(40, 8, true).strip);
        assert!(!arrange(40, 7, false).strip);
    }

    #[test]
    fn the_twelfth_row_can_show_a_hint() {
        let before = arrange(52, 11, true);
        let after = arrange(52, 12, true);
        assert!(!before.hint && after.hint);
        assert_eq!(before.body_rows, 10);
        assert_eq!(after.body_rows, 10);
    }

    #[test]
    fn extra_columns_require_enough_width() {
        assert!(!arrange(51, 8, true).wide);
        assert!(arrange(52, 8, true).wide);
    }
}
