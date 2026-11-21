//! Draws the file, its patch marks and its headings.

use std::ops::Range;
use std::time::Instant;

use super::changes::{Fold, Row};
use super::edit::Editor;
use super::{Body, JumpNotice, Reader, Source};
use crate::diff::{Line, LineKind};
use crate::git::Kind;
use crate::icons;
use crate::screen::{self, Screen, Style, Wash};
use crate::syntax::{Class, Lang, Span, State};
use crate::ui::{self, ADD, DEL, FAINT};
use crate::when;
use crate::width::{byte_at_col, str_width};

/// The byte to draw from and the prefix needed to colour what is shown.
fn text_for_draw(text: &str, pan: usize, width: usize) -> (usize, &str) {
    let skip = byte_at_col(text, pan);
    let rest = &text[skip..];
    let end = if rest.len() <= width { text.len() } else { skip + byte_at_col(rest, width) };
    (skip, &text[..end])
}

/// The colours use the terminal's palette so they follow its theme.
fn class_style(class: Class) -> Style {
    match class {
        Class::Comment => FAINT.italic(),
        Class::Str => Style::fg(screen::GREEN),
        Class::Number => Style::fg(screen::YELLOW),
        Class::Keyword => Style::fg(screen::MAGENTA),
        Class::Type => Style::fg(screen::CYAN),
        Class::Function | Class::Key => Style::fg(screen::BLUE),
    }
}

/// Draws one numbered line of a file with its code colours and change marks.
pub(super) struct LinePainter<'a> {
    pub(super) x: usize,
    pub(super) width: usize,
    pub(super) pan: usize,
    pub(super) lang: Option<&'static Lang>,
    pub(super) spans: &'a mut Vec<Span>,
}

impl LinePainter<'_> {
    pub(super) fn draw_line(&mut self, s: &mut Screen, y: usize, line: &Line, state: &State) {
        let (sign, text_style, margin_style) = match line.kind {
            LineKind::Same => (" ", Style::PLAIN, FAINT),
            LineKind::Add if self.lang.is_some() => ("+", Style::PLAIN, ADD.bold()),
            LineKind::Add => ("+", ADD, ADD),
            LineKind::Del => ("-", DEL, DEL),
        };
        if self.x >= 3 {
            if line.kind != LineKind::Del {
                s.put_number(self.x - 3, y, line.number.into(), margin_style);
            }
            s.put(self.x - 2, y, 1, sign, margin_style);
            if self.pan > 0 {
                s.put(self.x - 1, y, 1, "‹", FAINT);
            }
        }
        let lang = self.lang.filter(|_| line.kind != LineKind::Del);
        let (skip, text) = text_for_draw(&line.text, self.pan, self.width + lang.map_or(0, Lang::lookahead));
        let used = s.put(self.x, y, self.width, &text[skip..], text_style);
        if used > 0 {
            if let Some(lang) = lang {
                lang.scan(text, &mut state.clone(), self.spans);
                self.draw_code(s, y, skip, text, used);
            }
            self.draw_edit(s, y, skip, text, &line.changed, used);
        }
        if self.pan + self.width > line.width
            && let Some(label) = line.annotation.get(self.pan.saturating_sub(line.width)..)
            && !label.is_empty()
        {
            s.put(self.x + used, y, self.width - used, label, margin_style.reverse());
        }
        match line.kind {
            LineKind::Same => {}
            LineKind::Add => s.wash(0, y, s.w - 1, Wash::Added),
            LineKind::Del => s.wash(0, y, s.w - 1, Wash::Removed),
        }
    }

    fn draw_code(&self, s: &mut Screen, y: usize, skip: usize, text: &str, used: usize) {
        // Continue from the previous span so each gap is measured only once.
        let (mut at, mut col) = (skip, 0);
        for &(start, end, class) in &*self.spans {
            if end <= skip {
                continue;
            }
            let start = start.max(skip);
            col += str_width(&text[at..start]);
            if col >= used {
                break;
            }
            let width = str_width(&text[start..end]);
            s.paint(self.x + col, y, width.min(used - col), class_style(class));
            (at, col) = (end, col + width);
        }
    }

    fn draw_edit(&self, s: &mut Screen, y: usize, skip: usize, text: &str, changed: &[Range<usize>], used: usize) {
        // Continue from the previous range, as for code colours.
        let (mut at, mut col) = (skip, 0);
        for range in changed {
            let (start, end) = (range.start.max(at), range.end.min(text.len()));
            if start >= end {
                continue;
            }
            col += str_width(&text[at..start]);
            if col >= used {
                break;
            }
            let width = str_width(&text[start..end]);
            s.emphasise(self.x + col, y, width.min(used - col));
            (at, col) = (end, col + width);
        }
    }
}

