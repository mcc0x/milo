use crate::git::{Commit, GIT_UNAVAILABLE, PendingFile};
use crate::help::{self, Page};
use crate::pane::Pane;
use crate::preferences::IconMode;
use crate::screen::{self, Screen, Style, Wash};
use crate::ui::{self, FAINT};
use crate::when;
use crate::width::{char_width, str_width};

use super::{Entry, FILE_PREVIEW, History, Painted, TimelineRow};

const HEADING_ROWS: usize = 4;
const DOT_ROWS: usize = 8;
const DOT_COLS: usize = 52;
const HINT_ROWS: usize = 12;
const DETAIL_COLS: usize = 28;
const DETAIL_ROWS: usize = 6;

impl History {
    pub(super) fn help_page() -> Page {
        use help::{HelpRow, heading, key, note, sample};

        Page::new(vec![
            heading("Commits"),
            sample("○ C", Style::PLAIN.bold(), "files to commit"),
            sample("● P", Style::PLAIN.bold(), "a commit to push"),
            sample("● =", FAINT, "already pushed"),
            sample("●", Style::PLAIN, "no comparison"),
            sample("•", Style::PLAIN.bold(), "just arrived"),
            sample("+2", ui::ADD, "lines added"),
            sample("−1", ui::DEL, "lines removed"),
            HelpRow::new(),
            heading("Keys"),
            key("↑↓   ", "move (k j)"),
            key("PgUp ", "previous page"),
            key("PgDn ", "next page"),
            key("g G  ", "first/last"),
            key("Enter", "show files"),
            key("Enter", "hide files"),
            key("Enter", "show all"),
            key("Enter", "read"),
            key("← h  ", "parent or close"),
            key("→ l  ", "open or page text"),
            key("r    ", "reload"),
            key("q    ", "quit"),
            HelpRow::new(),
            note("Click to select a file."),
            note("Double-click to read."),
            note("Click to toggle files."),
            note("Focus a narrow pane"),
            note("to see the full subject."),
        ])
    }

    pub(super) fn draw_contents(&mut self, s: &mut Screen) {
        self.painted = None;
        if let Some(reader) = &mut self.reading {
            reader.draw(s);
            return;
        }
        s.clear();
        if s.w < 8 || s.h == 0 {
            return;
        }
        if !self.repo.git {
            let (note, style) =
                if self.repo.git_unavailable() { (GIT_UNAVAILABLE, ui::DEL) } else { ("Not a Git project.", FAINT) };
            s.put_fit(1, 0, s.w - 2, note, style);
            return;
        }
        let compact = s.h < HEADING_ROWS || s.w < 12;
        if compact && self.entries.is_empty() && !self.repo.ok() {
            s.put_fit(1, 0, s.w - 2, "Git did not answer.", ui::DEL);
            return;
        }
        if self.compact != compact {
            self.compact = compact;
            self.rebuild_in_place(self.selected());
        }
        let dots = s.h >= DOT_ROWS && s.w >= DOT_COLS && self.entries.len() > 1;
        let hint = s.h >= HINT_ROWS || (s.h > 1 && self.read_error.is_some());
        let body_top = usize::from(!compact) + usize::from(dots);
        self.shown = s.h - body_top - usize::from(hint);
        self.keep_cursor_in_view();
        let click_offset = self.clicked.map(|click| click.offset);
        let below_click = click_offset.map_or(self.shown, |offset| self.shown.saturating_sub(offset + 1));
        let selected = self.selected();
        // Leave room for the last note at the end, or for an expanded commit's preview.
        let list_rows = if self.truncated && self.rows[self.cursor + 1..].iter().all(|row| !row.selectable()) {
            self.rows.len() - self.cursor
        } else if matches!(selected, Some(TimelineRow::Commit(i)) if self.open_index() == Some(i)) {
            2
        } else {
            1
        };
        let room = (self.shown / 2)
            .min(DETAIL_ROWS)
            .min(below_click.saturating_sub(list_rows - 1))
            .min(self.shown.saturating_sub(list_rows));
        let previous_detail = self.detail.take();
        let mut detail = [""; DETAIL_ROWS];
        let mut remaining = "";
        let mut detail_rows = 0;
        if self.focused && s.w <= DETAIL_COLS && room > 0 {
            let commit = match selected {
                Some(TimelineRow::Commit(i)) => Some(i),
                Some(TimelineRow::File(_) | TimelineRow::More) => self.open_index(),
                _ => None,
            };
            if let Some(i) = commit {
                let subject = &self.entries[i].commit.subject;
                let start = previous_detail.filter(|detail| detail.commit == i).map_or(0, |detail| detail.start);
                let (first, rest) = wrap_line(&subject[start..], s.w - 4);
                if start != 0 || !rest.is_empty() {
                    detail[0] = first;
                    detail_rows = 1;
                    remaining = rest;
                    for line in &mut detail[1..room] {
                        if remaining.is_empty() {
                            break;
                        }
                        (*line, remaining) = wrap_line(remaining, s.w - 4);
                        detail_rows += 1;
                    }
                    let next = if remaining.is_empty() { 0 } else { subject.len() - remaining.len() };
                    self.detail = Some(super::Detail { commit: i, start, next });
                }
            }
        }
        self.shown -= detail_rows;
        // Keep the clicked row under the pointer; otherwise the peek can hide
        // rows without changing the underlying list.
        let top = click_offset.map_or_else(
            || self.top.max((self.cursor + list_rows.min(self.shown)).saturating_sub(self.shown)),
            |offset| self.cursor.saturating_sub(offset.min(self.shown - 1)),
        );
        if !compact {
            self.draw_heading(s);
        }
        if dots {
            self.draw_dots(s);
        }
        self.draw_body(s, body_top, top);
        self.painted = Some(Painted { width: s.w, body_top, top });
        let bottom = body_top + self.shown;
        for (i, line) in detail.into_iter().take(detail_rows).enumerate() {
            let y = bottom + i;
            if i == 0 {
                s.put(1, y, 1, "↳", FAINT);
            }
            s.put(3, y, s.w - 4, line, Style::PLAIN);
            if i + 1 == detail_rows && !remaining.is_empty() {
                s.put(s.w - 1, y, 1, "→", FAINT);
            }
        }
        if hint {
            self.draw_hint(s);
        }
    }

