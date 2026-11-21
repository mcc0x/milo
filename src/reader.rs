//! Shows one file with code colours and a line diff, and edits a working text file when asked.

mod buffer;
mod content;
mod disk;
mod draw;
mod edit;
#[cfg(test)]
mod edit_tests;
mod rescue;
#[cfg(test)]
mod tests;
mod working_path;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::diff::{Line, LineKind};
use crate::git::{Commit, Kind, PendingFile, Repo};
use crate::help::{self, Page};
use crate::keys::{Direction, Key};
use crate::pane::{After, Pane};
use crate::picture::Picture;
use crate::preferences::{IconMode, Preferences};
use crate::rex;
use crate::screen::{Screen, Style};
use crate::syntax::{self, Lang, Span, State};
use crate::ui::{ADD, DEL};
use crate::watch::{Ignored, Scope};
use disk::{ReadOnly, SaveError};
use edit::{Editor, Place, Step};
pub use rescue::take_note as take_rescue_note;

/// Lines of context kept above a change when jumping to it.
const LEAD: usize = 3;
const CHANGE_NOTICE: Duration = Duration::from_secs(4);

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
    /// Reveal the first change once the viewport has room for the file.
    opening: bool,
    top: usize,
    /// The current change can share its viewport top with other changes.
    current_change: Option<usize>,
    change_notice: Option<(JumpNotice, Instant)>,
    /// Display columns skipped on the left.
    pan: usize,
    /// Rows of content on screen, set by the last draw.
    rows: usize,
    /// Columns of text on screen, excluding the gutter and scrollbar.
    columns: usize,
    /// Furthest horizontal offset of the last drawn viewport.
    pan_limit: usize,
    /// Why the file cannot be edited, as far as can be told without opening it.
    read_only: Option<ReadOnly>,
    /// The draft, while the file is being edited.
    editor: Option<Box<Editor>>,
    /// Milo's state folder, where a draft is kept if the reader is closed from outside.
    state: Option<PathBuf>,
    /// The folders searched for a formatter, as `PATH` lists them.
    tools: Option<std::ffi::OsString>,
}

/// A short message in place of the hints, or of the details in a reader too short for hints.
#[derive(Clone, Copy)]
enum JumpNotice {
    At {
        number: usize,
        total: usize,
    },
    NoChanges,
    /// Words to show in full where they fit, and without `lead` where they do not.
    Said {
        lead: &'static str,
        text: &'static str,
    },
}

/// A working file can change; a commit needs only its name and date.
enum Source {
    Working(Box<WorkingFile>),
    Committed { short_id: String, time: i64 },
}

struct WorkingFile {
    repo: Repo,
    root: working_path::Root,
    relative: PathBuf,
    watch_only: Arc<Ignored>,
    /// Ancestor links can lead to entries elsewhere in the worktree.
    watch_all: Arc<Ignored>,
    attributes: content::Attributes,
    notified: bool,
    stamp: Option<content::WorkingStamp>,
}

/// A file selected in a pane that can open the reader.
pub enum ReadFile<'a> {
    Working(&'a str),
    Committed { commit: &'a Commit, file: &'a PendingFile },
}

/// Opens a file here, or replaces the last floating reader in Rex.
pub enum OpenReader {
    Here,
    Floating { block: Option<String> },
}

impl OpenReader {
    pub fn new(inside_rex: bool) -> Self {
        if inside_rex { Self::Floating { block: None } } else { Self::Here }
    }

