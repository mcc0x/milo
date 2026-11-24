//! Shows one file with code colours and a line diff, and edits a working text file when asked.

mod buffer;
mod changes;
mod content;
mod disk;
mod draw;
mod edit;
#[cfg(test)]
mod edit_tests;
mod inbox;
mod handed;
mod markdown;
mod media;
mod media_load;
mod rescue;
mod tabs;
#[cfg(test)]
mod tests;
mod video;
mod working_path;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::actions::{self, Choice, Menu};
use crate::diff::{Line, LineKind};
use crate::git::{Commit, CommitVersion, Kind, PendingFile, Repo};
use crate::help::{self, Page};
use crate::keys::{Direction, Key};
use crate::native::{self, Action};
use crate::pane::{After, Pane};
use crate::picture::Picture;
use crate::preferences::{IconMode, Preferences};
use crate::rex;
use crate::screen::{Screen, Style};
use crate::syntax::{self, Lang, Span, State};
use crate::ui::{ADD, DEL};
use crate::watch::{Ignored, Scope};
use changes::{Folds, Review};
use disk::{ReadOnly, SaveError};
use draw::{Hint, Hints};
use edit::{Editor, Place, Step};
pub use rescue::take_notes as take_rescue_notes;
pub use tabs::Tabs;
pub use handed::{Handed, handed_lines};

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
    /// Last populated working Markdown view, kept even through a replacement gap.
    markdown_preview: Option<Box<markdown::Preview>>,
    /// The source/diff and rendered document keep independent scroll positions in this tab.
    markdown_source: bool,
    /// Scratch space reused when colouring code.
    spans: Vec<Span>,
    /// Reveal the first change once the viewport has room for the file.
    opening: bool,
    top: usize,
    review: Review,
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
    actions: Option<Menu>,
    hints: Hints,
    /// The draft's top line, while the freshly inspected source is still loading.
    pending_editor_line: Option<usize>,
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
    Committed {
        project: PathBuf,
        hash: String,
        time: i64,
    },
    /// The working file as it was when read, marked with a handed-over patch's edits.
    Handed(Handed),
}

impl Source {
    /// The working file, or why what is shown in its place is never edited.
    fn working(&self) -> Result<&WorkingFile, ReadOnly> {
        match self {
            Self::Working(work) => Ok(work),
            Self::Committed { .. } => Err(ReadOnly::COMMIT),
            Self::Handed(_) => Err(ReadOnly::HANDED),
        }
    }
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
    media: Option<media_load::State>,
    media_check: Option<content::MediaCheck>,
    media_recheck: bool,
}

/// A file selected in a pane that can open the reader.
pub enum ReadFile<'a> {
    Working(&'a str),
    Committed { commit: &'a Commit, file: &'a PendingFile },
}

/// Opens a file here, or in the floating reader in Rex.
pub enum OpenReader {
    Here,
    Floating(Option<Floated>),
}

/// The floating reader a pane last opened, and the project whose files are its tabs.
pub struct Floated {
    pub block: String,
    pub root: PathBuf,
}

/// What the floating reader already open does about one more file.
#[derive(Debug, PartialEq, Eq)]
enum Offer {
    /// It shows the file in a tab.
    Taken,
    /// It can only be replaced, and has no unsaved edits to lose by that.
    Replace,
    /// It can only be replaced, which would end it with unsaved edits.
    Unsaved,
}

impl Floated {
    /// Offers a file of the project at `root` to this reader. A reader keeps to
    /// the project it was opened for, so another project's file is never a tab of it.
    fn offer(&self, state: &Path, root: &Path, file: &Path, commit: Option<&str>) -> Offer {
        if self.root == root && inbox::send(state, &self.block, file, commit) {
            Offer::Taken
        } else if rescue::block_has_draft(state, &self.block) {
            Offer::Unsaved
        } else {
            Offer::Replace
        }
    }
}

impl OpenReader {
    pub fn new(inside_rex: bool) -> Self {
        if inside_rex { Self::Floating(None) } else { Self::Here }
    }

