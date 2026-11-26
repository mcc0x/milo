//! Draws the file, its patch marks and its headings.

use std::ops::Range;
use std::time::Instant;

use super::changes::{Fold, Row};
use super::edit::Editor;
use super::handed::Handed;
use super::{Body, JumpNotice, MarkdownBody, Reader, Source, Spot, markdown};
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

/// Columns of a heading kept for the file's name, whatever is said at its right.
const NAME_ROOM: usize = 10;
const PLACE_OF: &str = " of ";
const PLACE_SPACE: &str = " · ";
const HELP: &str = "? help";
const ACTIONS: &str = "a Actions";
const ACTIONS_SHORT: &str = "Actions";
const EDIT: &str = "e edit";
const GAP: &str = " · ";

#[derive(Clone, Copy)]
pub(super) enum Hint {
    Actions,
    Help,
}

#[derive(Default)]
pub(super) struct Hints {
    row: usize,
    actions: Option<Range<usize>>,
    help: Option<Range<usize>>,
}

impl Hints {
    pub(super) fn at(&self, x: usize, y: usize) -> Option<(Hint, &Range<usize>)> {
        if y != self.row {
            return None;
        }
        [(Hint::Actions, &self.actions), (Hint::Help, &self.help)]
            .into_iter()
            .find_map(|(hint, span)| span.as_ref().filter(|span| span.contains(&x)).map(|span| (hint, span)))
    }
}

fn digits(n: usize) -> usize {
    n.max(1).ilog10() as usize + 1
}

/// The words of a heading that say whose patch a file is shown from.
fn handed_lead(handed: &Handed) -> (&'static str, &str) {
    match &handed.from {
        Some(name) => ("patch from ", name.as_str()),
        None => ("handed-over patch", ""),
    }
}