impl Reader {
    pub(super) fn draw_file(&mut self, s: &mut Screen, now: Instant) {
        s.clear();
        if let Some(mut editor) = self.editor.take() {
            // The frame before this one said a formatter was about to run; this one shows what it did.
            editor.format_if_announced();
            self.draw_editor(s, &mut editor, now);
            self.editor = Some(editor);
            return;
        }
        if self.change_notice.is_some_and(|(_, until)| now >= until) {
            self.change_notice = None;
        }
        let hint = s.h >= 3;
        self.rows = s.h.saturating_sub(1 + usize::from(hint));
        self.columns = 0;
        self.pan_limit = 0;
        self.review.click_width = 0;
        if s.w == 0 || s.h == 0 {
            return;
        }
        let notice = self.change_notice.map(|(notice, _)| notice);
        let right = match notice {
            Some(notice) if !hint => draw_change_notice(s, 0, notice),
            _ => self.draw_details(s),
        };
        self.draw_heading(s, right);
        if self.rows > 0 {
            self.draw_body(s);
        }
        if hint {
            if let Some(notice) = notice {
                draw_change_notice(s, s.h - 1, notice);
            } else {
                self.draw_hint(s);
            }
        }
    }

    /// The draft takes the place of the file, under the same heading.
    fn draw_editor(&mut self, s: &mut Screen, editor: &mut Editor, now: Instant) {
        if s.w == 0 || s.h == 0 {
            return;
        }
        let hint = s.h >= 3;
        let right = editor.draw_state(s);
        self.draw_heading(s, right);
        editor.draw_body(s, s.h - 1 - usize::from(hint));
        // With no row for hints, a question or message takes the heading's row.
        let y = if hint { s.h - 1 } else { 0 };
        if !editor.draw_message(s, y, now) && hint {
            editor.draw_hint(s);
        }
    }

    fn draw_body(&mut self, s: &mut Screen) {
        if std::mem::take(&mut self.opening) {
            self.reveal_first_change();
        }
        self.clamp_top();
        let focused = self.focused_fold();
        let note = match &self.body {
            Body::Note(note) => Some(*note),
            Body::Picture(picture) => match s.pictures() {
                Some(true) if s.w > 2 => {
                    let room = s.w - 2;
                    let (cols, rows) = picture.fit(s.cell_size(), room, self.rows);
                    s.show_picture(picture, 1 + (room - cols) / 2, 1 + (self.rows - rows) / 2, cols, rows);
                    None
                }
                Some(false) => Some("This terminal cannot show pictures."),
                _ => None,
            },
            Body::Text(text) => {
                let folds = self.review.folds();
                let total = folds.row_count(text.lines.len());
                let shown = folds.rows(self.top..(self.top + self.rows).min(total));
                let gutter = text.digits + 4;
                let x = if s.w >= gutter + 3 { gutter } else { 0 };
                let width = s.w - 1 - x;
                self.columns = width;
                self.pan_limit = shown
                    .clone()
                    .filter_map(|row| match row {
                        Row::Line(line) => Some(text.lines[line].display_width()),
                        Row::Fold(_) => None,
                    })
                    .max()
                    .unwrap_or(0)
                    .saturating_sub(width);
                self.pan = self.pan.min(self.pan_limit);
                let mut painter = LinePainter { x, width, pan: self.pan, lang: text.lang, spans: &mut self.spans };
                let mut starts = text.starts.as_slice();
                let mut state = &State::Code;
                let mut skipped = true;
                for (i, row) in shown.enumerate() {
                    match row {
                        Row::Line(line) => {
                            if starts.first().is_some_and(|(at, _)| *at <= line) {
                                let used = if skipped { starts.partition_point(|(at, _)| *at <= line) } else { 1 };
                                state = &starts[used - 1].1;
                                starts = &starts[used..];
                            }
                            painter.draw_line(s, i + 1, &text.lines[line], state);
                            skipped = false;
                        }
                        Row::Fold(fold) => {
                            draw_fold(s, i + 1, x, fold, focused == Some(self.top + i));
                            skipped = true;
                        }
                    }
                }
                if !folds.is_empty() {
                    self.review.click_width = s.w - 1;
                }
                ui::thumb(s, s.w - 1, 1, self.rows, total, self.top);
                return;
            }
        };
        self.pan = 0;
        if let Some(note) = note {
            draw_note(s, self.rows, note);
        }
    }