    pub fn open(&mut self, repo: &Repo, target: ReadFile<'_>) -> Result<Option<Reader>, &'static str> {
        match self {
            Self::Here => Ok(match target {
                ReadFile::Working(path) => Reader::open(&repo.root.join(path)),
                ReadFile::Committed { commit, file } => Some(Reader::committed(
                    repo,
                    file.path.clone(),
                    CommitVersion { hash: commit.hash.clone(), time: commit.time },
                    Some(file),
                )),
            }),
            Self::Floating(last) => {
                // What `milo view` is given: a working file in full, a file at a commit as Git names it.
                let (file, commit) = match target {
                    ReadFile::Working(path) => (repo.root.join(path), None),
                    ReadFile::Committed { commit, file } => (PathBuf::from(&file.path), Some(commit.hash.as_str())),
                };
                if let (Some(open), Some(state)) = (last.as_ref(), rescue::state_dir()) {
                    match open.offer(&state, &repo.root, &file, commit) {
                        Offer::Taken if rex::focus(&open.block, &file) => return Ok(None),
                        Offer::Taken => return Err("Rex did not show the reader."),
                        Offer::Unsaved => return Err("The open reader has unsaved edits."),
                        Offer::Replace => {}
                    }
                }
                let previous = last.as_ref().map(|open| open.block.as_str());
                let block = rex::open(&repo.root, &file, commit, previous)?;
                *last = Some(Floated { block, root: repo.root.clone() });
                Ok(None)
            }
        }
    }
}

enum Body {
    Text(Text),
    Note(&'static str),
    Picture(Picture),
    Video(Box<video::Video>),
}

impl Body {
    fn lines(&self) -> &[Line] {
        match self {
            Self::Text(text) => &text.lines,
            Self::Note(_) | Self::Picture(_) | Self::Video(_) => &[],
        }
    }

    fn from_lines(lines: Result<Vec<Line>, &'static str>, lang: Option<&'static Lang>) -> Self {
        match lines {
            Ok(lines) => Self::Text(Text::new(lines, lang)),
            Err(note) => Self::Note(note),
        }
    }
}

#[derive(Clone, Copy)]
enum MarkdownBody {
    Empty,
    Populated,
}

struct Text {
    lines: Vec<Line>,
    markdown: Option<MarkdownBody>,
    lang: Option<&'static Lang>,
    /// Start lines at which the open syntax differs from the preceding line.
    starts: Vec<(usize, State)>,
    /// Start of each block of changed lines, before adding viewing context.
    changes: Vec<usize>,
    folds: Folds,
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
        let mut folds = Folds::default();
        let mut state = State::default();
        let (mut last, mut added, mut removed) = (1, 0, 0);
        let mut previous = LineKind::Same;
        let mut end_of_change = None;
        for (i, line) in lines.iter().enumerate() {
            last = last.max(line.number);
            match line.kind {
                LineKind::Add => added += 1,
                LineKind::Del => removed += 1,
                LineKind::Same => {}
            }
            if previous == LineKind::Same && line.kind != LineKind::Same {
                let start = end_of_change.map_or(0, |end| end + LEAD);
                folds.push(start, i.saturating_sub(LEAD));
                changes.push(i);
            } else if previous != LineKind::Same && line.kind == LineKind::Same {
                end_of_change = Some(i);
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
        if let Some(end) = end_of_change.filter(|_| previous == LineKind::Same) {
            folds.push(end + LEAD, lines.len());
        }
        Self { lines, markdown: None, lang, starts, changes, folds, digits: last.ilog10() as usize + 1, added, removed }
    }
}

fn short_commit_id(hash: &str) -> &str {
    &hash[..hash.len().min(7)]
}

impl Reader {
    fn markdown_body(&self) -> Option<MarkdownBody> {
        if let Body::Text(text) = &self.body { text.markdown } else { None }
    }

    fn markdown(&self) -> Option<&markdown::Preview> {
        self.markdown_body().and(self.markdown_preview.as_deref())
    }

    fn pause_video(&mut self) {
        if let Body::Video(video) = &mut self.body {
            video.pause();
        }
    }

    /// Whether the file is being edited and the draft is not what was last saved.
    fn unsaved(&self) -> bool {
        self.editor.as_ref().is_some_and(|editor| editor.dirty())
    }

