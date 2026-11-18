//! Shows one file read-only, with code colours and a line diff.

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::diff::{self, FileDiff, Line, LineKind};
use crate::git::run::OutputError;
use crate::git::{Commit, CommitId, Kind, PendingFile, Repo, Stamp};
use crate::help::Page;
use crate::icons;
use crate::keys::{Direction, Key};
use crate::picture::{self, Picture};
use crate::read::{self, Content};
use crate::screen::{self, Screen, Style, Wash};
use crate::syntax::{self, Class, Lang, Span, State};
use crate::ui::{self, ADD, DEL, FAINT, Pane};
use crate::watch::Scope;
use crate::width::{byte_at_col, str_width};
use crate::{rex, when};

/// How many bytes to read when there is no diff text to show.
const LIMIT: u64 = 2 << 20;
/// Lines of context kept above a change when jumping to it.
const LEAD: usize = 3;
/// Extra room in the scan limit for a token that crosses the right edge.
const LEAD_OUT: usize = 32;
const NOT_TEXT: &str = "Not text, so there is nothing to show.";

/// The file open in a pane and the position from which it is shown.
pub struct Reader {
    source: Source,
    /// The name git uses, relative to the repository root.
    path: String,
    /// What happened to the working file.
    kind: Kind,
    body: Body,
    lang: Option<&'static Lang>,
    /// Scratch space reused when colouring code.
    spans: Vec<Span>,
    top: usize,
    /// Display columns skipped on the left.
    pan: usize,
    /// Rows of content on screen, set by the last draw.
    rows: usize,
}

/// A working file can change; a commit needs only its name and date.
enum Source {
    Working(Box<WorkingFile>),
    Committed { short_id: String, time: i64 },
}

struct WorkingFile {
    repo: Repo,
    absolute: PathBuf,
    stamp: Option<Stamp>,
}

struct Location {
    repo: Repo,
    absolute: PathBuf,
    path: String,
}

impl Location {
    fn find(file: &Path) -> Option<Self> {
        let dir = file.ancestors().skip(1).find(|dir| dir.is_dir()).unwrap_or(Path::new("."));
        let tail = file.strip_prefix(dir).unwrap_or(file);
        // Resolve the directory, but keep the final component even if it is a link.
        let dir = dir.canonicalize().ok()?;
        let absolute = dir.join(tail);
        let repo = Repo::open(&dir)?;
        let path = absolute.strip_prefix(&repo.root).ok()?.to_string_lossy().into_owned();
        Some(Self { repo, absolute, path })
    }
}

/// A file selected in a pane that can open the reader.
pub enum ReadFile<'a> {
    Working(&'a str),
    Committed { commit: &'a Commit, file: &'a PendingFile },
}

/// Opens a file here, or replaces the last floating reader in Rex.
pub enum OpenReader {
    Here,
    Floating { layer: Option<String> },
}

impl OpenReader {
    pub fn new(inside_rex: bool) -> Self {
        if inside_rex { Self::Floating { layer: None } } else { Self::Here }
    }