    pub fn open(&mut self, repo: &Repo, target: ReadFile<'_>) -> Result<Option<Reader>, &'static str> {
        match self {
            Self::Here => Ok(match target {
                ReadFile::Working(path) => Reader::open(&repo.root.join(path)),
                ReadFile::Committed { commit, file } => {
                    Some(Reader::committed(repo, &file.path, &commit.hash, commit.time, Some(file)))
                }
            }),
            Self::Floating { block } => {
                // Replacing the reader would end it and leave its edits only as a kept draft.
                if let (Some(open), Some(state)) = (block.as_deref(), rescue::state_dir())
                    && rescue::block_has_draft(&state, open)
                {
                    return Err("The open reader has unsaved edits.");
                }
                let (path, hash) = match target {
                    ReadFile::Working(path) => (path, None),
                    ReadFile::Committed { commit, file } => (file.path.as_str(), Some(commit.hash.as_str())),
                };
                *block = Some(rex::open(&repo.root, path, hash, block.as_deref())?);
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
            opening: true,
            top: 0,
            current_change: None,
            change_notice: None,
            pan: 0,
            rows: 0,
            columns: 0,
            pan_limit: 0,
            read_only: None,
            editor: None,
            state: rescue::state_dir(),
            tools: std::env::var_os("PATH"),
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
            self.current_change = Some(0);
            self.top = first.saturating_sub(self.lead());
        }
    }

    fn lead(&self) -> usize {
        LEAD.min(self.rows.saturating_sub(1))
    }

    fn last_top(&self) -> usize {
        self.body.lines().len().saturating_sub(self.rows.max(1))
    }

    /// Forgets the position among the changes. What was said about the file itself stays its full time.
    fn clear_change(&mut self) {
        self.current_change = None;
        if !matches!(self.change_notice, Some((JumpNotice::Said { .. }, _))) {
            self.change_notice = None;
        }
    }

    fn scroll(&mut self, delta: isize) {
        let top = self.top.saturating_add_signed(delta).min(self.last_top());
        if top != self.top {
            self.top = top;
            self.clear_change();
        }
    }

    fn jump(&mut self, direction: Direction, now: Instant) -> After {
        let notice = if let Body::Text(text) = &self.body
            && !text.changes.is_empty()
        {
            let total = text.changes.len();
            let (lead, last) = (self.lead(), self.last_top());
            let at = |line: &usize| line.saturating_sub(lead).min(last);
            let index = match direction {
                Direction::Next => {
                    let next = self
                        .current_change
                        .map_or_else(|| text.changes.partition_point(|line| at(line) <= self.top), |i| i + 1);
                    if next == total { 0 } else { next }
                }
                Direction::Previous => {
                    let before =
                        self.current_change.unwrap_or_else(|| text.changes.partition_point(|line| at(line) < self.top));
                    before.checked_sub(1).unwrap_or(total - 1)
                }
            };
            self.opening = false;
            self.current_change = Some(index);
            self.top = at(&text.changes[index]);
            JumpNotice::At { number: index + 1, total }
        } else {
            if let Some((JumpNotice::NoChanges, until)) = self.change_notice
                && now < until
            {
                return After::Ignored;
            }
            JumpNotice::NoChanges
        };
        self.change_notice = Some((notice, now + CHANGE_NOTICE));
        After::Stay
    }

    fn pan(&mut self, direction: Direction) {
        if self.columns == 0 {
            return;
        }
        // Leave an overlapping column for a wide glyph clipped at the right edge.
        let step = self.columns.saturating_sub(1).clamp(1, 8);
        self.pan = match direction {
            Direction::Next => self.pan.saturating_add(step).min(self.pan_limit),
            Direction::Previous => self.pan.saturating_sub(step),
        };
    }
}

impl Pane for Reader {
    fn notice(&self) -> Option<&'static str> {
        self.preference_note
    }

    fn help(&self) -> Page {
        use help::{HelpRow, heading, key, note, sample};

        if let Some(editor) = &self.editor {
            return editor.help();
        }
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
            HelpRow::new(),
            heading("Editing"),
        ];
        match self.read_only {
            None => {
                let name = self.path.rsplit('/').next().unwrap_or_default();
                rows.extend([key("e    ", "edit this file"), note("Nothing is written"), note("until you save.")]);
                rows.extend(edit::format_help(name, self.tools.as_deref()));
            }
            Some(ReadOnly(why)) => rows.extend([note("Read-only:"), note(why)]),
        }
        if matches!(self.source, Source::Committed { .. }) {
            rows.insert(1, note("As of a commit"));
        }
        Page::new(rows)
    }

    fn editing(&self) -> bool {
        self.editor.is_some()
    }

    fn paste(&mut self, text: Option<&str>) -> After {
        let now = Instant::now();
        match &mut self.editor {
            Some(editor) => {
                let step = editor.paste(text, now);
                self.apply(step, now)
            }
            None => After::Ignored,
        }
    }