    /// Whether the draft is waiting for the answer to a question.
    fn asking(&self) -> bool {
        self.editor.as_ref().is_some_and(|editor| editor.asking())
    }

    fn offers_native_actions(&self) -> bool {
        cfg!(target_os = "macos") && matches!(self.source, Source::Working(_))
    }

    fn can_preview(&self) -> bool {
        !self.markdown_source && self.markdown().is_some()
    }

    /// Use the current view to decide whether to advertise the menu. Check
    /// the working path and external capabilities only on explicit input.
    fn offers_actions(&self) -> bool {
        matches!(self.source, Source::Working(_))
            && (self.read_only.is_none() || self.can_preview() || self.offers_native_actions())
    }

    fn action_help(&self) -> Option<help::HelpRow> {
        self.offers_actions().then(|| help::key("a Ctrl-A", "file actions"))
    }

    fn show_actions(&mut self, now: Instant) -> After {
        let Source::Working(work) = &self.source else { return After::Ignored };
        let root = &work.repo.root;
        let id = Some(work.root.identity());
        let path = &work.relative;
        let mut choices = Vec::new();
        if self.read_only.is_none() && self.edit_refusal().is_none() {
            choices.push(Choice::Edit);
        }
        if self.can_preview() && crate::preview::eligible(root, id, path) {
            choices.push(Choice::Preview);
        }
        let native = native::choices(root, id, path);
        if native.open {
            choices.push(Choice::Open);
        }
        if native.reveal {
            choices.push(Choice::Reveal);
        }
        if choices.is_empty() {
            self.say("No file actions here.", now);
        } else {
            let menu = Menu::new(choices, root);
            self.pause_video();
            self.clear_hits();
            self.actions = Some(menu);
        }
        After::Stay
    }

    fn run_action(&mut self, choice: Choice, now: Instant) -> After {
        match choice {
            Choice::Edit => self.edit(now),
            Choice::Open => self.native_action(Action::Open, now),
            Choice::Reveal => self.native_action(Action::Reveal, now),
            Choice::Preview => {
                let result = match &self.source {
                    Source::Working(work) if self.can_preview() => {
                        crate::preview::open(&work.repo.root, Some(work.root.identity()), &work.relative)
                    }
                    _ => Err("Markdown preview is unavailable here."),
                };
                self.say(result.err().unwrap_or("Opened in tkmp."), now);
                After::Stay
            }
            Choice::Read => unreachable!("already reading this file"),
        }
    }

    fn native_action(&mut self, action: Action, now: Instant) -> After {
        match self.source.working() {
            Ok(work) => {
                let success = match action {
                    Action::Open => "Opened in default app.",
                    Action::Reveal => "Revealed in Finder.",
                };
                let result = action.run(&work.repo.root, Some(work.root.identity()), &work.relative);
                self.say(result.err().unwrap_or(success), now);
            }
            Err(ReadOnly(why)) => {
                let notice = JumpNotice::Said { lead: "Unavailable: ", text: why };
                self.change_notice = Some((notice, now + CHANGE_NOTICE));
            }
        }
        After::Stay
    }

    fn clear_hits(&mut self) {
        self.hints = Hints::default();
        self.review.click_width = 0;
    }

    fn hide(&mut self) {
        self.actions = None;
        self.clear_hits();
        self.park_media();
    }

    fn native_help(&self) -> impl Iterator<Item = help::HelpRow> {
        self.offers_native_actions()
            .then(|| [help::key("^O", "open in default app"), help::key("^R", "reveal in Finder")])
            .into_iter()
            .flatten()
    }

    /// Says `text` for a while, where the reader or its draft says such things.
    fn say(&mut self, text: &'static str, now: Instant) {
        match &mut self.editor {
            Some(editor) => editor.say(text, now),
            None => self.change_notice = Some((JumpNotice::Said { lead: "", text }, now + CHANGE_NOTICE)),
        }
    }

    pub(super) fn working_repo(&self) -> Option<&Repo> {
        self.source.working().ok().map(|work| &work.repo)
    }