    pub fn open(&mut self, repo: &Repo, target: ReadFile<'_>) -> Result<Option<Reader>, &'static str> {
        match self {
            Self::Here => Ok(match target {
                ReadFile::Working(path) => Reader::open(&repo.root.join(path)),
                ReadFile::Committed { commit, file } => {
                    Some(Reader::committed(repo, &file.path, &commit.hash, commit.time, Some(file)))
                }
            }),
            Self::Floating { layer } => {
                let (path, hash) = match target {
                    ReadFile::Working(path) => (path, None),
                    ReadFile::Committed { commit, file } => (file.path.as_str(), Some(commit.hash.as_str())),
                };
                *layer = Some(rex::open(&repo.root, path, hash, layer.take().as_deref())?);
                Ok(None)
            }
        }
    }
}

enum Body {
    Text(Text),
    Note(&'static str),
    Picture(Picture),
}

impl Body {
    fn lines(&self) -> &[Line] {
        match self {
            Self::Text(text) => &text.lines,
            Self::Note(_) | Self::Picture(_) => &[],
        }
    }
}

struct Text {
    lines: Vec<Line>,
    /// What was still open at the start of each line, for files coloured as code.
    starts: Vec<State>,
    digits: usize,
    added: u32,
    removed: u32,
}

impl Text {
    fn new(lines: Vec<Line>, lang: Option<&Lang>, spans: &mut Vec<Span>) -> Self {
        let mut starts = Vec::with_capacity(if lang.is_some() { lines.len() } else { 0 });
        let mut state = State::default();
        let (mut last, mut added, mut removed) = (1, 0, 0);
        for line in &lines {
            last = last.max(line.number);
            match line.kind {
                LineKind::Add => added += 1,
                LineKind::Del => removed += 1,
                LineKind::Same => {}
            }
            if let Some(lang) = lang {
                starts.push(state);
                // Removed lines are not part of the file's syntax.
                if line.kind != LineKind::Del {
                    lang.scan(&line.text, &mut state, spans);
                }
            }
        }
        Self { lines, starts, digits: last.ilog10() as usize + 1, added, removed }
    }
}

/// Reads the file itself when its diff has no lines to show.
fn file_lines(path: &Path, kind: Kind) -> Result<Vec<Line>, &'static str> {
    let line_kind = if kind == Kind::Untracked { LineKind::Add } else { LineKind::Same };
    match read::file(path, LIMIT) {
        Content::Gone => Err("This file is gone."),
        Content::Unreadable => Err("This file cannot be read."),
        Content::NotFile => Err("Not a file, so there is nothing to show."),
        Content::TooLarge => Err("Too large to show."),
        Content::Link(target) => Ok(vec![Line::new(line_kind, 1, &target.to_string_lossy())]),
        Content::Bytes(bytes) => byte_lines(&bytes, line_kind),
    }
}

/// Turns a stored file into text, or says why it cannot be shown.
fn byte_lines(bytes: &[u8], kind: LineKind) -> Result<Vec<Line>, &'static str> {
    if bytes.is_empty() {
        return Err("Empty.");
    }
    if bytes.contains(&0) {
        return Err(NOT_TEXT);
    }
    let text = String::from_utf8_lossy(bytes);
    Ok((1..).zip(text.lines()).map(|(no, line)| Line::new(kind, no, line)).collect())
}

/// Finds the next or previous run of changes, wrapping at either end.
fn change_top(
    kinds: impl IntoIterator<Item = LineKind>,
    top: usize,
    last: usize,
    direction: Direction,
) -> Option<usize> {
    let (mut first, mut final_top, mut previous) = (None, None, None);
    let mut same = true;
    for (i, kind) in kinds.into_iter().enumerate() {
        let starts = same && kind != LineKind::Same;
        same = kind == LineKind::Same;
        if !starts {
            continue;
        }
        let at = i.saturating_sub(LEAD).min(last);
        if direction == Direction::Next && at > top {
            return Some(at);
        }
        if direction == Direction::Previous && at >= top && previous.is_some() {
            return previous;
        }
        first.get_or_insert(at);
        final_top = Some(at);
        if at < top {
            previous = Some(at);
        }
    }
    match direction {
        Direction::Next => first,
        Direction::Previous => previous.or(final_top),
    }
}

/// The byte to draw from and the prefix needed to colour what is shown.
fn text_for_draw(text: &str, pan: usize, width: usize) -> (usize, &str) {
    let skip = byte_at_col(text, pan);
    let end = text.floor_char_boundary(skip + (width + LEAD_OUT) * 4);
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

struct LinePainter<'a> {
    x: usize,
    width: usize,
    pan: usize,
    lang: Option<&'static Lang>,
    spans: &'a mut Vec<Span>,
}