    fn watch_scope(&self) -> Scope {
        match &self.source {
            Source::Working(work) if self.editor.is_none() => {
                if work.stamp.as_ref().is_some_and(|stamp| !stamp.at_root_path) {
                    // Keep polling the held directory, not the possibly
                    // unrelated directory now at its old pathname.
                    return Scope { roots: Vec::new(), ignored: Arc::clone(&work.watch_all), complete: false };
                }
                let mut scope = work.repo.watch_scope();
                scope.ignored = Arc::clone(if work.stamp.as_ref().is_some_and(|stamp| stamp.linked_ancestor) {
                    &work.watch_all
                } else {
                    &work.watch_only
                });
                scope
            }
            _ => Scope { roots: Vec::new(), ignored: Default::default(), complete: true },
        }
    }

    fn watch_scope_after_input(&self, current: &Scope) -> Option<Scope> {
        let idle = current.complete && current.roots.is_empty();
        (matches!(self.source, Source::Working(_)) && idle != self.editor.is_some()).then(|| self.watch_scope())
    }

    fn git_busy(&self) -> bool {
        let Source::Working(work) = &self.source else { return false };
        self.editor.is_none() && work.repo.busy() && work.root.at_path(&work.repo.root) == Ok(true)
    }

    fn filesystem_changed(&mut self) {
        if self.editor.is_none()
            && let Source::Working(work) = &mut self.source
        {
            // Attribute changes can affect the patch without changing porcelain status.
            work.notified = true;
        }
    }

    fn check(&mut self) -> bool {
        // Nothing on disk changes the draft view. Saving checks the held file
        // independently, and closing the editor refreshes the diff.
        if self.editor.is_some() {
            return true;
        }
        let force = match &mut self.source {
            Source::Working(work) => std::mem::take(&mut work.notified),
            Source::Committed { .. } => false,
        };
        self.refresh(force);
        self.top = self.top.min(self.last_top());
        true
    }

    fn next_frame(&self) -> Option<Duration> {
        let now = Instant::now();
        match &self.editor {
            Some(editor) => editor.next_frame(now),
            None => self.change_notice.map(|(_, until)| until.saturating_duration_since(now)),
        }
    }

    fn key(&mut self, key: Key) -> After {
        self.input(key, Instant::now())
    }

    fn draw(&mut self, s: &mut Screen) {
        self.draw_file(s, Instant::now());
    }
}

impl Reader {
    fn input(&mut self, key: Key, now: Instant) -> After {
        if let Some(editor) = &mut self.editor {
            let step = editor.key(key, now);
            return self.apply(step, now);
        }
        let key = match key {
            Key::Char(' ') => Key::PgDn,
            Key::Char('b') => Key::PgUp,
            _ => key,
        };
        let before = (self.top, self.pan);
        match key {
            Key::Char('q') | Key::Esc => return After::Leave,
            Key::Char('e') => return self.edit(now),
            Key::Char('n') => return self.jump(Direction::Next, now),
            Key::Char('N') => return self.jump(Direction::Previous, now),
            Key::Char('l') | Key::Right => self.pan(Direction::Next),
            Key::Char('h') | Key::Left => self.pan(Direction::Previous),
            _ => {
                if let Some(rows) = key.rows(self.rows) {
                    self.scroll(rows);
                }
            }
        }
        if (self.top, self.pan) == before { After::Ignored } else { After::Stay }
    }

    /// Starts editing the file, or says why it is read-only.
    pub fn edit(&mut self, now: Instant) -> After {
        match self.open_editor() {
            Ok(editor) => {
                self.editor = Some(Box::new(editor));
                self.clear_change();
            }
            Err(ReadOnly(why)) => {
                let notice = JumpNotice::Said { lead: "Read-only: ", text: why };
                self.change_notice = Some((notice, now + CHANGE_NOTICE));
            }
        }
        After::Stay
    }

    /// Reads the file as it is now, which is what gets edited, not the lines on screen.
    fn open_editor(&mut self) -> Result<Editor, ReadOnly> {
        // What is shown decides part of the answer, so bring it up to date first.
        self.refresh(false);
        if let Some(why) = self.read_only {
            return Err(why);
        }
        let Source::Working(work) = &self.source else { return Err(ReadOnly::COMMIT) };
        let (entry, stamp) = content::WorkingStamp::at(&work.repo, &work.root, &work.relative);
        let entry = stamp.reachable().and(entry.map_err(ReadOnly::of))?;
        let original = disk::load(&entry)?;
        let place = Place {
            file: work.repo.root.join(&work.relative),
            root: work.repo.root.clone(),
            state: self.state.clone(),
            block: std::env::var("REX_BLOCK").ok().filter(|_| !cfg!(test) && rex::inside()),
            tools: self.tools.clone(),
        };
        Ok(Editor::open(original, syntax::for_file(&self.path), self.top_line(), place))
    }