    fn new(path: String, source: Source, icons: IconMode, preference_note: Option<&'static str>) -> Self {
        Self {
            source,
            path,
            icons,
            preference_note,
            kind: Kind::None,
            body: Body::Note("Git did not answer."),
            markdown_preview: None,
            markdown_source: false,
            spans: Vec::new(),
            opening: true,
            top: 0,
            review: Review::default(),
            current_change: None,
            change_notice: None,
            pan: 0,
            rows: 0,
            columns: 0,
            pan_limit: 0,
            read_only: None,
            editor: None,
            actions: None,
            hints: Hints::default(),
            pending_editor_line: None,
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
            self.top = self.folds().row_at(*first).saturating_sub(self.lead());
        }
    }

    fn lead(&self) -> usize {
        LEAD.min(self.rows.saturating_sub(1))
    }

    fn last_top(&self) -> usize {
        self.folds().row_count(self.body.lines().len()).saturating_sub(self.rows.max(1))
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
            self.review.moved();
            self.clear_change();
        }
    }

    fn jump(&mut self, direction: Direction, now: Instant) -> After {
        let notice = if let Body::Text(text) = &self.body
            && !text.changes.is_empty()
        {
            let total = text.changes.len();
            let (lead, last) = (self.lead(), self.last_top());
            let at = |line: &usize| self.folds().row_at(*line).saturating_sub(lead).min(last);
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
            self.top = at(&text.changes[index]);
            self.review.moved();
            self.opening = false;
            self.current_change = Some(index);
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
        if let Body::Video(video) = &self.body {
            let mut rows = media::help_rows(video);
            rows.extend(self.action_help());
            rows.extend(self.native_help());
            rows.push(key("q Esc", "close"));
            return Page::new(rows);
        }
        if self.markdown().is_some() && !self.markdown_source {
            let mut rows = vec![
                heading("Rendered Markdown"),
                key("m    ", "source / diff"),
                key("↑↓   ", "scroll (k j)"),
                key("Space", "page down"),
                key("b    ", "page up"),
                key("g    ", "top"),
                key("G    ", "follow end"),
                note("Scrolling stops follow."),
                sample("!", Style::PLAIN.bold(), "recently changed"),
            ];
            rows.extend(self.action_help());
            rows.extend(self.native_help());
            if let Some(ReadOnly(why)) = self.read_only {
                rows.extend([note("Read-only:"), note(why)]);
            } else {
                rows.push(key("e    ", "edit source"));
                let name = self.path.rsplit('/').next().unwrap_or_default();
                rows.extend(edit::format_help(name, self.tools.as_deref()));
            }
            rows.extend([key("q Esc", "close"), note("Links are not opened.")]);
            return Page::new(rows);
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
        ];
        if self.markdown().is_some() {
            rows.push(key("m    ", "render Markdown"));
        }
        if self.can_fold() {
            rows.extend([
                key("c    ", "changes only/full"),
                key("Tab  ", "choose next fold"),
                key("Enter", "expand fold"),
                note("Or click a fold."),
            ]);
        }
        rows.extend(self.action_help());
        rows.extend(self.native_help());
        rows.extend([
            key("g G  ", "top/bottom"),
            key("q Esc", "close"),
            HelpRow::new(),
            heading("Editing"),
        ]);
        match self.read_only {
            None => {
                let name = self.path.rsplit('/').next().unwrap_or_default();
                rows.extend([key("e    ", "edit this file"), note("Nothing is written"), note("until you save.")]);
                rows.extend(edit::format_help(name, self.tools.as_deref()));
            }
            Some(ReadOnly(why)) => rows.extend([note("Read-only:"), note(why)]),
        }
        match &self.source {
            Source::Working(_) => {}
            Source::Committed { .. } => rows.insert(1, note("As of a commit")),
            Source::Handed(handed) => {
                let files = (handed.total > 1).then(|| key("[ ]  ", "prev/next file"));
                let about = [
                    heading("Handed-over patch"),
                    note("Marked lines are what"),
                    note("the patch says it"),
                    note("changed. Milo cannot"),
                    note("tell who wrote them."),
                ];
                rows.splice(0..0, about.into_iter().chain(files).chain([key("r    ", "reload"), HelpRow::new()]));
            }
        }
        Page::new(rows)
    }

    fn cover(&mut self) {
        self.actions = None;
        self.clear_hits();
        self.pause_video();
    }

    fn tracks_hover(&self) -> bool {
        true
    }

    fn hover_span(&self, x: usize, y: usize) -> Option<std::ops::Range<usize>> {
        match &self.actions {
            Some(menu) => menu.hover_span(x, y),
            None => self.hints.at(x, y).map(|(_, span)| span.clone()),
        }
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
        // A pending or ready preview does not wait for the index; its status can.
        self.editor.is_none()
            && matches!(work.media, None | Some(media_load::State::Text { .. }))
            && work.repo.busy()
            && work.root.at_path(&work.repo.root) == Ok(true)
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
            Source::Committed { .. } | Source::Handed(_) => false,
        };
        self.refresh(force);
        self.top = self.top.min(self.last_top());
        true
    }

