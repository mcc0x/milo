//! Shows one file read-only, with code colours and a line diff.

mod content;
mod draw;
#[cfg(test)]
mod tests;

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;

use crate::diff::{Line, LineKind};
use crate::git::{Commit, Kind, PendingFile, Repo};
use crate::help::{self, Page};
use crate::keys::{Direction, Key};
use crate::pane::{After, Pane};
use crate::picture::Picture;
use crate::preferences::{IconMode, Preferences};
use crate::rex;
use crate::screen::{Screen, Style};
use crate::syntax::{Lang, Span, State};
use crate::ui::{ADD, DEL};
use crate::watch::Scope;

/// Lines of context kept above a change when jumping to it.
const LEAD: usize = 3;

/// The file open in a pane and the position from which it is shown.
pub struct Reader {
    source: Source,
    /// The name git uses, relative to the repository root.
    path: String,
    icons: IconMode,
    preference_note: Option<&'static str>,
    /// What happened to the working file.
    kind: Kind,
    body: Body,
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

/// Includes file identity and change time: saving by replacement can preserve length and mtime.
#[derive(PartialEq)]
struct Stamp {
    identity: (u64, u64),
    changed: (i64, i64),
}

impl Stamp {
    fn of(metadata: &Metadata) -> Self {
        Self { identity: (metadata.dev(), metadata.ino()), changed: (metadata.ctime(), metadata.ctime_nsec()) }
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
                *layer = Some(rex::open(&repo.root, path, hash, layer.as_deref())?);
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

    fn from_lines(lines: Result<Vec<Line>, &'static str>, lang: Option<&'static Lang>) -> Self {
        match lines {
            Ok(lines) => Self::Text(Text::new(lines, lang)),
            Err(note) => Self::Note(note),
        }
    }
}

struct Text {
    lines: Vec<Line>,
    lang: Option<&'static Lang>,
    /// Start lines at which the open syntax differs from the preceding line.
    starts: Vec<(usize, State)>,
    /// Start of each block of changed lines, before adding viewing context.
    changes: Vec<usize>,
    digits: usize,
    added: u32,
    removed: u32,
}

impl Text {
    fn new(lines: Vec<Line>, lang: Option<&'static Lang>) -> Self {
        let lang = lang.filter(|lang| {
            lines.iter().find(|line| line.kind != LineKind::Del).is_none_or(|line| lang.agrees_with_shebang(&line.text))
        });
        let mut starts = Vec::new();
        let mut changes = Vec::new();
        let mut state = State::default();
        let (mut last, mut added, mut removed) = (1, 0, 0);
        let mut previous = LineKind::Same;
        for (i, line) in lines.iter().enumerate() {
            last = last.max(line.number);
            match line.kind {
                LineKind::Add => added += 1,
                LineKind::Del => removed += 1,
                LineKind::Same => {}
            }
            if previous == LineKind::Same && line.kind != LineKind::Same {
                changes.push(i);
            }
            previous = line.kind;
            // Removed lines are not part of the file's syntax.
            if let Some(lang) = lang.filter(|_| line.kind != LineKind::Del) {
                lang.advance(&line.text, &mut state);
                let before = starts.last().map_or(&State::Code, |(_, state)| state);
                if &state != before {
                    starts.push((i + 1, state.clone()));
                }
            }
        }
        Self { lines, lang, starts, changes, digits: last.ilog10() as usize + 1, added, removed }
    }

    /// Finds the next or previous block, wrapping and skipping duplicate viewport positions.
    fn change_top(&self, top: usize, last: usize, direction: Direction) -> Option<usize> {
        let at = |&line: &usize| line.saturating_sub(LEAD).min(last);
        let changes = &self.changes;
        match direction {
            Direction::Next => changes.get(changes.partition_point(|line| at(line) <= top)).or(changes.first()),
            Direction::Previous => changes[..changes.partition_point(|line| at(line) < top)].last().or(changes.last()),
        }
        .map(at)
    }
}

impl Reader {
    pub(super) fn working_repo(&self) -> Option<&Repo> {
        match &self.source {
            Source::Working(work) => Some(&work.repo),
            Source::Committed { .. } => None,
        }
    }

    fn new(path: String, source: Source, icons: IconMode, preference_note: Option<&'static str>) -> Self {
        Self {
            source,
            path,
            icons,
            preference_note,
            kind: Kind::None,
            body: Body::Note("Git did not answer."),
            spans: Vec::new(),
            top: 0,
            pan: 0,
            rows: 0,
        }
    }

    fn appearance_for(repo: &Repo) -> (IconMode, Option<&'static str>) {
        if cfg!(test) {
            return (Preferences::default().icons, None);
        }
        let (prefs, note) = Preferences::for_pane(repo.git.then_some(repo.root.as_path()));
        (prefs.icons.effective(), note)
    }

    fn reveal_first_change(&mut self) {
        if let Body::Text(text) = &self.body
            && let Some(first) = text.changes.first()
        {
            self.top = first.saturating_sub(LEAD);
        }
    }

    fn last_top(&self) -> usize {
        self.body.lines().len().saturating_sub(self.rows.max(1))
    }

    fn scroll(&mut self, delta: isize) {
        self.top = self.top.saturating_add_signed(delta).min(self.last_top());
    }

    fn jump(&mut self, direction: Direction) {
        if let Body::Text(text) = &self.body
            && let Some(top) = text.change_top(self.top, self.last_top(), direction)
        {
            self.top = top;
        }
    }
}

impl Pane for Reader {
    fn notice(&self) -> Option<&'static str> {
        self.preference_note
    }

    fn help(&self) -> Page {
        use help::{HelpRow, heading, key, note, sample};

        let mut rows = vec![
            heading("Lines"),
            sample("+", ADD.bold(), "added"),
            sample("-", DEL, "removed"),
            sample("word", Style::PLAIN.reverse(), "what changed"),
            HelpRow::new(),
            heading("Keys"),
            key("↑↓   ", "scroll (k j)"),
            key("Space", "page down"),
            key("b    ", "page up"),
            key("n N  ", "next/prev change"),
            key("←→   ", "scroll (h l)"),
            key("g G  ", "top/bottom"),
            key("q Esc", "close"),
        ];
        if matches!(self.source, Source::Committed { .. }) {
            rows.insert(1, note("As of a commit"));
        }
        Page::new(rows)
    }

    fn watch_scope(&self) -> Scope {
        match &self.source {
            Source::Working(work) => work.repo.watch_scope(),
            Source::Committed { .. } => Scope { roots: Vec::new(), ignored: Default::default(), complete: true },
        }
    }

    fn git_busy(&self) -> bool {
        self.working_repo().is_some_and(Repo::busy)
    }

    fn check(&mut self) -> bool {
        let Source::Working(work) = &mut self.source else { return true };
        if work.repo.poll(&[]) || work.absolute.symlink_metadata().ok().as_ref().map(Stamp::of) != work.stamp {
            self.load();
            self.top = self.top.min(self.last_top());
        }
        true
    }

    fn key(&mut self, key: Key) -> After {
        let key = match key {
            Key::Char(' ') => Key::PgDn,
            Key::Char('b') => Key::PgUp,
            _ => key,
        };
        let before = (self.top, self.pan);
        match key {
            Key::Char('q') | Key::Esc => return After::Leave,
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
        if (self.top, self.pan) == before { After::Ignored } else { After::Stay }
    }

    fn draw(&mut self, s: &mut Screen) {
        self.draw_file(s);
    }
}