/// Whether a heading `w` columns wide has room, after those words, to say
/// which of the patch's files this is.
fn handed_place_fits(handed: &Handed, w: usize) -> bool {
    let (lead, name) = handed_lead(handed);
    let place = digits(handed.number) + PLACE_OF.len() + digits(handed.total) + str_width(PLACE_SPACE);
    handed.total > 1 && lead.len() + str_width(name) + place <= w.saturating_sub(1 + NAME_ROOM)
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
    /// The column up to which a line is read to draw it: what is shown, and
    /// what follows that the colours of what is shown can depend on.
    pub(super) fn reach(&self) -> usize {
        self.pan + self.width + self.lang.map_or(0, Lang::lookahead)
    }

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
                // What is past the right edge is read only where something shown runs into it.
                let rest = &text[skip..];
                let shown = if rest.len() <= self.width { text.len() } else { skip + byte_at_col(rest, used) };
                lang.scan_shown(text, shown, state, self.spans);
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
        self.clear_hits();
        if let Some(mut editor) = self.editor.take() {
            s.clear();
            // The frame before this one said a formatter was about to run; this one shows what it did.
            editor.format_if_announced();
            self.draw_editor(s, &mut editor, now);
            self.editor = Some(editor);
            return;
        }
        self.tick_media();
        if let Body::Video(video) = &mut self.body {
            video.tick();
        }
        if self.change_notice.is_some_and(|(_, until)| now >= until) {
            self.change_notice = None;
        }
        if let Some(preview) = &mut self.markdown_preview {
            preview.expire_marks(now);
        }
        if let Some(menu) = &mut self.actions {
            menu.draw(s, &self.path);
            return;
        }
        s.clear();
        let hint = s.h >= 3;
        self.rows = s.h.saturating_sub(1 + usize::from(hint));
        self.columns = 0;
        self.pan_limit = 0;
        if s.w == 0 || s.h == 0 {
            return;
        }
        let notice = self.change_notice.map(|(notice, _)| notice);
        let right = match notice {
            Some(notice) if !hint => draw_change_notice(s, 0, notice).start,
            _ => self.draw_details(s),
        };
        self.draw_heading(s, right);
        if self.rows > 0 {
            self.draw_body(s);
        }
        if hint {
            self.hints.row = s.h - 1;
            if let Some(notice) = notice {
                let shown = draw_change_notice(s, self.hints.row, notice);
                let start = shown.end + str_width(GAP);
                if start + HELP.len() + shown.start <= s.w {
                    s.put(shown.end, self.hints.row, str_width(GAP), GAP, FAINT);
                    s.put(start, self.hints.row, HELP.len(), HELP, FAINT);
                    self.hints.help = Some(start..start + HELP.len());
                }
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
            self.hints.row = y;
            self.hints.help = editor.draw_hint(s);
        }
    }

    fn draw_body(&mut self, s: &mut Screen) {
        self.place_spot();
        if !self.markdown_source
            && let Some(body) = self.markdown_body()
        {
            match body {
                MarkdownBody::Empty => {
                    let width = s.w - usize::from(s.w > 1);
                    s.put_fit(0, 1, width, markdown::EMPTY, FAINT);
                }
                MarkdownBody::Populated => {
                    if let Some(preview) = &mut self.markdown_preview {
                        preview.draw(s, self.rows);
                    }
                }
            }
            return;
        }
        if std::mem::take(&mut self.opening) {
            self.reveal_first_change();
        }
        self.clamp_top();
        if let Some(line) = self.pick_cursor() {
            // A panel under the file can have taken the cursor's row.
            self.show_cursor(line);
        }
        let focused = self.focused_fold();
        let note = match &self.body {
            Body::Note(note) => Some(*note),
            Body::Picture(picture) => super::media::picture(s, self.rows, picture),
            Body::Video(video) => super::media::movie(s, self.rows, video),
            Body::Text(text) if text.lines.is_empty() => Some(super::content::EMPTY),
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
                let marked = self.spot.map(|spot| spot.line);
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
                            let shown = &text.lines[line];
                            painter.draw_line(s, i + 1, shown, state);
                            if x > 0 && shown.kind != LineKind::Del && marked == Some(shown.number) {
                                // The line the reader was asked for keeps its number picked out.
                                s.paint(x - 3 - text.digits, i + 1, text.digits, Style::PLAIN.bold().reverse());
                            }
                            let shown = super::pick::Shown { line, gutter: x > 0 };
                            super::pick::draw_marks(self.pick, &self.noted, s, i + 1, shown);
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

    /// The hints of a file of a handed-over patch. With several files, moving
    /// between them is kept longest, and says which file this is where the
    /// heading has no room to. Middle hints are given up from the left, so
    /// what the notes need stays longest.
    fn draw_handed_hint(&self, s: &mut Screen, handed: &Handed) -> Option<Range<usize>> {
        if self.pick.is_some() {
            // None of these opens the help, so none is a place to click for it.
            crate::help::draw_hint(s, "↑↓ line", &["v range", "n N changes", "Esc back"], "Enter write");
            return None;
        }
        let counted = if handed.total == 1 || handed_place_fits(handed, s.w) {
            String::new()
        } else {
            let spelled = format!("[ ] file {} of {}", handed.number, handed.total);
            if spelled.len() + 3 + HELP.len() + 2 <= s.w {
                spelled
            } else {
                format!("[ ] {}/{}", handed.number, handed.total)
            }
        };
        let files = match (handed.total, counted.is_empty()) {
            (1, _) => "",
            (_, true) => "[ ] files",
            (_, false) => &counted,
        };
        let fold = self.can_fold().then(|| if self.review.is_compact() { "c full file" } else { "c changes only" });
        let changes = self.can_fold() || self.has_changes_among_other_lines();
        let text = matches!(&self.body, Body::Text(text) if !text.lines.is_empty());
        let scroll = matches!(self.body, Body::Text(_)).then_some("↑↓ scroll");
        let waiting = match self.waiting_notes {
            None => String::new(),
            Some(0) => "p notes".to_string(),
            Some(1) => "p 1 note".to_string(),
            Some(notes) => format!("p {notes} notes"),
        };
        let hints = [
            scroll,
            Some("q close"),
            fold,
            changes.then_some("n N changes"),
            text.then_some("i note"),
            (!waiting.is_empty()).then_some(waiting.as_str()),
        ];
        let mut middle = [""; 6];
        let mut count = 0;
        for item in hints.into_iter().flatten() {
            middle[count] = item;
            count += 1;
        }
        crate::help::draw_hint(s, files, &middle[..count], HELP).span(count + 1)
    }

    /// Whether `n` has a change to go to and a line that is not one to leave.
    fn has_changes_among_other_lines(&self) -> bool {
        matches!(&self.body, Body::Text(text)
            if text.added + text.removed > 0 && text.lines.len() > (text.added + text.removed) as usize)
    }

    fn draw_hint(&mut self, s: &mut Screen) {
        if let Body::Video(video) = &self.body {
            if video.error().is_some() {
                self.draw_reader_hint(s, "q close", &[]);
                return;
            }
            let action = if video.is_playing() { "Space pause" } else { "Space play" };
            self.draw_reader_hint(s, action, &["←→ seek 5s", "q close"]);
            return;
        }
        if let Some(preview) = self.markdown() {
            let action = if self.markdown_source { "m preview" } else { "m source" };
            let follow = if preview.following() { "↑↓ stop follow" } else { "G follow end" };
            let middle: &[_] = match (self.read_only.is_none(), self.markdown_source, self.can_fold()) {
                (true, false, _) => &[follow, EDIT, "q close"],
                (false, false, _) => &[follow, "q close"],
                (true, true, true) => &[EDIT, "n N changes", "c fold", "q close"],
                (false, true, true) => &["n N changes", "c fold", "q close"],
                (true, true, false) => &[EDIT, "↑↓ scroll", "q close"],
                (false, true, false) => &["↑↓ scroll", "q close"],
            };
            self.draw_reader_hint(s, action, middle);
            return;
        }
        if let Source::Handed(handed) = &self.source {
            self.hints.help = self.draw_handed_hint(s, handed);
            return;
        }
        if self.can_fold() {
            let action = if self.review.is_compact() { "c full file" } else { "c changes only" };
            let middle: &[_] = if self.read_only.is_some() {
                &["↑↓ scroll", "q close", "n N changes"]
            } else {
                &[EDIT, "↑↓ scroll", "q close", "n N changes"]
            };
            self.draw_reader_hint(s, action, middle);
            return;
        }
        let changes = self.has_changes_among_other_lines();
        let working = matches!(self.source, Source::Working(_));
        let still = !matches!(self.body, Body::Text(_));
        let action = match (changes, working) {
            (true, false) => "n N changes",
            (true, true) => "n next change",
            (false, _) => "",
        };
        let middle: &[_] = match (still, self.read_only.is_none()) {
            (true, true) => &[EDIT, "q close"],
            (true, false) => &["q close"],
            (false, true) => &["↑↓ scroll", EDIT, "q close"],
            (false, false) => &["↑↓ scroll", "q close"],
        };
        self.draw_reader_hint(s, action, middle);
    }

    fn draw_reader_hint(&mut self, s: &mut Screen, action: &str, middle: &[&str]) {
        let mut items = [""; 6];
        let native = usize::from(self.offers_native_actions());
        if native > 0 {
            items[0] = "^O open · ^R reveal";
        }
        let actions_at = native + middle.len();
        items[native..actions_at].copy_from_slice(middle);
        let actions = self.offers_actions();
        let mut compact = false;
        if actions {
            let edit = middle.iter().position(|&item| item == EDIT);
            let tail = edit.map_or(&["q close"][..], |i| &middle[i..]);
            let required = 2 + HELP.len() + str_width(action) + ACTIONS.len();
            let gaps = 1 + usize::from(!action.is_empty());
            let occupied = required + tail.iter().map(|item| str_width(item)).sum::<usize>();
            let decorated = occupied + (gaps + tail.len()) * str_width(GAP);
            compact = s.w < decorated && s.w >= required + gaps;
            items[actions_at] = if s.w >= decorated || compact { ACTIONS } else { ACTIONS_SHORT };
            if compact
                && s.w < occupied + gaps + tail.len()
                && let Some(i) = edit
                && s.w >= required + gaps + 1 + EDIT.len()
            {
                // The painter drops a prefix of the middle hints. In a split
                // too narrow for the whole tail, keep Edit beside Actions.
                items[native + i..actions_at].rotate_left(1);
            }
        }
        let count = actions_at + usize::from(actions);
        let hits = if compact {
            crate::help::draw_compact_hint(s, action, &items[..count], HELP)
        } else {
            crate::help::draw_hint(s, action, &items[..count], HELP)
        };
        self.hints.help = hits.span(count + 1);
        if actions {
            self.hints.actions = hits.span(actions_at + 1);
        }
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
        if let Source::Committed { hash, time, .. } = &self.source {
            let short_id = super::short_commit_id(hash);
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
        } else if let Source::Handed(handed) = &self.source {
            // Whose patch it is said to be comes first: it is what tells this from a file's own changes.
            let (lead, name) = handed_lead(handed);
            let room = right.saturating_sub(NAME_ROOM);
            if lead.len() + str_width(name) <= room {
                right -= s.put_right(right, 0, name, FAINT);
                right -= s.put_right(right, 0, lead, FAINT);
                if handed_place_fits(handed, s.w) {
                    right -= s.put_right(right, 0, PLACE_SPACE, FAINT);
                    s.put_number(right, 0, handed.total as u64, FAINT);
                    right -= digits(handed.total);
                    right -= s.put_right(right, 0, PLACE_OF, FAINT);
                    s.put_number(right, 0, handed.number as u64, FAINT);
                    right -= digits(handed.number);
                }
            } else if "patch".len() <= room {
                right -= s.put_right(right, 0, "patch", FAINT);
            }
        } else if let Body::Video(video) = &self.body
            && video.error().is_none()
        {
            let mut buffer = [0; 64];
            let status = super::media::status(video, &mut buffer);
            let room = right.saturating_sub(NAME_ROOM);
            let state = if video.is_playing() { "Playing" } else { "Paused" };
            let label = if str_width(status) <= room { status } else { state };
            if str_width(label) <= room {
                right -= s.put_right(right, 0, label, FAINT);
            }
        } else if let Body::Picture(picture) = &self.body {
            let size = format!("{}×{}", picture.width, picture.height);
            if str_width(&size) <= right.saturating_sub(NAME_ROOM) {
                right -= s.put_right(right, 0, &size, FAINT);
            }
        } else if self.markdown().is_some_and(super::markdown::Preview::following) {
            let room = right.saturating_sub(NAME_ROOM);
            let label = if "Following end".len() <= room { "Following end" } else { "Following" };
            if label.len() <= room {
                right -= s.put_right(right, 0, label, Style::PLAIN.bold());
                right = right.saturating_sub(2);
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

/// Says the place the reader was asked to show, as `Line 184:17`, at the same edge as a change notice.
fn draw_spot_notice(s: &mut Screen, y: usize, spot: Spot) -> Range<usize> {
    const PREFIX: &str = "Line ";
    let digits = |n: u32| n.ilog10() as usize + 1;
    let line = digits(spot.line);
    let full = PREFIX.len() + line + spot.column.map_or(0, |column| 1 + digits(column));
    let margin = usize::from(s.w >= full + 2);
    let room = s.w - margin * 2;
    let width = full.min(room);
    let x = if y == 0 { s.w - margin - width } else { margin };
    let st = Style::PLAIN;
    if full > room {
        s.put_fit(x, y, width, "Line…", st);
        return x..x + width;
    }
    let end = x + s.put(x, y, width, PREFIX, st) + line;
    s.put_number(end, y, spot.line.into(), st);
    if let Some(column) = spot.column {
        s.put(end, y, 1, ":", st);
        s.put_number(x + full, y, column.into(), st);
    }
    x..x + width
}

/// Uses the right edge of the heading or the left edge of the hint.
fn draw_change_notice(s: &mut Screen, y: usize, notice: JumpNotice) -> Range<usize> {
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
        JumpNotice::Spot(spot) => return draw_spot_notice(s, y, spot),
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
    x..x + width
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