impl LinePainter<'_> {
    fn draw_line(&mut self, s: &mut Screen, y: usize, line: &Line, mut state: State) {
        let (sign, text_style, margin_style) = match line.kind {
            LineKind::Same => (" ", Style::PLAIN, FAINT),
            LineKind::Add if self.lang.is_some() => ("+", Style::PLAIN, ADD.bold()),
            LineKind::Add => ("+", ADD, ADD),
            LineKind::Del => ("-", DEL, DEL),
        };
        if line.kind != LineKind::Del {
            s.put_number(self.x - 3, y, line.number, margin_style);
        }
        s.put(self.x - 2, y, 1, sign, margin_style);
        if self.pan > 0 {
            s.put(self.x - 1, y, 1, "‹", FAINT);
        }
        let (skip, text) = text_for_draw(&line.text, self.pan, self.width);
        s.put(self.x, y, self.width, &text[skip..], text_style);
        if let Some(lang) = self.lang.filter(|_| line.kind != LineKind::Del) {
            lang.scan(text, &mut state, self.spans);
            self.draw_code(s, y, skip, text);
        }
        self.draw_edit(s, y, skip, text, &line.changed);
        match line.kind {
            LineKind::Same => {}
            LineKind::Add => s.wash(0, y, s.w - 1, Wash::Added),
            LineKind::Del => s.wash(0, y, s.w - 1, Wash::Removed),
        }
    }

    fn draw_code(&self, s: &mut Screen, y: usize, skip: usize, text: &str) {
        // Continue from the previous span so each gap is measured only once.
        let (mut at, mut col) = (skip, 0);
        for &(start, end, class) in &*self.spans {
            if end <= skip {
                continue;
            }
            let start = start.max(skip);
            col += str_width(&text[at..start]);
            if col >= self.width {
                break;
            }
            let width = str_width(&text[start..end]);
            s.paint(self.x + col, y, width.min(self.width - col), class_style(class));
            (at, col) = (end, col + width);
        }
    }

    fn draw_edit(&self, s: &mut Screen, y: usize, skip: usize, text: &str, changed: &Range<usize>) {
        let start = changed.start.max(skip);
        let end = changed.end.min(text.len());
        if start < end {
            let from = str_width(&text[skip..start]);
            let width = str_width(&text[start..end]);
            s.emphasise(self.x + from, y, width.min(self.width.saturating_sub(from)));
        }
    }
}

impl Reader {
    /// Opens `file`, which git may remember even if its directory is gone.
    pub fn open(file: &Path) -> Option<Self> {
        let Location { mut repo, absolute, path } = Location::find(file)?;
        repo.only(&path);
        let mut reader = Self::new(path, Source::Working(Box::new(WorkingFile { repo, absolute, stamp: None })));
        reader.load();
        reader.reveal_first_change();
        Some(reader)
    }

    /// The current repository takes precedence over whatever is at the old path today.
    pub fn at_commit(file: &Path, id: CommitId<'_>) -> Result<Self, &'static str> {
        let cwd = std::env::current_dir().map_err(|_| "Cannot find the current directory.")?;
        let absolute = cwd.join(file);
        let (repo, path) = if let Some(repo) = Repo::open(&cwd).filter(|r| r.git)
            && let Ok(tail) = absolute.strip_prefix(&repo.root)
            && tail.components().all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            (repo, tail.to_string_lossy().into_owned())
        } else {
            let location =
                Location::find(file).filter(|l| l.repo.git).ok_or("This file is not in a git repository.")?;
            (location.repo, location.path)
        };
        let commit = repo.resolve_commit(id).ok_or("No commit with that ID.")?;
        let files = repo.files_at(&commit.hash).ok_or("Git did not answer for this commit.")?;
        let changed = files.iter().find(|file| file.path == path);
        Ok(Self::committed(&repo, &path, &commit.hash, commit.time, changed))
    }

    fn new(path: String, source: Source) -> Self {
        let lang = syntax::for_file(&path);
        Self {
            source,
            path,
            kind: Kind::None,
            body: Body::Note("Git did not answer."),
            lang,
            spans: Vec::new(),
            top: 0,
            pan: 0,
            rows: 0,
        }
    }

    fn committed(repo: &Repo, path: &str, hash: &str, time: i64, file: Option<&PendingFile>) -> Self {
        let short_id = hash[..hash.len().min(7)].to_string();
        let mut reader = Self::new(path.to_string(), Source::Committed { short_id, time });
        reader.body = reader.text_or_note(committed_lines(repo, path, hash, file));
        reader.reveal_first_change();
        reader
    }

