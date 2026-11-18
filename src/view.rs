//! `strata view FILE`: one file, read-only, with what changed in it marked.
//!
//! A file with pending changes is shown whole, with the lines it gained and
//! lost in place and the words that changed inside an edited line picked
//! out. Any other file is shown as it is.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::git::{FileChange, Kind, Repo};
use crate::icons;
use crate::term::{self, Key, Screen, Style};
use crate::ui::{self, ADD, DEL, DIM};

/// Files larger than this are not shown.
const LIMIT: u64 = 2 << 20;
const TAB: usize = 4;
/// Lines of the file kept in view above a change that was jumped to.
const LEAD: usize = 3;
/// How often the file is checked for changes, and how soon after the
/// operating system reports one.
const CHECK: Duration = Duration::from_secs(2);
const SETTLE: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, PartialEq)]
enum LineKind {
    Same,
    Add,
    Del,
}

#[derive(PartialEq)]
struct Line {
    kind: LineKind,
    /// Line number in the file: the current one, or the old one for a removal.
    no: u32,
    text: String,
    /// Byte range of `text` that differs from its paired line; empty if none.
    changed: (usize, usize),
}

pub struct View {
    repo: Repo,
    /// Everything pending in the repository, to notice when that moves.
    files: Vec<FileChange>,
    /// The file, relative to the repository root.
    path: String,
    abs: PathBuf,
    stamp: Option<(SystemTime, u64)>,
    lines: Vec<Line>,
    /// Said in place of the content when there is none to show.
    note: Option<&'static str>,
    /// What is pending for this file: its kind, lines gained, lines lost.
    change: Option<(Kind, u32, u32)>,
    top: usize,
    pan: usize,
    /// Rows of content on screen, set by the last draw.
    rows: usize,
    checked: Instant,
    poked: bool,
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
/// place among them.
fn parse_diff(raw: &[u8]) -> Vec<Line> {
    let mut lines = Vec::new();
    let (mut old, mut new, mut in_hunk) = (1, 1, false);
    for l in String::from_utf8_lossy(raw).lines() {
        if let Some(hunk) = l.strip_prefix("@@ -") {
            // "@@ -12,5 +14,6 @@"
            let start = |s: &str| s.split([',', ' ']).next().and_then(|n| n.parse().ok()).unwrap_or(1);
            old = start(hunk);
            new = hunk.split_once(" +").map_or(1, |(_, n)| start(n));
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (kind, no) = match l.as_bytes().first() {
            Some(b'+') => (LineKind::Add, &mut new),
            Some(b'-') => (LineKind::Del, &mut old),
            // "\ No newline at end of file"
            Some(b'\\') => continue,
            _ => {
                old += 1;
                (LineKind::Same, &mut new)
            }
        };
        lines.push(Line { kind, no: *no, text: clean(l.get(1..).unwrap_or("")), changed: (0, 0) });
        *no += 1;
    }
    mark_changed(&mut lines);
    lines
}

impl View {
    /// Opens `file`, which need not exist any more if git remembers it.
    /// `None` if the directory it is in does not exist.
    pub fn open(file: &Path) -> Option<View> {
        let dir = file.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let repo = Repo::open(dir)?;
        let abs = dir.canonicalize().ok()?.join(file.file_name()?);
        let path = abs.strip_prefix(&repo.root).ok()?.to_string_lossy().into_owned();
        let mut view = View {
            repo,
            files: Vec::new(),
            path,
            abs,
            stamp: None,
            lines: Vec::new(),
            note: None,
            change: None,
            top: 0,
            pan: 0,
            rows: 0,
            checked: Instant::now(),
            poked: false,
        };
        view.load();
        // Start at the first change, with a little of the file above it.
        let first = view.lines.iter().position(|l| l.kind != LineKind::Same);
        view.top = first.map_or(0, |i| i.saturating_sub(LEAD));
        Some(view)
    }

    pub fn root(&self) -> &Path {
        &self.repo.root
    }

    fn stamp(&self) -> Option<(SystemTime, u64)> {
        let m = std::fs::metadata(&self.abs).ok()?;
        Some((m.modified().ok()?, m.len()))
    }

    fn load(&mut self) {
        self.files = self.repo.load();
        self.stamp = self.stamp();
        self.checked = Instant::now();
        self.change = self.files.iter().find(|f| f.path == self.path).map(|f| (f.kind, f.add, f.del));
        self.note = None;
        if self.stamp.is_some_and(|(_, len)| len > LIMIT) {
            self.lines.clear();
            self.note = Some("Too large to show.");
            return;
        }
        // Git has nothing to compare a file it does not track against.
        let tracked = self.change.is_some_and(|(kind, ..)| kind != Kind::Untracked);
        let diff = if tracked { self.repo.file_diff(&self.path) } else { Vec::new() };
        self.lines = parse_diff(&diff);
        if !self.lines.is_empty() {
            return;
        }
        match std::fs::read(&self.abs) {
            Err(_) => self.note = Some("This file is gone."),
            Ok(bytes) if bytes.contains(&0) => self.note = Some("Not text, so there is nothing to show."),
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let line = |(i, l): (usize, &str)| Line {
                    kind: LineKind::Same,
                    no: i as u32 + 1,
                    text: clean(l),
                    changed: (0, 0),
                };
                self.lines = text.lines().enumerate().map(line).collect();
                if self.lines.is_empty() {
                    self.note = Some("Empty.");
                }
            }
        }
    }

    /// Looks for changes, at most once per `SETTLE` after being told of one
    /// and once per `CHECK` regardless.
    fn check_if_due(&mut self) {
        let due = if self.poked { SETTLE } else { CHECK };
        if self.checked.elapsed() < due {
            return;
        }
        (self.checked, self.poked) = (Instant::now(), false);
        if self.repo.stale(&self.files) || self.stamp() != self.stamp {
            self.load();
            self.scroll(0);
        }
    }

    pub fn tick(&mut self) {
        self.check_if_due();
    }

    pub fn changed(&mut self) {
        self.poked = true;
        self.check_if_due();
    }

    pub fn idle_ms(&self) -> i32 {
        let due = if self.poked { SETTLE } else { CHECK };
        due.saturating_sub(self.checked.elapsed()).as_millis() as i32 + 1
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
        let hit = if forward {
            starts.map(at).find(|&t| t > self.top)
        } else {
            starts.map(at).filter(|&t| t < self.top).last()
        };
        if let Some(top) = hit {
            self.top = top;
            self.scroll(0);
        }
    }

    pub fn draw(&mut self, s: &mut Screen) {
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
        for (y, line) in (1..).zip(self.lines.iter().skip(self.top).take(self.rows)) {
            let (sign, st, no_st) = match line.kind {
                LineKind::Same => (" ", Style::PLAIN, DIM),
                LineKind::Add => ("+", ADD, ADD),
                LineKind::Del => ("-", DEL, DEL),
            };
            let no = line.no.to_string();
            s.put(1 + digits - no.len(), y, digits, &no, no_st);
            s.put(digits + 2, y, 1, sign, st);
            // Panning skips whole characters off the left of the text.
            let skip = line.text.char_indices().nth(self.pan).map_or(line.text.len(), |(i, _)| i);
            s.put(text_x, y, text_w, &line.text[skip..], st);
            let (a, b) = (line.changed.0.max(skip), line.changed.1.max(skip));
            if a < b {
                let from = term::str_width(&line.text[skip..a]);
                let width = term::str_width(&line.text[a..b]);
                s.emphasise(text_x + from, y, width.min(text_w.saturating_sub(from)));
            }
        }
        let len = self.lines.len();
        if len > self.rows {
            let thumb = (self.rows * self.rows / len).max(1);
            let at = self.top * (self.rows - thumb) / (len - self.rows);
            for y in 1 + at..1 + at + thumb {
                s.put(w - 1, y, 1, "▐", DIM);
            }
        }
        let hint = if self.change.is_some() { "↑↓ scroll · n next change · q close" } else { "↑↓ scroll · q close" };
        s.put_fit(1, h - 1, w - 2, hint, DIM);
    }

    /// The file's name and where it lives, and what is pending for it.
    fn header(&self, s: &mut Screen) {
        let w = s.w;
        let mut right = w - 1;
        if let Some((kind, add, del)) = self.change {
            right -= ui::size_right(s, right, 0, add, del);
            let word = ui::kind_word(kind);
            right = right.saturating_sub(word.len() + 2);
            s.put(right, 0, word.len(), word, ui::kind_style(kind));
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
        match key {
            Key::Char('q') | Key::Esc | Key::Ctrl(b'c') | Key::Eof => return true,
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
