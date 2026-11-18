//! `strata view FILE`: one file, read-only, with what changed in it marked.
//!
//! A file with pending changes is shown whole, with the lines it gained and
//! lost in place and the words that changed inside an edited line picked
//! out. Any other file is shown as it is.

use std::path::{Path, PathBuf};

use crate::git::{Kind, Repo, Stamp};
use crate::read::{self, Content};
use crate::syntax::{self, Class, Lang, Span, State};
use crate::term::{self, Key, Screen, Style, Wash};
use crate::ui::{self, ADD, DEL, DIM};
use crate::{help, icons};

/// Files larger than this are not shown.
const LIMIT: u64 = 2 << 20;
const TAB: usize = 4;
/// Lines of the file kept in view above a change that was jumped to.
const LEAD: usize = 3;
/// Columns kept past the right edge so a word cut by it can still be coloured whole.
const LEAD_OUT: usize = 32;

#[derive(Clone, Copy, PartialEq, Debug)]
enum LineKind {
    Same,
    Add,
    Del,
}

#[derive(PartialEq, Debug)]
struct Line {
    kind: LineKind,
    /// Line number in the file: the current one, or the old one for a removal.
    no: u32,
    text: String,
    /// Display width, measured once so a long line need not be measured on every draw.
    width: usize,
    /// Byte range of `text` that differs from its paired line; empty if none.
    changed: (usize, usize),
    /// What was still open in the code where this line begins.
    state: State,
}

impl Line {
    fn new(kind: LineKind, no: u32, text: &str) -> Self {
        let text = clean(text);
        let width = term::str_width(&text);
        Self { kind, no, text, width, changed: (0, 0), state: State::default() }
    }

    /// The byte offset to draw from and the prefix needed to colour what is shown.
    fn text_for_draw(&self, pan: usize, width: usize) -> (usize, &str) {
        let skip = term::byte_at_col(&self.text, pan);
        // A character occupies at most four bytes. Keep the prefix for its syntax.
        let end = self.text.floor_char_boundary(skip + (width + LEAD_OUT) * 4);
        (skip, &self.text[..end])
    }
}

pub struct View {
    repo: Repo,
    /// The file, relative to the repository root.
    path: String,
    abs: PathBuf,
    stamp: Stamp,
    lines: Vec<Line>,
    /// Said in place of the content when there is none to show.
    note: Option<&'static str>,
    /// What is pending for this file; `Kind::None` if nothing is.
    kind: Kind,
    /// The language the file is written in, if it is one this colours.
    lang: Option<&'static Lang>,
    /// The coloured pieces of the line being drawn, kept to save making it
    /// again for each.
    spans: Vec<Span>,
    top: usize,
    /// Display columns skipped on the left.
    pan: usize,
    /// Rows of content on screen, set by the last draw.
    rows: usize,
    /// The page of explanations, when it is up, and how far it is scrolled.
    help: Option<usize>,
}

/// The colour of each class of code. These follow the terminal's theme.
fn class_style(class: Class) -> Style {
    match class {
        Class::Comment => DIM.italic(),
        Class::Str => Style::fg(term::GREEN),
        Class::Number => Style::fg(term::YELLOW),
        Class::Keyword => Style::fg(term::MAGENTA),
        Class::Type => Style::fg(term::CYAN),
        Class::Function | Class::Key => Style::fg(term::BLUE),
    }
}

/// Expands tabs and drops control characters so a line is safe to draw.
fn clean(line: &str) -> String {
    let mut s = String::with_capacity(line.len());
    let mut col = 0;
    for c in line.chars() {
        if c == '\t' {
            let pad = TAB - col % TAB;
            s.extend(std::iter::repeat_n(' ', pad));
            col += pad;
        } else if !c.is_control() {
            s.push(c);
            col += 1;
        }
    }
    s
}