    /// Whether the file may be edited, as far as its place and what is shown of it can say.
    fn edit_refusal(&self) -> Option<ReadOnly> {
        let Source::Working(work) = &self.source else { return Some(ReadOnly::COMMIT) };
        match &self.body {
            Body::Note(note) if *note == content::NOT_TEXT => return Some(ReadOnly::NOT_TEXT),
            Body::Picture(_) => return Some(ReadOnly::PICTURE),
            _ => {}
        }
        let (entry, stamp) = content::WorkingStamp::at(&work.repo, &work.root, &work.relative);
        let entry = match stamp.reachable().and(entry.map_err(ReadOnly::of)) {
            Ok(entry) => entry,
            Err(why) => return Some(why),
        };
        if entry.is_link() {
            Some(ReadOnly::LINK)
        } else if !entry.is_regular() {
            Some(ReadOnly::NOT_FILE)
        } else if matches!(self.body, Body::Note(note) if note != content::EMPTY) {
            Some(ReadOnly::NOT_SHOWN)
        } else if entry.size() > content::LIMIT {
            Some(ReadOnly::TOO_LARGE)
        } else if !entry.writable() {
            Some(ReadOnly::NO_PERMISSION)
        } else {
            None
        }
    }

    /// The file line at the top of the view, counted from zero and skipping removed lines.
    fn top_line(&self) -> usize {
        let lines = self.body.lines();
        let from = self.top.min(lines.len().saturating_sub(1));
        let shown = lines[from..].iter().chain(lines[..from].iter().rev()).find(|line| line.kind != LineKind::Del);
        shown.map_or(0, |line| line.number as usize - 1)
    }

    fn apply(&mut self, step: Step, now: Instant) -> After {
        match step {
            Step::Ignored => return After::Ignored,
            Step::Stay => {}
            Step::Help => return After::Help,
            Step::Close => self.close_editor(None),
            Step::Save { close, overwrite } => self.save(close, overwrite, now),
        }
        After::Stay
    }

    /// Writes the draft over the file it was read from, if that file is still where it was.
    fn save(&mut self, close: bool, overwrite: bool, now: Instant) {
        let (Source::Working(work), Some(editor)) = (&self.source, &mut self.editor) else { return };
        let saved = {
            let (entry, stamp) = content::WorkingStamp::at(&work.repo, &work.root, &work.relative);
            match stamp.reachable() {
                Ok(()) => editor.save_to(entry.as_ref().map_err(|error| *error), overwrite),
                Err(_) => Err(SaveError::Refused("Not saved: the path to the file changed.")),
            }
        };
        match saved {
            Ok(()) => {
                editor.saved(now);
                if close {
                    self.close_editor(Some("Saved"));
                }
            }
            Err(SaveError::Changed) => editor.ask_overwrite(close),
            Err(SaveError::Refused(why)) => editor.say(why, now),
        }
    }

    /// Goes back to reading at the line the draft was showing, with a fresh diff.
    fn close_editor(&mut self, said: Option<&'static str>) {
        let Some(editor) = self.editor.take() else { return };
        let line = editor.top();
        drop(editor);
        // A save into the same file can leave it looking, to the reader's
        // cheap check, like the file it had already read.
        if let Source::Working(work) = &mut self.source {
            work.stamp = None;
            work.notified = false;
        }
        self.refresh(true);
        self.opening = false;
        self.clear_change();
        self.pan = 0;
        let lines = self.body.lines();
        self.top =
            lines.iter().position(|l| l.kind != LineKind::Del && l.number as usize > line).unwrap_or(lines.len());
        self.top = self.top.min(self.last_top());
        self.change_notice = said.map(|text| (JumpNotice::Said { lead: "", text }, Instant::now() + CHANGE_NOTICE));
    }
}