    fn draw_heading(&self, s: &mut Screen) {
        let branch = self.repo.branch();
        let name_width = ui::project_heading_width(&self.repo.name, branch);
        let summary = self.draw_summary(s, name_width);
        let room = s.w.saturating_sub(summary + 3);
        let branch_room = str_width(branch).min(room / 2);
        let skip = if branch_room > 0 { 2 } else { 0 };
        let name = ui::section_heading(s, 0, room.saturating_sub(branch_room + skip), &self.repo.name);
        s.put_fit(1 + name + skip, 0, branch_room, branch, Style::fg(screen::MAGENTA));
    }

    fn draw_summary(&self, s: &mut Screen, name_width: usize) -> usize {
        let right = s.w - 1;
        if !self.repo.ok() {
            let text = if name_width + 18 <= s.w { "git did not answer" } else { "git error" };
            return s.put_right(right, 0, text, ui::DEL);
        }
        if !self.repo.has_remote() {
            return s.put_right(right, 0, "no remote", FAINT);
        }
        let Some(ahead) = self.version.as_ref().and_then(|version| version.ahead) else {
            let text = if name_width + 17 <= s.w { "no branch tracked" } else { "no tracking" };
            return s.put_right(right, 0, text, FAINT);
        };
        if ahead == 0 {
            let text = if name_width + 12 <= s.w { "✓ all pushed" } else { "✓" };
            return s.put_right(right, 0, text, ui::ADD);
        }
        let count = u64::from(ahead);
        let digits = digits(count);
        let words = if ahead == 1 { [" commit to push", " commit ↑"] } else { [" commits to push", " commits ↑"] };
        if let Some(word) = words.into_iter().find(|word| name_width + digits + str_width(word) <= s.w) {
            count_right(s, 0, count, word, Style::PLAIN)
        } else {
            s.put_number(right, 0, count, Style::PLAIN);
            s.put(right.saturating_sub(digits + 1), 0, 1, "↑", Style::PLAIN);
            digits + 1
        }
    }

    /// Shows the shape of recent history: a gap between days, a tick at the remote.
    fn draw_dots(&self, s: &mut Screen) {
        let right = s.w - 1;
        let cells = self.entries.len() + usize::from(self.uncommitted > 0);
        // If even the dots alone would not fit with spaces, don't count the gaps.
        let spaced = cells * 2 < right && {
            let days = self.entries.windows(2).filter(|pair| pair[0].commit.day != pair[1].commit.day).count();
            cells * 2 + days + 2 * usize::from(self.remote_boundary.is_some()) < right
        };
        let step = if spaced { 2 } else { 1 };
        let mut x = 1;
        if self.uncommitted > 0 {
            s.put(x, 1, 1, self.uncommitted_mark(), Style::PLAIN.bold());
            x += step;
        }
        let mut before: Option<&Commit> = None;
        for (i, entry) in self.entries.iter().enumerate() {
            let commit = &entry.commit;
            if let Some(prev) = before {
                if self.remote_boundary == Some(i) && x < right {
                    s.put(x, 1, 1, "│", FAINT);
                    x += step;
                }
                x += usize::from(prev.day != commit.day);
            }
            if x >= right {
                break;
            }
            s.put(x, 1, 1, self.commit_mark(commit), self.dot_style(commit));
            x += step;
            before = Some(commit);
        }
    }