    fn reveal_first_change(&mut self) {
        let first = self.body.lines().iter().position(|line| line.kind != LineKind::Same);
        self.top = first.map_or(0, |i| i.saturating_sub(LEAD));
    }

    fn text_or_note(&mut self, lines: Result<Vec<Line>, &'static str>) -> Body {
        match lines {
            Ok(lines) => Body::Text(Text::new(lines, self.lang, &mut self.spans)),
            Err(note) => Body::Note(note),
        }
    }

    /// Rebuilds the working file on screen, keeping the last good result if git fails.
    fn load(&mut self) {
        let Source::Working(work) = &mut self.source else { return };
        work.repo.read();
        let stamp = Stamp::at(&work.absolute);
        if !work.repo.ok() {
            return;
        }
        let (kind, lines) = match work.file_diff(&self.path) {
            Some(FileDiff { lines, kind, .. }) => (kind, Ok(lines)),
            None if work.repo.ok() => {
                let note = if matches!(read::file(&work.absolute, LIMIT), Content::Unreadable) {
                    "This file cannot be read."
                } else {
                    "Too large to show."
                };
                (Kind::None, Err(note))
            }
            None => return,
        };
        let picture = picture::is_picture(&self.path);
        let kept = lines.is_ok() && picture && stamp == work.stamp && matches!(self.body, Body::Picture(_));
        self.kind = kind;
        work.stamp = stamp;
        if kept {
            return;
        }
        self.body = match lines {
            Ok(lines) if lines.is_empty() || picture => {
                // A link is shown as where it points, never as what is there.
                if picture && !work.absolute.is_symlink() {
                    Picture::open(&work.absolute).map_or_else(Body::Note, Body::Picture)
                } else {
                    let lines = file_lines(&work.absolute, kind);
                    self.text_or_note(lines)
                }
            }
            lines => self.text_or_note(lines),
        };
    }

    fn last_top(&self) -> usize {
        self.body.lines().len().saturating_sub(self.rows.max(1))
    }

    fn scroll(&mut self, delta: isize) {
        self.top = self.top.saturating_add_signed(delta).min(self.last_top());
    }

    fn jump(&mut self, direction: Direction) {
        let kinds = self.body.lines().iter().map(|line| line.kind);
        if let Some(top) = change_top(kinds, self.top, self.last_top(), direction) {
            self.top = top;
        }
    }
}

impl WorkingFile {
    fn file_diff(&mut self, path: &str) -> Option<FileDiff> {
        let kind = if self.repo.is_untracked(path) { Kind::Untracked } else { Kind::None };
        if kind == Kind::Untracked || !self.repo.git {
            return Some(FileDiff { lines: Vec::new(), kind, binary: false });
        }
        let mut diff = diff::parse(&self.repo.file_diff(path, None)?);
        // A file that looks new may have been moved here; compare it with its old path.
        if diff.kind == Kind::Added
            && let Some(from) = self.repo.moved_from(path)
            && let Some(raw) = self.repo.file_diff(path, Some(&from))
        {
            diff = diff::parse(&raw);
        }
        if self.repo.is_unmerged(path) {
            diff.kind = Kind::Conflict;
        }
        self.repo.ok().then_some(diff)
    }
}

/// A patch covers all the lines when there are edits. Otherwise, read the stored file.
fn committed_lines(repo: &Repo, path: &str, hash: &str, file: Option<&PendingFile>) -> Result<Vec<Line>, &'static str> {
    if picture::is_picture(path) {
        return Err(NOT_TEXT);
    }
    if let Some(file) = file {
        let raw = repo.file_diff_at(hash, file).map_err(|err| match err {
            OutputError::TooLarge => "Too large to show.",
            OutputError::Failed => "Git did not answer for this file.",
        })?;
        if raw.contains(&0) {
            return Err(NOT_TEXT);
        }
        let patch = diff::parse(&raw);
        if !patch.lines.is_empty() {
            return Ok(patch.lines);
        }
        if patch.binary {
            return Err(NOT_TEXT);
        }
        if file.kind == Kind::Deleted {
            return Err("Empty.");
        }
    }
    let raw = repo.blob_at(hash, path).map_err(|err| match err {
        OutputError::TooLarge => "Too large to show.",
        OutputError::Failed => "There is no file here in this commit.",
    })?;
    byte_lines(&raw, LineKind::Same)
}