/// Finds the next or previous run of changes, wrapping at either end.
fn change_top(kinds: impl IntoIterator<Item = LineKind>, top: usize, last: usize, forward: bool) -> Option<usize> {
    let (mut first, mut final_top, mut previous) = (None, None, None);
    let mut same = true;
    for (i, kind) in kinds.into_iter().enumerate() {
        let starts = same && kind != LineKind::Same;
        same = kind == LineKind::Same;
        if !starts {
            continue;
        }
        // Include the lead-in, but never go past the top of the last page.
        let at = i.saturating_sub(LEAD).min(last);
        if forward && at > top {
            return Some(at);
        }
        if !forward && at >= top && previous.is_some() {
            return previous;
        }
        first.get_or_insert(at);
        final_top = Some(at);
        if at < top {
            previous = Some(at);
        }
    }
    if forward { first } else { previous.or(final_top) }
}

/// Bytes two strings share at the start and at the end, without overlap.
fn common_ends(a: &str, b: &str) -> (usize, usize) {
    let prefix: usize = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    let (ra, rb) = (&a[prefix..], &b[prefix..]);
    let suffix = ra.chars().rev().zip(rb.chars().rev()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    (prefix, suffix)
}

/// Where a run of removals is followed by as many additions, marks the part
/// of each line that actually changed, so a one-word edit reads as one word.
fn mark_changed(lines: &mut [Line]) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != LineKind::Del {
            i += 1;
            continue;
        }
        let dels = lines[i..].iter().take_while(|l| l.kind == LineKind::Del).count();
        let adds = lines[i + dels..].iter().take_while(|l| l.kind == LineKind::Add).count();
        if dels == adds {
            for k in i..i + dels {
                let (old, new) = (&lines[k].text, &lines[k + dels].text);
                let (prefix, suffix) = common_ends(old, new);
                // Lines that share less than half are different lines, not edits.
                if (prefix + suffix) * 2 < old.len().max(new.len()) {
                    continue;
                }
                let (old_end, new_end) = (old.len() - suffix, new.len() - suffix);
                lines[k].changed = (prefix, old_end);
                lines[k + dels].changed = (prefix, new_end);
            }
        }
        i += dels + adds;
    }
}

/// Reads a whole-file diff: every line of the file, with removed lines in
/// place among them, and what the diff says happened to the file.
fn parse_diff(raw: &[u8]) -> (Vec<Line>, Kind) {
    let mut lines = Vec::new();
    let mut kind = Kind::None;
    let (mut old, mut new, mut in_hunk, mut second) = (1, 1, false, false);
    for l in String::from_utf8_lossy(raw).lines() {
        if l.starts_with("diff --git ") {
            // A file that changed type, say into a link, comes as two diffs.
            (in_hunk, second) = (false, kind != Kind::None);
            kind = kind.max(Kind::Modified);
            continue;
        }
        if let Some(hunk) = l.strip_prefix("@@ -") {
            // "@@ -12,5 +14,6 @@"
            let start = |s: &str| s.split([',', ' ']).next().and_then(|n| n.parse().ok()).unwrap_or(1);
            old = start(hunk);
            new = hunk.split_once(" +").map_or(1, |(_, n)| start(n));
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            // Between the file's name and its first hunk, git says what
            // became of it. "new mode" is not "new file mode".
            if l.starts_with("new file mode ") {
                // Deleted and then new again is one file, changed.
                kind = if second { Kind::Modified } else { Kind::Added };
            } else if l.starts_with("deleted file mode ") {
                kind = Kind::Deleted;
            } else if l.starts_with("rename from ") {
                kind = Kind::Renamed;
            }
            continue;
        }
        let (line_kind, no) = match l.as_bytes().first() {
            Some(b'+') => (LineKind::Add, &mut new),
            Some(b'-') => (LineKind::Del, &mut old),
            // "\ No newline at end of file"
            Some(b'\\') => continue,
            _ => {
                old += 1;
                (LineKind::Same, &mut new)
            }
        };
        lines.push(Line::new(line_kind, *no, l.get(1..).unwrap_or("")));
        *no += 1;
    }
    mark_changed(&mut lines);
    (lines, kind)
}