    fn draw_hint(&self, s: &mut Screen) {
        if self.can_fold() {
            let action = if self.review.is_compact() { "c full file" } else { "c changes only" };
            let middle = &["e edit", "↑↓ scroll", "q close", "n N changes"][usize::from(self.read_only.is_some())..];
            crate::help::draw_hint(s, action, middle, "? help");
            return;
        }
        let changes = matches!(&self.body, Body::Text(text)
            if text.added + text.removed > 0 && text.lines.len() > (text.added + text.removed) as usize);
        let committed = matches!(self.source, Source::Committed { .. });
        let still = !matches!(self.body, Body::Text(_));
        let action = match (changes, committed) {
            (true, true) => "n N changes",
            (true, false) => "n next change",
            (false, _) => "",
        };
        let middle: &[_] = match (still, self.read_only.is_none()) {
            (true, true) => &["e edit", "q close"],
            (true, false) => &["q close"],
            (false, true) => &["↑↓ scroll", "e edit", "q close"],
            (false, false) => &["↑↓ scroll", "q close"],
        };
        crate::help::draw_hint(s, action, middle, "? help");
    }

    /// Keeps the file name ahead of its directory so the name gets the space
    /// first. `right` is where what is already drawn at the right edge starts.
    fn draw_heading(&self, s: &mut Screen, right: usize) {
        let (dir, name) = self.path.rsplit_once('/').unwrap_or(("", &self.path));
        let room = if right == s.w - 1 { right } else { right.saturating_sub(2) };
        let mut x = 1;
        if room > 10 {
            x += ui::icon(s, x, 0, room - x, icons::file(self.icons, name));
        }
        x += s.put_middle(x, 0, room.saturating_sub(x), name, Style::PLAIN.bold());
        s.put_fit(x + 2, 0, room.saturating_sub(x + 2), dir, FAINT);
    }

    fn draw_details(&self, s: &mut Screen) -> usize {
        let mut right = s.w - 1;
        const NAME_ROOM: usize = 10;
        if let Source::Committed { short_id, time } = &self.source {
            let age = when::short_age(when::now().saturating_sub(*time));
            let ago = if age.count.is_some() { " ago" } else { "" };
            let bare = "commit ";
            let space = " · ";
            let room = right.saturating_sub(NAME_ROOM);
            let width = bare.len() + short_id.len();
            if width + str_width(space) + age.width() + ago.len() <= room {
                right -= s.put_right(right, 0, ago, FAINT);
                right -= ui::age_right(s, right, 0, age);
                right -= s.put_right(right, 0, space, FAINT);
            }
            if width <= room {
                right -= s.put_right(right, 0, short_id, FAINT);
                right -= s.put_right(right, 0, bare, FAINT);
            }
        } else if let Body::Picture(picture) = &self.body {
            let size = format!("{}×{}", picture.width, picture.height);
            if str_width(&size) <= right.saturating_sub(NAME_ROOM) {
                right -= s.put_right(right, 0, &size, FAINT);
            }
        }
        if matches!(self.source, Source::Working(_)) && self.kind != Kind::None {
            let word = ui::kind_word(self.kind);
            let status = word.len() + 2;
            if status <= right.saturating_sub(NAME_ROOM) {
                if let Body::Text(text) = &self.body
                    && ui::size_width(text.added, text.removed) + status <= right.saturating_sub(NAME_ROOM)
                {
                    right -= ui::size_right(s, right, 0, text.added, text.removed);
                }
                right -= 2;
                right -= s.put_right(right, 0, word, ui::kind_colour(self.kind));
            }
        }
        right
    }
}