impl Pane for Reader {
    fn help(&self) -> Page {
        Page::reader(matches!(self.source, Source::Committed { .. }))
    }

    fn watch_scope(&self) -> Scope {
        match &self.source {
            Source::Working(work) => work.repo.watch_scope(),
            Source::Committed { .. } => Scope { roots: Vec::new(), ignored: Vec::new(), complete: true },
        }
    }

    fn check(&mut self) -> bool {
        let Source::Working(work) = &mut self.source else { return true };
        if work.repo.busy() {
            return false;
        }
        if work.repo.poll(&[]) || (work.repo.ok() && Stamp::at(&work.absolute) != work.stamp) {
            self.load();
            self.top = self.top.min(self.last_top());
        }
        true
    }

    fn key(&mut self, key: Key) -> bool {
        let key = match key {
            Key::Char(' ') => Key::PgDn,
            Key::Char('b') => Key::PgUp,
            _ => key,
        };
        match key {
            Key::Esc => return true,
            Key::Char('n') => self.jump(Direction::Next),
            Key::Char('N') => self.jump(Direction::Previous),
            Key::Char('l') | Key::Right => self.pan += 8,
            Key::Char('h') | Key::Left => self.pan = self.pan.saturating_sub(8),
            _ => {
                if let Some(rows) = key.rows(self.rows) {
                    self.scroll(rows);
                }
            }
        }
        false
    }

    fn draw(&mut self, s: &mut Screen) {
        s.clear();
        let (w, h) = (s.w, s.h);
        if w < 20 || h < 3 {
            return;
        }
        self.rows = h - 2;
        self.top = self.top.min(self.last_top());
        self.draw_heading(s);
        match &self.body {
            Body::Note(note) => {
                s.put_fit(2, 2, w - 4, note, FAINT);
                self.pan = 0;
            }
            Body::Picture(picture) => {
                self.pan = 0;
                // The picture sits in the middle of the space between
                // heading and hint, with a column clear on either side.
                let (room_w, room_h) = (w - 2, self.rows);
                match s.pictures() {
                    Some(true) => {
                        let (cols, rows) = picture.fit(s.cell_size(), room_w, room_h);
                        s.show_picture(picture, 1 + (room_w - cols) / 2, 1 + (room_h - rows) / 2, cols, rows);
                    }
                    Some(false) => {
                        s.put_fit(2, 2, w - 4, "This terminal cannot show pictures.", FAINT);
                    }
                    // The terminal has not yet said; it will in a moment.
                    None => {}
                }
            }
            Body::Text(text) => {
                let lines = &text.lines[self.top..];
                let shown = &lines[..lines.len().min(self.rows)];
                let x = text.digits + 4;
                let width = w - 1 - x;
                if self.pan > 0 {
                    let longest = shown.iter().map(|line| line.width).max().unwrap_or(0);
                    self.pan = self.pan.min(longest.saturating_sub(width));
                }
                let mut painter = LinePainter { x, width, pan: self.pan, lang: self.lang, spans: &mut self.spans };
                for (i, line) in shown.iter().enumerate() {
                    let state = text.starts.get(self.top + i).copied().unwrap_or_default();
                    painter.draw_line(s, i + 1, line, state);
                }
                ui::thumb(s, w - 1, 1, self.rows, text.lines.len(), self.top);
            }
        }
        self.draw_hint(s);
    }
}

impl Reader {
    fn draw_hint(&self, s: &mut Screen) {
        let changes = matches!(&self.body, Body::Text(text)
            if text.added + text.removed > 0 && text.lines.len() > (text.added + text.removed) as usize);
        let committed = matches!(self.source, Source::Committed { .. });
        let still = matches!(self.body, Body::Picture(_)) || (committed && !matches!(self.body, Body::Text(_)));
        let action = match (changes, committed) {
            (true, true) => "n N changes",
            (true, false) => "n next change",
            (false, _) => "",
        };
        let middle: &[_] = if still { &["q close"] } else { &["↑↓ scroll", "q close"] };
        crate::help::draw_hint(s, action, middle, "? help");
    }