    fn uncommitted_mark(&self) -> &'static str {
        if self.text_git_marks { "C" } else { "○" }
    }

    fn commit_mark(&self, commit: &Commit) -> &'static str {
        match (self.text_git_marks, self.repo.remote(), commit.unpushed) {
            (true, Some(_), true) => "P",
            (true, Some(_), false) => "=",
            _ => "●",
        }
    }

    fn dot_style(&self, c: &Commit) -> Style {
        match (self.repo.remote(), c.unpushed) {
            (None, _) => Style::PLAIN,
            (Some(_), false) => FAINT,
            (Some(_), true) => Style::PLAIN.bold(),
        }
    }

    fn draw_divider(&self, s: &mut Screen, y: usize) {
        let right = s.w - 1;
        let x = 1 + s.put(1, y, right - 1, "── on ", FAINT);
        let target = self.repo.remote().unwrap_or_default();
        let end = x + s.put_fit(x, y, right - x - 3, target, FAINT);
        for x in end + 1..right {
            s.put(x, y, 1, "─", FAINT);
        }
    }

    fn draw_body(&self, s: &mut Screen, body_top: usize, top: usize) {
        let mut ages = Ages::default();
        let open_files = self.open_files();
        let files = open_files.map_or(&[][..], |diff| diff.files.as_slice());
        let right = s.w - 1;
        for (i, row) in self.rows.iter().enumerate().skip(top).take(self.shown) {
            let y = body_top + i - top;
            match *row {
                TimelineRow::Uncommitted => self.draw_uncommitted(s, y),
                TimelineRow::Divider => self.draw_divider(s, y),
                TimelineRow::Commit(index) => {
                    let end = if s.w <= DETAIL_COLS {
                        right
                    } else if self.open_index() == Some(index) {
                        let end = open_files.map_or(right, |diff| file_count(s, y, diff.files.len() + diff.unrepresented));
                        // Leave room for a useful subject before spending columns on the object ID.
                        let id = &self.entries[index].commit.hash[..7];
                        if end >= 3 + 20 + 1 + id.len() + 2 {
                            let start = end - id.len() - 2;
                            s.put(start, y, id.len(), id, FAINT);
                            if open_files.is_some() {
                                s.put(end - 1, y, 1, "·", FAINT);
                            }
                            start - 1
                        } else {
                            end
                        }
                    } else {
                        ages.draw(s, y, self.entries[index].commit.time)
                    };
                    self.draw_commit(s, y, end, index);
                }
                TimelineRow::File(index) => draw_file(s, y, &files[index], self.icons),
                TimelineRow::More => {
                    draw_more(s, y, files.len() - FILE_PREVIEW);
                }
                TimelineRow::Note(text) => {
                    s.put_fit(3, y, right - 3, text, FAINT);
                }
            }
            if i == self.cursor && row.selectable() {
                s.wash(0, y, right, Wash::Bar);
            }
        }
        ui::thumb(s, right, body_top, self.shown, self.rows.len(), top);
    }

    fn draw_uncommitted(&self, s: &mut Screen, y: usize) {
        let word = if self.uncommitted == 1 { "file" } else { "files" };
        let bold = Style::PLAIN.bold();
        s.put(1, y, 1, self.uncommitted_mark(), bold);
        s.put_fit(3, y, s.w - 4, &format!("{} {word} to commit", self.uncommitted), bold);
    }

    fn draw_commit(&self, s: &mut Screen, y: usize, end: usize, i: usize) {
        let Entry { commit, fresh, .. } = &self.entries[i];
        let bold = Style::PLAIN.bold();
        if *fresh {
            s.put(0, y, 1, "•", bold);
        }
        s.put(1, y, 1, self.commit_mark(commit), self.dot_style(commit));
        if self.open_index() == Some(i) {
            s.put(2, y, 1, "▾", FAINT);
        }
        let quiet = commit.merge || (!commit.unpushed && self.repo.remote().is_some());
        let style = if *fresh {
            bold
        } else if quiet {
            FAINT
        } else {
            Style::PLAIN
        };
        let split = prefix_len(&commit.subject);
        let room = end.saturating_sub(3);
        let used = s.put(3, y, room, &commit.subject[..split], FAINT);
        s.put_fit(3 + used, y, room - used, &commit.subject[split..], style);
    }

    fn draw_hint(&self, s: &mut Screen) {
        if let Some(error) = self.read_error {
            s.put_fit(1, s.h - 1, s.w - 2, error, FAINT);
            return;
        }
        let action = match self.selected() {
            Some(TimelineRow::Commit(i)) if self.open_index() == Some(i) => "Enter hide files",
            Some(TimelineRow::Commit(_)) => "Enter show files",
            Some(TimelineRow::File(_)) => "Enter read",
            Some(TimelineRow::More) => "Enter show all",
            _ => "",
        };
        let middle: &[_] =
            if self.entries.is_empty() { &["r reload", "q quit"] } else { &["↑↓ move", "r reload", "q quit"] };
        help::draw_hint(s, action, middle, "? help");
    }
}