    fn next_frame(&self) -> Option<Duration> {
        let now = Instant::now();
        match &self.editor {
            Some(editor) => editor.next_frame(now),
            None => {
                let message = self.change_notice.map(|(_, until)| until.saturating_duration_since(now));
                let marks = if !self.markdown_source && matches!(self.markdown_body(), Some(MarkdownBody::Populated)) {
                    self.markdown().and_then(markdown::Preview::mark_deadline)
                } else {
                    None
                };
                let loading = matches!(&self.source, Source::Working(work)
                    if matches!(&work.media, Some(media_load::State::Loading(_))) || work.media_check.is_some());
                let playing = matches!(&self.body, Body::Video(video) if video.needs_tick());
                (loading || playing)
                    .then_some(Duration::from_millis(30))
                    .into_iter()
                    .chain(message)
                    .chain(marks.map(|until| until.saturating_duration_since(now)))
                    .min()
            }
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
        if let Some(menu) = &mut self.actions {
            return match menu.key(key) {
                actions::Step::Stay => After::Stay,
                actions::Step::Close => {
                    self.actions = None;
                    After::Stay
                }
                actions::Step::Run(choice) => {
                    self.actions = None;
                    self.run_action(choice, now)
                }
            };
        }
        if let Key::Click(x, y) = key
            && let Some((hint, _)) = self.hints.at(x, y)
        {
            return match hint {
                Hint::Actions => self.show_actions(now),
                Hint::Help => {
                    self.cover();
                    After::Help
                }
            };
        }
        if let Some(editor) = &mut self.editor {
            let step = editor.key(key, now);
            return self.apply(step, now);
        }
        if key == Key::Ctrl(b'a') || key == Key::Char('a') && self.offers_actions() {
            return self.show_actions(now);
        }
        if let Some(action) = Action::key(key) {
            return self.native_action(action, now);
        }
        if let Body::Video(video) = &mut self.body
            && let Some(after) = media::input(video, key)
        {
            return after;
        }
        let key = match key {
            Key::Char(' ') => Key::PgDn,
            Key::Char('b') => Key::PgUp,
            _ => key,
        };
        if let Some(body) = self.markdown_body()
            && let Some(preview) = self.markdown_preview.as_deref_mut()
        {
            if key == Key::Char('m') {
                preview.stop_follow();
                self.markdown_source = !self.markdown_source;
                self.change_notice = None;
                self.clear_hits();
                return After::Stay;
            }
            if !self.markdown_source {
                if key == Key::Char('G') {
                    return if preview.follow_end() { After::Stay } else { After::Ignored };
                }
                if let Some(delta) = key.rows(self.rows) {
                    if matches!(body, MarkdownBody::Empty) {
                        let following = preview.following();
                        preview.stop_follow();
                        return if following { After::Stay } else { After::Ignored };
                    }
                    return if preview.scroll(delta) { After::Stay } else { After::Ignored };
                }
                if !matches!(key, Key::Char('e' | 'q') | Key::Esc) {
                    return After::Ignored;
                }
            }
        } else if !self.markdown_source
            && key != Key::Char('G')
            && key.rows(self.rows).is_some()
            && let Some(preview) = &mut self.markdown_preview
        {
            // The old document is not visible, but manual navigation still disarms follow.
            preview.stop_follow();
        }
        let before = (self.top, self.pan);
        match key {
            Key::Char('q') | Key::Esc => return After::Leave,
            Key::Char('e') => return self.edit(now),
            Key::Char('n') => return self.jump(Direction::Next, now),
            Key::Char('N') => return self.jump(Direction::Previous, now),
            Key::Char('c') => return self.toggle_changes(),
            Key::Ctrl(b'i') => return self.next_fold(),
            Key::Enter => return self.expand_fold(self.focused_fold()),
            Key::Click(x, y) => return self.click_fold(x, y),
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
                if let Some(preview) = &mut self.markdown_preview {
                    preview.stop_follow();
                }
                self.editor = Some(Box::new(editor));
                self.clear_hits();
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
        let work = self.source.working()?;
        let (entry, stamp) = content::WorkingStamp::at(&work.repo, &work.root, &work.relative);
        let entry = stamp.reachable().and(entry.map_err(ReadOnly::of))?;
        let original = disk::load(&entry)?;
        let place = Place {
            file: work.repo.root.join(&work.relative),
            root: work.repo.root.clone(),
            state: self.state.clone(),
            tools: self.tools.clone(),
        };
        Ok(Editor::open(original, syntax::for_file(&self.path), self.top_line(), place))
    }

    /// Whether the file may be edited, as far as its place and what is shown of it can say.
    fn edit_refusal(&self) -> Option<ReadOnly> {
        let work = match self.source.working() {
            Ok(work) => work,
            Err(why) => return Some(why),
        };
        match &self.body {
            Body::Note(note) if *note == content::NOT_TEXT => return Some(ReadOnly::NOT_TEXT),
            Body::Picture(_) => return Some(ReadOnly::PICTURE),
            Body::Video(_) => return Some(ReadOnly::NOT_TEXT),
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
        if !self.markdown_source
            && let Some(body) = self.markdown_body()
        {
            return match body {
                MarkdownBody::Empty => 0,
                MarkdownBody::Populated => self.markdown_preview.as_deref().map_or(0, markdown::Preview::source_line),
            };
        }
        self.line_number_at(self.source_top())
    }

    fn line_number_at(&self, row: usize) -> usize {
        let lines = self.body.lines();
        let from = row.min(lines.len().saturating_sub(1));
        let shown = lines[from..].iter().chain(lines[..from].iter().rev()).find(|line| line.kind != LineKind::Del);
        shown.map_or(0, |line| line.number as usize - 1)
    }

    fn source_row(&self, number: usize) -> usize {
        let lines = self.body.lines();
        lines.iter().position(|l| l.kind != LineKind::Del && l.number as usize > number).unwrap_or(lines.len())
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
        let (Source::Working(work), Some(editor)) = (&mut self.source, &mut self.editor) else { return };
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
                // Even a save indistinguishable by its stamp needs a fresh source.
                work.stamp = None;
                if close {
                    self.close_editor(Some("Saved"));
                }
            }
            Err(SaveError::Changed) => editor.ask_overwrite(close),
            Err(SaveError::Refused(why)) => editor.say(why, now),
        }
    }

    /// Goes back to reading at the line the draft was showing.
    fn close_editor(&mut self, said: Option<&'static str>) {
        let Some(editor) = self.editor.take() else { return };
        self.clear_hits();
        let line = editor.top();
        drop(editor);
        self.pending_editor_line = None;
        let force = match &mut self.source {
            Source::Working(work) => std::mem::take(&mut work.notified),
            _ => false,
        };
        self.refresh(force);
        if self.markdown_source || self.markdown().is_none() {
            self.opening = false;
        }
        if matches!(self.body, Body::Note(media_load::PENDING)) {
            self.pending_editor_line = Some(line);
        } else if line != self.top_line() {
            self.show_editor_line(line);
        }
        self.change_notice = said.map(|text| (JumpNotice::Said { lead: "", text }, Instant::now() + CHANGE_NOTICE));
    }

    fn show_editor_line(&mut self, line: usize) {
        if !self.markdown_source
            && let Some(body) = self.markdown_body()
        {
            if matches!(body, MarkdownBody::Populated)
                && let Some(preview) = &mut self.markdown_preview
            {
                preview.go_to_line(line);
            }
            return;
        }
        self.opening = false;
        self.pan = 0;
        self.set_source_top(self.source_row(line));
    }
}
