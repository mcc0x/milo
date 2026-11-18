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
    /// Byte range of `text` that differs from its paired line; empty if none.
    changed: (usize, usize),
    /// What was still open in the code where this line begins.
    state: State,
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
            kind = match l {
                // Deleted and then new again is one file, changed.
                _ if l.starts_with("new file mode ") && second => Kind::Modified,
                _ if l.starts_with("new file mode ") => Kind::Added,
                _ if l.starts_with("deleted file mode ") => Kind::Deleted,
                _ if l.starts_with("rename from ") => Kind::Renamed,
                _ => kind,
            };
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
        let text = clean(l.get(1..).unwrap_or(""));
        lines.push(Line { kind: line_kind, no: *no, text, changed: (0, 0), state: State::default() });
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
    pub fn watch(&self) -> (Vec<&Path>, bool) {
        (self.repo.watch_paths(), self.repo.watched_fully())
    }

    fn stamp(&self) -> Stamp {
        let m = std::fs::metadata(&self.abs).ok()?;
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
                self.note = Some("Too large to show.");
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
        match read::file(&self.abs, LIMIT) {
            Content::Gone => self.note = Some("This file is gone."),
            Content::NotFile => self.note = Some("Not a file, so there is nothing to show."),
            Content::TooLarge => self.note = Some("Too large to show."),
            Content::Bytes(bytes) if bytes.contains(&0) => self.note = Some("Not text, so there is nothing to show."),
            Content::Bytes(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                // A file git has never seen is new from its first line to its last.
                let kind = if self.kind == Kind::Untracked { LineKind::Add } else { LineKind::Same };
                let line = |(i, l): (usize, &str)| Line {
                    kind,
                    no: i as u32 + 1,
                    text: clean(l),
                    changed: (0, 0),
                    state: State::default(),
                };
                self.lines = text.lines().enumerate().map(line).collect();
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

    fn scroll(&mut self, delta: isize) {
        let last = self.lines.len().saturating_sub(self.rows.max(1));
        self.top = self.top.saturating_add_signed(delta).min(last);
    }

    /// Jumps to the next or previous run of changed lines.
    fn jump(&mut self, forward: bool) {
        let starts = (0..self.lines.len()).filter(|&i| {
            self.lines[i].kind != LineKind::Same && (i == 0 || self.lines[i - 1].kind == LineKind::Same)
        });
        // A change is "at" the top once its lead-in is the first row shown.
        let at = |i: usize| i.saturating_sub(LEAD);
        // Past the last change comes the first again, as in the outline.
        let hit = if forward {
            let mut tops = starts.map(at);
            let first = tops.next();
            first.filter(|&t| t > self.top).or_else(|| tops.find(|&t| t > self.top)).or(first)
        } else {
            let tops: Vec<usize> = starts.map(at).collect();
            tops.iter().rev().find(|&&t| t < self.top).or(tops.last()).copied()
        };
        if let Some(top) = hit {
            self.top = top;
            self.scroll(0);
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
        let shown = || self.lines.iter().skip(self.top).take(self.rows);
        let longest = shown().map(|l| l.text.chars().count()).max().unwrap_or(0);
        self.pan = self.pan.min(longest.saturating_sub(text_w));
        for (y, line) in (1..).zip(self.lines.iter().skip(self.top).take(self.rows)) {
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
            let skip = line.text.char_indices().nth(self.pan).map_or(line.text.len(), |(i, _)| i);
            s.put(text_x, y, text_w, &line.text[skip..], st);
            // A removed line is not part of the file as it now reads, and
            // is not coloured as code.
            self.spans.clear();
            if let Some(lang) = self.lang.filter(|_| line.kind != LineKind::Del) {
                lang.scan(&line.text, &mut { line.state }, &mut self.spans);
            }
            // Columns run on from one span to the next, so each is measured once.
            let (mut at, mut col) = (skip, 0);
            for &(start, end, class) in &self.spans {
                let (start, end) = (start.max(skip), end.max(skip));
                if start >= end || col >= text_w {
                    continue;
                }
                col += term::str_width(&line.text[at..start]);
                let width = term::str_width(&line.text[start..end]);
                s.paint(text_x + col, y, width.min(text_w.saturating_sub(col)), class_style(class));
                (at, col) = (end, col + width);
            }
            let (a, b) = (line.changed.0.max(skip), line.changed.1.max(skip));
            if a < b {
                let from = term::str_width(&line.text[skip..a]);
                let width = term::str_width(&line.text[a..b]);
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

    fn shape(lines: &[Line]) -> Vec<(LineKind, u32, &str)> {
        lines.iter().map(|l| (l.kind, l.no, l.text.as_str())).collect()
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
}