impl View {
    /// Opens `file`. It need not exist any more, nor the directory it was
    /// in, if git remembers it. `None` if nothing above it exists either.
    pub fn open(file: &Path) -> Option<View> {
        // The nearest directory that is still there says which repository
        // this is; the rest of the path is taken on trust.
        let here = file.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let dir = here.ancestors().find(|d| d.is_dir()).filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let repo = Repo::open(dir)?;
        let abs = dir.canonicalize().ok()?.join(file.strip_prefix(dir).unwrap_or(file));
        let path = abs.strip_prefix(&repo.root).ok()?.to_string_lossy().into_owned();
        let mut view = View {
            repo,
            path,
            abs,
            stamp: None,
            lines: Vec::new(),
            note: None,
            kind: Kind::None,
            lang: None,
            spans: Vec::new(),
            top: 0,
            pan: 0,
            rows: 0,
            help: None,
        };
        view.lang = syntax::for_file(&view.path);
        view.repo.only(&view.path);
        view.load(None);
        // Start at the first change, with a little of the file above it.
        let first = view.lines.iter().position(|l| l.kind != LineKind::Same);
        view.top = first.map_or(0, |i| i.saturating_sub(LEAD));
        Some(view)
    }

    /// The directories to watch, and whether watching them misses nothing.
    pub fn watch(&self) -> crate::watch::Scope {
        self.repo.watch()
    }

    /// The last write time and length of the path itself, never of a link's target.
    fn stamp(&self) -> Stamp {
        let m = std::fs::symlink_metadata(&self.abs).ok()?;
        Some((m.modified().ok()?, m.len()))
    }

    /// Reads the file and what is pending for it. `raw` is a status the
    /// repository was just asked for, if any.
    fn load(&mut self, raw: Option<Vec<u8>>) {
        self.read(raw);
        let Some(lang) = self.lang else { return };
        // Only what is on screen is ever coloured, when it is drawn. For
        // that, each line remembers what was still open where it begins: a
        // comment, say. A removed line is not part of the file as it now
        // reads, so the lines either side of it are neighbours.
        let mut state = State::default();
        for line in self.lines.iter_mut().filter(|l| l.kind != LineKind::Del) {
            line.state = state;
            lang.scan(&line.text, &mut state, &mut self.spans);
        }
    }

    fn read(&mut self, raw: Option<Vec<u8>>) {
        self.repo.read(raw);
        self.stamp = self.stamp();
        (self.lines, self.note, self.kind) = (Vec::new(), None, Kind::None);
        if self.repo.is_untracked(&self.path) {
            // Git has nothing to compare a file it does not track against.
            self.kind = Kind::Untracked;
        } else if self.repo.git {
            let Some(mut diff) = self.repo.file_diff(&self.path, None) else {
                self.note = Some(if matches!(read::file(&self.abs, LIMIT), Content::Unreadable) {
                    "This file cannot be read."
                } else {
                    "Too large to show."
                });
                return;
            };
            (self.lines, self.kind) = parse_diff(&diff);
            // A file that looks new may have been moved here; shown beside
            // where it was, only what changed in it counts as a change.
            if self.kind == Kind::Added {
                if let Some(from) = self.repo.moved_from(&self.path) {
                    diff = self.repo.file_diff(&self.path, Some(&from)).unwrap_or(diff);
                    (self.lines, self.kind) = parse_diff(&diff);
                }
            }
            if self.repo.is_unmerged(&self.path) {
                self.kind = Kind::Conflict;
            }
        }
        if !self.lines.is_empty() {
            return;
        }
        // A file git has never seen is new from its first line to its last.
        let kind = if self.kind == Kind::Untracked { LineKind::Add } else { LineKind::Same };
        match read::file(&self.abs, LIMIT) {
            Content::Gone => self.note = Some("This file is gone."),
            Content::Unreadable => self.note = Some("This file cannot be read."),
            Content::NotFile => self.note = Some("Not a file, so there is nothing to show."),
            Content::TooLarge => self.note = Some("Too large to show."),
            Content::Link(target) => self.lines.push(Line::new(kind, 1, &target.to_string_lossy())),
            Content::Bytes(bytes) if bytes.contains(&0) => self.note = Some("Not text, so there is nothing to show."),
            Content::Bytes(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                self.lines = text.lines().enumerate().map(|(i, l)| Line::new(kind, i as u32 + 1, l)).collect();
                if self.lines.is_empty() {
                    self.note = Some("Empty.");
                }
            }
        }
    }