/// Break at a word when possible; a long token still makes progress in terminal cells.
fn wrap_line(text: &str, width: usize) -> (&str, &str) {
    let mut used = 0;
    let mut space = None;
    for (i, ch) in text.char_indices() {
        if i > 0 && ch.is_whitespace() {
            space = Some(i);
        }
        used += char_width(ch);
        if used > width {
            let end = space.unwrap_or(i);
            return (&text[..end], text[end..].trim_start());
        }
    }
    (text, "")
}

/// Leaves the age blank when it would repeat the one above.
#[derive(Default)]
struct Ages {
    now: Option<i64>,
    time: Option<i64>,
    shown: Option<when::Age>,
}

impl Ages {
    fn draw(&mut self, s: &mut Screen, y: usize, time: i64) -> usize {
        let right = s.w - 1;
        if self.time == Some(time) {
            return right;
        }
        self.time = Some(time);
        let age = when::short_age(self.now.get_or_insert_with(when::now).saturating_sub(time));
        if Some(age) == self.shown {
            return right;
        }
        let used = ui::age_right(s, right, y, age);
        self.shown = Some(age);
        right - used - 1
    }
}

fn count_right(s: &mut Screen, y: usize, count: u64, suffix: &str, style: Style) -> usize {
    let right = s.w - 1;
    let used = s.put_right(right, y, suffix, style);
    s.put_number(right - used, y, count, style);
    digits(count) + used
}

fn digits(count: u64) -> usize {
    count.checked_ilog10().unwrap_or(0) as usize + 1
}

fn draw_more(s: &mut Screen, y: usize, count: usize) {
    let Ok(count) = u32::try_from(count) else { return };
    let right = s.w - 1;
    let start = 5 + s.put(5, y, 2, "… ", FAINT);
    let end = start + digits(count.into());
    if end <= right {
        s.put_number(end, y, count.into(), FAINT);
        s.put_fit(end, y, right - end, " more", FAINT);
    }
}

fn file_count(s: &mut Screen, y: usize, files: usize) -> usize {
    let right = s.w - 1;
    let Ok(count) = u32::try_from(files) else { return right };
    let suffix = if count == 1 { " file" } else { " files" };
    let width = digits(count.into()) + suffix.len();
    if width + 5 > right {
        return right;
    }
    right - count_right(s, y, count.into(), suffix, FAINT) - 1
}

fn draw_file(s: &mut Screen, y: usize, file: &PendingFile, icons: IconMode) {
    let right = s.w - 1;
    let size = ui::size_width(file.add, file.del);
    let word = ui::kind_word(file.kind);
    let left = 5;
    let details = size + usize::from(size > 0) + word.len();
    // Keep enough room for the change details even when the name must be clipped.
    let name_end = if left + 4 + details <= right {
        ui::size_right(s, right, y, file.add, file.del);
        let end = right - size - usize::from(size > 0);
        s.put_right(end, y, word, ui::kind_colour(file.kind));
        end - word.len() - 1
    } else {
        right
    };
    s.put(3, y, 1, "│", FAINT);
    ui::file_name(s, left, y, name_end - left, file, icons, false);
}

/// A leading `scope: ` is dimmed so the rest of the subject stands out.
fn prefix_len(subject: &str) -> usize {
    match subject.split_once(": ") {
        Some((scope, _)) if scope.len() <= 24 && !scope.contains(' ') => scope.len() + 2,
        _ => 0,
    }
}

#[test]
fn a_scope_prefix_is_one_short_word() {
    assert_eq!(prefix_len("fix: off by one"), 5);
    assert_eq!(prefix_len("feat(ui): tabs"), 10);
    assert_eq!(prefix_len("Merge branch: not a scope"), 0);
    assert_eq!(prefix_len("No prefix here"), 0);
}