    /// Keeps the file name ahead of its directory so the name gets the space first.
    fn draw_heading(&self, s: &mut Screen) {
        let mut right = s.w - 1;
        if let Source::Committed { short_id, time } = &self.source {
            let age = when::short_age(when::now().saturating_sub(*time));
            let ago = if age == "now" { "" } else { " ago" };
            let summary = format!("commit {short_id} · {age}{ago}");
            let summary =
                if str_width(&summary) <= right.saturating_sub(6) { &summary } else { &summary[..7 + short_id.len()] };
            right -= s.put_right(right, 0, summary, FAINT);
        } else if let Body::Picture(picture) = &self.body {
            let size = format!("{}×{}", picture.width, picture.height);
            right -= s.put_right(right, 0, &size, FAINT).min(right);
        }
        if matches!(self.source, Source::Working(_)) && self.kind != Kind::None {
            if let Body::Text(text) = &self.body {
                right -= ui::size_right(s, right, 0, text.added, text.removed).min(right);
            }
            let word = ui::kind_word(self.kind);
            let end = right.saturating_sub(2);
            s.put_right(end.max(word.len()), 0, word, ui::kind_colour(self.kind));
            right = end.saturating_sub(word.len());
        }
        let (dir, name) = self.path.rsplit_once('/').unwrap_or(("", &self.path));
        let room = right.saturating_sub(2);
        let mut x = 1 + ui::icon(s, 1, 0, room, icons::file(name));
        x += s.put_fit(x, 0, room.saturating_sub(x), name, Style::PLAIN.bold());
        s.put_fit(x + 2, 0, room.saturating_sub(x + 2), dir, FAINT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::fs::{self, Permissions};
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn drawn(reader: &mut Reader, w: usize, h: usize) -> Vec<String> {
        let mut screen = Screen::sized(w, h);
        reader.draw(&mut screen);
        screen.text()
    }

    #[test]
    fn reader_hints_say_close_and_keep_help_last() {
        let f = Fixture::folder();
        f.write("page.txt", "a file\n");
        let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
        assert_eq!(drawn(&mut reader, 40, 5)[4], " ↑↓ scroll · q close · ? help");
        assert_eq!(drawn(&mut reader, 20, 5)[4], " q close · ? help");
    }

    #[test]
    fn committed_pictures_and_other_binary_files_have_a_plain_note() {
        let f = Fixture::new();
        fs::write(f.root.join("logo.png"), Picture::sample_png(64, 32)).unwrap();
        fs::write(f.root.join("data.bin"), b"\0one\0two").unwrap();
        f.commit("binary files");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let commits = repo.history(1).unwrap();
        let id = CommitId::parse(&commits[0].hash).unwrap();
        f.write("logo.png", "not the saved picture");
        f.write("data.bin", "not the saved data");
        for file in ["logo.png", "data.bin"] {
            let mut reader = Reader::at_commit(&f.root.join(file), id).unwrap();
            let mut screen = Screen::sized(45, 8);
            screen.set_pictures(true);
            reader.draw(&mut screen);
            assert!(screen.picture_at().is_none());
            assert_eq!(screen.text()[2], format!("  {NOT_TEXT}"));
        }
    }

    #[test]
    fn both_jump_keys_work_in_a_committed_file() {
        let f = Fixture::new();
        let file = |changed| {
            (1..=60)
                .map(|n| format!("row {n}{}\n", if changed && matches!(n, 8 | 45) { " edited" } else { "" }))
                .collect::<String>()
        };
        f.write("page.txt", &file(false));
        f.commit("original");
        f.write("page.txt", &file(true));
        f.commit("edit two lines");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let commits = repo.history(1).unwrap();
        let id = CommitId::parse(&commits[0].hash).unwrap();
        let mut reader = Reader::at_commit(&f.root.join("page.txt"), id).unwrap();
        for (line, key) in [(8, 'n'), (45, 'N'), (8, 'n')] {
            let rows = drawn(&mut reader, 45, 12);
            assert!(rows.iter().any(|row| row.contains(&format!("+ row {line} edited"))), "{rows:?}");
            reader.on_key(Key::Char(key));
        }
    }

    #[test]
    fn the_reader_counts_added_and_removed_lines_and_numbers_only_the_current_file() {
        let f = Fixture::new();
        f.write("file.txt", "one\nbefore\nfour\n");
        f.commit("first");
        f.write("file.txt", "one\nafter\nthree\nfour\n");

        let mut reader = Reader::open(&f.root.join("file.txt")).unwrap();
        let rows = drawn(&mut reader, 40, 8);
        assert!(rows[0].ends_with("+2 −1"), "{rows:?}");
        assert_eq!(&rows[1..6], [" 1   one", "   - before", " 2 + after", " 3 + three", " 4   four"]);
    }

    #[test]
    fn line_numbers_align_when_they_have_different_widths() {
        let f = Fixture::folder();
        f.write("file.txt", &"line\n".repeat(10));

        let mut reader = Reader::open(&f.root.join("file.txt")).unwrap();
        let rows = drawn(&mut reader, 40, 12);
        assert_eq!(&rows[9..11], ["  9   line", " 10   line"]);
    }

    #[test]
    fn every_line_of_an_untracked_file_is_added() {
        let f = Fixture::new();
        f.write("file.txt", "first\nlast without a newline");

        let mut reader = Reader::open(&f.root.join("file.txt")).unwrap();
        let rows = drawn(&mut reader, 45, 6);
        assert!(rows[0].contains("new") && rows[0].ends_with("+2"), "{rows:?}");
        assert_eq!(&rows[1..3], [" 1 + first", " 2 + last without a newline"]);
    }

    #[test]
    fn the_reader_opens_a_deleted_file_in_a_deleted_directory() {
        let f = Fixture::new();
        f.write("sub/file.txt", "removed content\n");
        f.commit("first");
        fs::remove_dir_all(f.root.join("sub")).unwrap();

        let mut reader = Reader::open(&f.root.join("sub/file.txt")).unwrap();
        let rows = drawn(&mut reader, 60, 6);
        assert!(rows[0].contains("removed") && rows[0].ends_with("−1"), "{rows:?}");
        assert_eq!(rows[1], "   - removed content");
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

        let mut reader = Reader::open(&f.root.join("file.txt")).unwrap();
        for n in [10, 95, 10, 95] {
            let rows = drawn(&mut reader, 45, 22);
            let change = format!("+ line {n} edited");
            assert!(rows.iter().any(|row| row.contains(&change)), "{rows:?}");
            reader.on_key(Key::Char('n'));
        }
    }

    #[test]
    fn jumping_wraps_even_when_two_changes_share_the_last_page_top() {
        let mut kinds = [LineKind::Same; 100];
        kinds[9] = LineKind::Del;
        kinds[10] = LineKind::Add;
        kinds[94] = LineKind::Add;
        kinds[96] = LineKind::Del;
        use Direction::{Next, Previous};
        for (top, direction, expected) in [(6, Next, 82), (82, Next, 6), (6, Previous, 82), (82, Previous, 6)] {
            assert_eq!(change_top(kinds, top, 82, direction), Some(expected));
        }
    }

    #[test]
    fn panning_can_reach_the_end_of_a_line_of_wide_characters() {
        let dir = Fixture::folder();
        let file = dir.root.join("text");
        fs::write(&file, format!("{}最後", "界".repeat(16))).unwrap();
        let mut reader = Reader::open(&file).unwrap();
        let mut screen = Screen::new();
        screen.resize(20, 3);
        for _ in 0..4 {
            reader.on_key(Key::Right);
            reader.draw(&mut screen);
        }
        assert_eq!(reader.pan, 22);
        let (skip, text) = text_for_draw(&reader.body.lines()[0].text, reader.pan, 14);
        assert_eq!(&text[skip..], "界界界界界最後");
    }

    #[test]
    fn a_link_shows_its_path_even_when_the_target_is_gone() {
        let f = Fixture::new();
        let target = f.root.join("../target.txt");
        let link = f.root.join("link");
        fs::write(&target, "outside the project\n").unwrap();
        symlink("../target.txt", &link).unwrap();
        let mut reader = Reader::open(&link).unwrap();
        let rows = drawn(&mut reader, 60, 6);
        assert!(rows[0].ends_with("+1"), "{rows:?}");
        assert_eq!(rows[1], " 1 + ../target.txt");
        assert!(!rows.iter().any(|row| row.contains("outside the project")));
        fs::remove_file(target).unwrap();
        let mut reader = Reader::open(&link).unwrap();
        assert_eq!(drawn(&mut reader, 60, 6), rows);
    }

    #[test]
    fn a_failed_git_read_does_not_replace_the_file_on_screen() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        let mut reader = Reader::open(&f.root.join("page.txt")).unwrap();
        let before = drawn(&mut reader, 60, 6);
        let index = f.root.join(".git/index");
        let saved = fs::read(&index).unwrap();
        f.write(".git/index", "corrupt index");
        f.write("page.txt", "edited while git is not answering\n");
        assert!(reader.check());
        let Source::Working(work) = &reader.source else { unreachable!() };
        assert!(!work.repo.ok());
        assert_eq!(drawn(&mut reader, 60, 6), before);

        fs::write(index, saved).unwrap();
        assert!(reader.check());
        let Source::Working(work) = &reader.source else { unreachable!() };
        assert!(work.repo.ok());
        assert!(drawn(&mut reader, 60, 6).iter().any(|row| row.contains("edited while git is not answering")));
    }

    #[test]
    fn a_picture_is_shown_in_the_middle_at_its_own_size_where_the_terminal_can() {
        let dir = Fixture::folder();
        fs::write(dir.root.join("logo.png"), Picture::sample_png(64, 32)).unwrap();
        let mut reader = Reader::open(&dir.root.join("logo.png")).unwrap();
        let mut screen = Screen::sized(40, 10);

        // Until the terminal has said, nothing is claimed either way.
        reader.draw(&mut screen);
        assert!(screen.picture_at().is_none() && screen.text()[2].is_empty());
        assert!(screen.text()[0].ends_with("64×32"));

        // 64 by 32 pixels in cells of 8 by 16 is 8 columns by 2 rows.
        screen.set_pictures(true);
        reader.draw(&mut screen);
        assert_eq!(screen.picture_at(), Some((16, 4, 8, 2)));

        let mut plain = Screen::sized(40, 10);
        plain.answered();
        reader.draw(&mut plain);
        assert!(plain.picture_at().is_none());
        assert_eq!(plain.text()[2], "  This terminal cannot show pictures.");
    }

    #[test]
    fn a_link_named_like_a_picture_is_shown_as_where_it_points() {
        let dir = Fixture::folder();
        fs::write(dir.root.join("../outside.png"), Picture::sample_png(8, 8)).unwrap();
        symlink("../outside.png", dir.root.join("logo.png")).unwrap();
        let mut reader = Reader::open(&dir.root.join("logo.png")).unwrap();
        let mut screen = Screen::sized(40, 6);
        screen.set_pictures(true);
        reader.draw(&mut screen);
        assert!(screen.picture_at().is_none());
        assert_eq!(screen.text()[1], " 1   ../outside.png");
        fs::remove_file(dir.root.join("../outside.png")).unwrap();
    }

    #[test]
    fn an_unreadable_file_is_not_reported_as_gone() {
        let dir = Fixture::folder();
        let file = dir.root.join("text");
        fs::write(&file, "content").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o0)).unwrap();
        // Users who can bypass file permissions can still read this file.
        if fs::File::open(&file).is_ok() {
            return;
        }
        let mut reader = Reader::open(&file).unwrap();
        assert_eq!(drawn(&mut reader, 60, 6)[2].trim_start(), "This file cannot be read.");
        fs::remove_file(&file).unwrap();
        let mut gone = Reader::open(&file).unwrap();
        assert_eq!(drawn(&mut gone, 60, 6)[2].trim_start(), "This file is gone.");
    }
}