fn draw_fold(s: &mut Screen, y: usize, gutter: usize, fold: Fold, selected: bool) {
    let number = fold.len();
    let digits = number.ilog10() as usize + 1;
    let end = s.w - 1;
    let label = " unchanged lines";
    let full = 2 + digits + label.len();
    let x = if gutter != 0 && end.saturating_sub(gutter) >= full { gutter } else { usize::from(end > 1) };
    let room = end - x;
    let style = if selected { Style::PLAIN.reverse() } else { FAINT };
    if room < 2 + digits {
        s.put(x, y, room, "…", style);
        return;
    }
    let mut at = x + s.put(x, y, room, "… ", style) + digits;
    s.put_number(at, y, number as u64, style);
    let suffix = if room >= full { label } else { " hidden" };
    if suffix.len() <= end - at {
        at += s.put(at, y, suffix.len(), suffix, style);
    }
    const ENTER: &str = " · Enter";
    if selected && str_width(ENTER) <= end - at {
        s.put(at, y, end - at, ENTER, FAINT);
    }
}

/// Uses the right edge of the heading or the left edge of the hint.
fn draw_change_notice(s: &mut Screen, y: usize, notice: JumpNotice) -> usize {
    const PREFIX: &str = "Change ";
    const OF: &str = " of ";
    let number_width = |n: usize| n.ilog10() as usize + 1;
    let (full, numbers, (lead, text)) = match notice {
        JumpNotice::At { number, total } => {
            let first = number_width(number);
            let counts = first + OF.len() + number_width(total);
            (PREFIX.len() + counts, Some((number, total, first, counts)), ("", ""))
        }
        JumpNotice::NoChanges => ("No changes".len(), None, ("", "No changes")),
        JumpNotice::Said { lead, text } => (lead.len() + text.len(), None, (lead, text)),
    };
    let margin = usize::from(s.w >= full + 2);
    let room = s.w - margin * 2;
    let width = full.min(room);
    let x = if y == 0 { s.w - margin - width } else { margin };
    let st = Style::PLAIN;
    match numbers {
        Some((number, total, first, counts)) if counts <= width => {
            let start = if full <= room { x + s.put(x, y, width, PREFIX, st) } else { x };
            s.put_number(start + first, y, number as u64, st);
            s.put(start + first, y, OF.len(), OF, st);
            s.put_number(start + counts, y, total as u64, st);
        }
        Some(_) => {
            s.put_fit(x, y, width, "Change…", st);
        }
        None => {
            // Where both do not fit, the words that differ say more than the words that lead them.
            let led = if full <= room { s.put(x, y, width, lead, st) } else { 0 };
            s.put_fit(x + led, y, width - led, text, st);
        }
    }
    if y != 0 {
        const HELP: &str = " · ? help";
        let help_width = str_width(HELP);
        if width + help_width <= room {
            s.put(x + width, y, help_width, HELP, FAINT);
        }
    }
    x
}

fn draw_note(s: &mut Screen, rows: usize, mut note: &str) {
    let x = if s.w >= 8 { 2 } else { 0 };
    let room = s.w - x - 1;
    if room == 0 {
        return;
    }
    for y in rows.min(2)..=rows {
        let end = byte_at_col(note, room);
        if end == note.len() || y == rows {
            s.put_fit(x, y, room, note, FAINT);
            break;
        }
        let end = if note[end..].starts_with(' ') {
            end
        } else {
            note[..end].rfind(' ').filter(|&space| space > 0).unwrap_or(end)
        };
        s.put(x, y, room, &note[..end], FAINT);
        note = note[end..].trim_start();
    }
}