    /// Looks for changes to the file or to the repository around it.
    /// Returns false if it is too soon to tell, and worth looking again.
    pub fn check(&mut self) -> bool {
        if self.repo.busy() {
            return false;
        }
        let raw = self.repo.poll(&[]);
        if raw.is_some() || self.stamp() != self.stamp {
            self.load(raw);
            self.scroll(0);
        }
        true
    }

    fn last_top(&self) -> usize {
        self.lines.len().saturating_sub(self.rows.max(1))
    }

    fn scroll(&mut self, delta: isize) {
        self.top = self.top.saturating_add_signed(delta).min(self.last_top());
    }

    /// Jumps to the next or previous run of changed lines.
    fn jump(&mut self, forward: bool) {
        let kinds = self.lines.iter().map(|l| l.kind);
        if let Some(top) = change_top(kinds, self.top, self.last_top(), forward) {
            self.top = top;
        }
    }

    pub fn draw(&mut self, s: &mut Screen) {
        if let Some(top) = &mut self.help {
            return help::draw(s, &help::reader(), top);
        }
        s.clear();
        let (w, h) = (s.w, s.h);
        if w < 20 || h < 3 {
            return;
        }
        self.rows = h - 2;
        self.scroll(0);
        self.header(s);
        if let Some(note) = self.note {
            s.put_fit(2, 2, w - 4, note, DIM);
        }

        let digits = self.lines.iter().map(|l| l.no).max().unwrap_or(1).max(1).ilog10() as usize + 1;
        // [space][line number][space][sign][space][text], the last column the thumb's.
        let text_x = digits + 4;
        let text_w = (w - 1).saturating_sub(text_x);
        // There is no panning past the end of the longest line in view.
        let shown = self.lines.iter().skip(self.top).take(self.rows);
        let longest = shown.clone().map(|l| l.width).max().unwrap_or(0);
        self.pan = self.pan.min(longest.saturating_sub(text_w));
        for (y, line) in (1..).zip(shown) {
            // In a language this knows, an added line keeps the colours of
            // its code and is marked in the margin; otherwise it is green
            // throughout. A removed line is red either way.
            let coloured = self.lang.is_some();
            let (sign, st, margin) = match line.kind {
                LineKind::Same => (" ", Style::PLAIN, DIM),
                LineKind::Add if coloured => ("+", Style::PLAIN, ADD.bold()),
                LineKind::Add => ("+", ADD, ADD),
                LineKind::Del => ("-", DEL, DEL),
            };
            // A removed line has no place in the file as it now reads, so
            // it has no number either.
            let no = if line.kind == LineKind::Del { String::new() } else { line.no.to_string() };
            s.put(1 + digits - no.len(), y, digits, &no, margin);
            s.put(digits + 2, y, 1, sign, margin);
            // Panning skips whole characters off the left of the text, and
            // says that it has.
            if self.pan > 0 {
                s.put(text_x - 1, y, 1, "‹", DIM);
            }
            let (skip, text) = line.text_for_draw(self.pan, text_w);
            s.put(text_x, y, text_w, &text[skip..], st);
            // A removed line is not part of the file as it now reads, and
            // is not coloured as code.
            self.spans.clear();
            if let Some(lang) = self.lang.filter(|_| line.kind != LineKind::Del) {
                lang.scan(text, &mut { line.state }, &mut self.spans);
            }
            // Columns run on from one span to the next, so each is measured once.
            let (mut at, mut col) = (skip, 0);
            for &(start, end, class) in &self.spans {
                let (start, end) = (start.max(skip), end.max(skip));
                if col >= text_w {
                    break;
                }
                if start >= end {
                    continue;
                }
                col += term::str_width(&text[at..start]);
                let width = term::str_width(&text[start..end]);
                s.paint(text_x + col, y, width.min(text_w.saturating_sub(col)), class_style(class));
                (at, col) = (end, col + width);
            }
            let (a, b) = (line.changed.0.clamp(skip, text.len()), line.changed.1.clamp(skip, text.len()));
            if a < b {
                let from = term::str_width(&text[skip..a]);
                let width = term::str_width(&text[a..b]);
                s.emphasise(text_x + from, y, width.min(text_w.saturating_sub(from)));
            }
            // A tint across the row, so a change is seen without reading
            // the margin.
            match line.kind {
                LineKind::Same => {}
                LineKind::Add => s.wash(0, y, w - 1, Wash::Added),
                LineKind::Del => s.wash(0, y, w - 1, Wash::Removed),
            }
        }
        ui::thumb(s, w - 1, 1, self.rows, self.lines.len(), self.top);
        let changes = self.lines.iter().any(|l| l.kind == LineKind::Same) && self.kind != Kind::None;
        let hint = match (changes, w) {
            (true, 44..) => "↑↓ scroll · n next change · q close · ? help",
            (true, _) => "n next change · ? help",
            (false, 36..) => "↑↓ scroll · q close · ? help",
            (false, _) => "q close · ? help",
        };
        s.put_fit(1, h - 1, w - 2, hint, DIM);
    }

    /// The file's name and where it lives, and what is pending for it. The
    /// counts are of the lines shown, so they always agree with the body.
    fn header(&self, s: &mut Screen) {
        let w = s.w;
        let mut right = w - 1;
        if self.kind != Kind::None {
            let count = |kind| self.lines.iter().filter(|l| l.kind == kind).count() as u32;
            right -= ui::size_right(s, right, 0, count(LineKind::Add), count(LineKind::Del)).min(right);
            let word = ui::kind_word(self.kind);
            right = right.saturating_sub(word.len() + 2);
            s.put(right, 0, word.len(), word, ui::kind_colour(self.kind));
        }
        let (dir, name) = self.path.rsplit_once('/').unwrap_or(("", &self.path));
        let room = right.saturating_sub(2);
        let mut x = 1 + ui::icon(s, 1, 0, room, icons::file(name));
        x += s.put_fit(x, 0, room.saturating_sub(x), name, Style::PLAIN.bold());
        s.put_fit(x + 2, 0, room.saturating_sub(x + 2), dir, DIM);
    }

    /// Returns true when the viewer should close.
    pub fn on_key(&mut self, key: Key) -> bool {
        let page = self.rows.max(2) as isize - 1;
        if let Some(top) = self.help {
            self.help = match key {
                Key::Ctrl(b'c') | Key::Eof => return true,
                Key::Focus(_) => Some(top),
                Key::Char('j') | Key::Down | Key::WheelDown => Some(top + 1),
                Key::Char('k') | Key::Up | Key::WheelUp => Some(top.saturating_sub(1)),
                _ => None,
            };
            return false;
        }
        match key {
            Key::Char('q') | Key::Esc | Key::Ctrl(b'c') | Key::Eof => return true,
            Key::Char('?') => self.help = Some(0),
            Key::Char('j') | Key::Down => self.scroll(1),
            Key::Char('k') | Key::Up => self.scroll(-1),
            Key::WheelDown => self.scroll(3),
            Key::WheelUp => self.scroll(-3),
            Key::Char(' ') | Key::PgDn | Key::Ctrl(b'f') => self.scroll(page),
            Key::Char('b') | Key::PgUp | Key::Ctrl(b'b') => self.scroll(-page),
            Key::Ctrl(b'd') => self.scroll(page / 2),
            Key::Ctrl(b'u') => self.scroll(-page / 2),
            Key::Char('g') | Key::Home => self.scroll(isize::MIN),
            Key::Char('G') | Key::End => self.scroll(isize::MAX),
            Key::Char('n') => self.jump(true),
            Key::Char('N') => self.jump(false),
            Key::Char('l') | Key::Right => self.pan += 8,
            Key::Char('h') | Key::Left => self.pan = self.pan.saturating_sub(8),
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::fs::{self, Permissions};
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn shape(lines: &[Line]) -> Vec<(LineKind, u32, &str)> {
        lines.iter().map(|l| (l.kind, l.no, l.text.as_str())).collect()
    }

    fn drawn(view: &mut View, w: usize, h: usize) -> Vec<String> {
        let mut screen = Screen::sized(w, h);
        view.draw(&mut screen);
        screen.text()
    }

    #[test]
    fn the_reader_counts_added_and_removed_lines_and_numbers_only_the_current_file() {
        let f = Fixture::new();
        f.write("file.txt", "one\nbefore\nfour\n");
        f.commit("first");
        f.write("file.txt", "one\nafter\nthree\nfour\n");

        let mut view = View::open(&f.root.join("file.txt")).unwrap();
        let rows = drawn(&mut view, 40, 8);
        assert!(rows[0].ends_with("+2 −1"), "{rows:?}");
        assert_eq!(&rows[1..6], [" 1   one", "   - before", " 2 + after", " 3 + three", " 4   four"]);
    }

    #[test]
    fn every_line_of_an_untracked_file_is_added() {
        let f = Fixture::new();
        f.write("file.txt", "first\nlast without a newline");

        let mut view = View::open(&f.root.join("file.txt")).unwrap();
        let rows = drawn(&mut view, 45, 6);
        assert!(rows[0].contains("new") && rows[0].ends_with("+2"), "{rows:?}");
        assert_eq!(&rows[1..3], [" 1 + first", " 2 + last without a newline"]);
    }

    #[test]
    fn next_change_wraps_from_the_last_page_back_to_the_first_change() {
        let f = Fixture::new();
        let lines = |changed| {
            (1..=100)
                .map(|n| format!("line {n}{}\n", if changed && matches!(n, 10 | 95) { " edited" } else { "" }))
                .collect::<String>()
        };
        f.write("file.txt", &lines(false));
        f.commit("first");
        f.write("file.txt", &lines(true));

        let mut view = View::open(&f.root.join("file.txt")).unwrap();
        for n in [10, 95, 10, 95] {
            let rows = drawn(&mut view, 45, 22);
            let change = format!("+ line {n} edited");
            assert!(rows.iter().any(|row| row.contains(&change)), "{rows:?}");
            view.on_key(Key::Char('n'));
        }
    }

    #[test]
    fn a_diff_becomes_numbered_lines() {
        let (lines, kind) = parse_diff(
            b"diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,4 @@\n one\n-two\n+2\n+2.5\n three\n",
        );
        assert_eq!(kind, Kind::Modified);
        assert_eq!(
            shape(&lines),
            [
                (LineKind::Same, 1, "one"),
                (LineKind::Del, 2, "two"),
                (LineKind::Add, 2, "2"),
                (LineKind::Add, 3, "2.5"),
                (LineKind::Same, 4, "three"),
            ]
        );
    }

    #[test]
    fn a_diff_says_what_became_of_the_file() {
        let new = b"diff --git a/f b/f\nnew file mode 100644\n--- /dev/null\n+++ b/f\n@@ -0,0 +1 @@\n+x\n";
        assert_eq!(parse_diff(new).1, Kind::Added);
        let gone = b"diff --git a/f b/f\ndeleted file mode 100644\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        assert_eq!(parse_diff(gone).1, Kind::Deleted);
        let moved = b"diff --git a/f b/g\nsimilarity index 90%\nrename from f\nrename to g\n@@ -1 +1 @@\n-x\n+y\n";
        assert_eq!(parse_diff(moved).1, Kind::Renamed);
        assert_eq!(parse_diff(b"").1, Kind::None);
        // A file made runnable is edited, not new: "new mode", not "new file mode".
        let mode = b"diff --git a/f b/f\nold mode 100644\nnew mode 100755\n";
        assert_eq!(parse_diff(mode).1, Kind::Modified);
        // One swapped for a link comes as a deletion and then an addition.
        let swapped = b"diff --git a/f b/f\ndeleted file mode 100644\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n\
                        diff --git a/f b/f\nnew file mode 120000\n--- /dev/null\n+++ b/f\n@@ -0,0 +1 @@\n+t\n";
        assert_eq!(parse_diff(swapped).1, Kind::Modified);
    }

    #[test]
    fn a_second_diff_does_not_leak_its_headers_into_the_text() {
        // A file replaced by a link: git writes a deletion, then an addition.
        let raw = b"diff --git a/f b/f\ndeleted file mode 100644\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-text\n\
                    diff --git a/f b/f\nnew file mode 120000\n--- /dev/null\n+++ b/f\n@@ -0,0 +1 @@\n+target\n";
        let (lines, _) = parse_diff(raw);
        assert_eq!(shape(&lines), [(LineKind::Del, 1, "text"), (LineKind::Add, 1, "target")]);
    }

    #[test]
    fn an_edited_word_is_picked_out() {
        let (lines, _) = parse_diff(b"diff --git a/f b/f\n@@ -1 +1 @@\n-let total = count + 1;\n+let total = count + 2;\n");
        assert_eq!(&lines[0].text[lines[0].changed.0..lines[0].changed.1], "1");
        assert_eq!(&lines[1].text[lines[1].changed.0..lines[1].changed.1], "2");
    }

    #[test]
    fn unrelated_lines_are_not_paired() {
        let (lines, _) = parse_diff(b"diff --git a/f b/f\n@@ -1 +1 @@\n-alpha\n+zzzzzzzzzz\n");
        assert_eq!(lines[0].changed, (0, 0));
    }

    #[test]
    fn text_is_made_safe_to_draw() {
        assert_eq!(clean("a\tb\x1b[31m"), "a   b[31m");
    }

    #[test]
    fn jumping_wraps_even_when_two_changes_share_the_last_page_top() {
        let mut kinds = [LineKind::Same; 100];
        kinds[9] = LineKind::Del;
        kinds[10] = LineKind::Add;
        kinds[94] = LineKind::Add;
        kinds[96] = LineKind::Del;
        for (top, forward, expected) in [(6, true, 82), (82, true, 6), (6, false, 82), (82, false, 6)] {
            assert_eq!(change_top(kinds, top, 82, forward), Some(expected));
        }
    }

    #[test]
    fn panning_can_reach_the_end_of_a_line_of_wide_characters() {
        let dir = Fixture::folder();
        let file = dir.root.join("text");
        fs::write(&file, format!("{}最後", "界".repeat(16))).unwrap();
        let mut view = View::open(&file).unwrap();
        let mut screen = Screen::new();
        screen.resize(20, 3);
        for _ in 0..4 {
            view.on_key(Key::Right);
            view.draw(&mut screen);
        }
        assert_eq!(view.pan, 22);
        let (skip, text) = view.lines[0].text_for_draw(view.pan, 14);
        assert_eq!(&text[skip..], "界界界界界最後");
    }

    #[test]
    fn a_link_shows_its_path_even_when_the_target_is_gone() {
        let f = Fixture::new();
        let target = f.root.join("../target.txt");
        let link = f.root.join("link");
        fs::write(&target, "outside the project\n").unwrap();
        symlink("../target.txt", &link).unwrap();
        let mut view = View::open(&link).unwrap();
        let rows = drawn(&mut view, 60, 6);
        assert!(rows[0].ends_with("+1"), "{rows:?}");
        assert_eq!(rows[1], " 1 + ../target.txt");
        assert!(!rows.iter().any(|row| row.contains("outside the project")));
        fs::remove_file(target).unwrap();
        let mut view = View::open(&link).unwrap();
        assert_eq!(drawn(&mut view, 60, 6), rows);
    }

    #[test]
    fn an_unreadable_file_is_not_reported_as_gone() {
        let dir = Fixture::folder();
        let file = dir.root.join("text");
        fs::write(&file, "content").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0)).unwrap();
        // Users who can bypass file permissions can still read this file.
        if fs::File::open(&file).is_ok() {
            return;
        }
        let mut view = View::open(&file).unwrap();
        assert_eq!(drawn(&mut view, 60, 6)[2].trim_start(), "This file cannot be read.");
        fs::remove_file(&file).unwrap();
        let mut gone = View::open(&file).unwrap();
        assert_eq!(drawn(&mut gone, 60, 6)[2].trim_start(), "This file is gone.");
    }
}
