//! Holds the files open in one reader as tabs, and shows one of them at a time.
//!
//! Each tab is a whole reader, so its place in the file, its draft and that
//! draft's undo history stay with it while another tab is shown. A reader
//! with one file draws exactly as it does alone; a strip of names appears
//! above it with the second. A tab closes only once its file is no longer
//! being edited, which is where unsaved edits are asked about.

use std::cell::{Cell, OnceCell};
use std::ops::Range;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::content::{CommittedTarget, WorkingTarget};
use super::inbox::{Inbox, Request};
use super::reopen::{Plan, Reopen, Step};
use super::rescue::{self, Mark};
use super::sets::{Entry, List, Shelf};
use super::{Body, Reader, Source, media_load};
use crate::git::{CommitId, Repo};
use crate::help::{self, Page};
use crate::keys::Key;
use crate::pane::{After, External, Pane};
use crate::rex;
use crate::screen::{Screen, Style, Wash, YELLOW};
use crate::ui::FAINT;
use crate::watch::Scope;
use crate::width::str_width;

/// The most columns a name takes in the strip.
const NAME_ROOM: usize = 24;
/// The most columns the folder before a name takes, where two tabs share a name.
const FOLDER_ROOM: usize = 12;
/// The fewest columns of the shown tab's name that are kept beside its close mark.
const NAME_LEAST: usize = 4;
/// Columns at an edge of the strip that say tabs are hidden beyond it.
const EDGE: usize = 2;
const UNSAVED: &str = "*";
const CLOSE: &str = "×";
/// The keys of the strip, said at its right edge where every tab is shown and room is left.
const KEYS: [&str; 2] = ["^N ^P tabs · ^W close", "^N ^P tabs"];

/// The last part of a path written with `/`.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The clickable parts of a strip, by their columns.
type Hits = Vec<(Range<usize>, Hit)>;

/// What a click on a part of the strip does to the tab it names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Show(usize),
    Close(usize),
}

/// The Rex block a reader runs in, and the state folder that says so to other panes.
pub(super) struct Place {
    pub(super) state: PathBuf,
    pub(super) block: String,
}

pub struct Tabs {
    /// Never empty: closing the last tab closes the reader.
    tabs: Vec<Reader>,
    shown: usize,
    place: Option<Place>,
    inbox: Option<Inbox>,
    /// Says to other panes, while it lives, that some tab has unsaved edits.
    mark: Option<Mark>,
    /// Rows the strip took in the last frame.
    strip_rows: usize,
    /// The clickable parts of the strip in the last frame, the narrower first.
    hits: Hits,
    /// Another tab is shown since the loop last asked what to watch.
    rescope: Cell<bool>,
    /// The shown tab closes once its edit has ended.
    closing: bool,
    /// Where the tabs are remembered, found when first needed: a reader that
    /// is only read looks nothing up until it closes.
    memory: OnceCell<Option<Memory>>,
    /// The screen of lists, while it covers the tabs.
    reopen: Option<Reopen>,
}

/// Where one reader's tabs are remembered as the last tabs of its worktree.
struct Memory {
    state: PathBuf,
    /// The top of the worktree the first tab's file is read from. Only tabs of that worktree are remembered.
    root: PathBuf,
    /// The last tabs as they were before this reader began to replace them.
    earlier: Option<List>,
}

impl Memory {
    fn new(state: PathBuf, root: PathBuf) -> Self {
        let earlier = Shelf::new(state.clone(), &root).last();
        Self { state, root, earlier }
    }

    fn shelf(&self) -> Shelf {
        Shelf::new(self.state.clone(), &self.root)
    }
}

/// The top of the worktree a tab's file is read from. A handed-over patch is never remembered.
fn root_of(reader: &Reader) -> Option<&Path> {
    match &reader.source {
        Source::Working(work) => Some(&work.repo.root),
        Source::Committed { project, .. } => Some(project),
        Source::Handed(_) => None,
    }
}

impl Reader {
    /// This tab as a list keeps it, if its file is read from the worktree at `root`.
    fn listed(&self, root: &Path) -> Option<Entry> {
        let (path, commit) = match &self.source {
            Source::Working(work) if work.repo.root == root => (work.relative.to_str()?.to_owned(), None),
            Source::Committed { project, hash, .. } if project == root => (self.path.clone(), Some(hash.clone())),
            _ => return None,
        };
        let line = self.editor.as_ref().map_or_else(|| self.top_line(), |editor| editor.top());
        Some(Entry { path, commit, line })
    }

    /// Shows the file from `line`, as nearly as the file as it is now allows.
    fn show_from(&mut self, line: usize) {
        if matches!(self.body, Body::Note(media_load::PENDING)) {
            self.pending_editor_line = Some(line);
        } else {
            self.show_editor_line(line);
        }
    }
}

/// A tab of a list as it can be opened now: the working file, or the file at the commit that was kept.
enum Kept {
    Working(WorkingTarget),
    Committed(CommittedTarget),
}

impl Kept {
    /// None for a working file that is gone and for a commit the repository does not have.
    /// The working file never stands in for the file at a commit.
    fn find(root: &Path, entry: &Entry) -> Option<Self> {
        match &entry.commit {
            Some(id) => CommittedTarget::resolve_from(root, Path::new(&entry.path), CommitId::parse(id)?)
                .ok()
                .map(Self::Committed),
            None => WorkingTarget::resolve(&root.join(&entry.path)).filter(WorkingTarget::present).map(Self::Working),
        }
    }

    fn matches(&self, reader: &Reader) -> bool {
        match self {
            Self::Working(target) => target.matches(reader),
            Self::Committed(target) => target.matches(reader),
        }
    }

    /// Reads the file as it is now and shows it from the line that was kept.
    fn load(self, line: usize) -> Reader {
        let mut reader = match self {
            Self::Working(target) => target.load(),
            Self::Committed(target) => target.load(),
        };
        reader.show_from(line);
        reader
    }
}

impl Tabs {
    /// A reader of one file, which in a Rex block takes requests to show more.
    pub fn new(reader: Reader) -> Self {
        // Rex names the block in every terminal it starts. Listening there needs no session.
        let block = std::env::var("REX_BLOCK").ok().filter(|id| !cfg!(test) && (rex::inside() || rex::runs_in(id)));
        Self::placed(reader, rescue::state_dir().zip(block).map(|(state, block)| Place { state, block }))
    }

    pub(super) fn placed(reader: Reader, place: Option<Place>) -> Self {
        let inbox = place.as_ref().and_then(|place| Inbox::open(&place.state, &place.block, reader.project()));
        Self {
            tabs: vec![reader],
            shown: 0,
            inbox,
            place,
            mark: None,
            strip_rows: 0,
            hits: Vec::new(),
            rescope: Cell::new(false),
            closing: false,
            memory: OnceCell::new(),
            reopen: None,
        }
    }

    /// A reader of the chosen tabs of a list, in their saved order, showing the one that
    /// was shown. None if none of them can be opened now.
    fn reopened(plan: &Plan, root: &Path) -> Option<Self> {
        let mut loaded = plan
            .entries
            .iter()
            .enumerate()
            .filter_map(|(at, entry)| Some((at, Kept::find(root, entry)?.load(entry.line))));
        let (first, reader) = loaded.next()?;
        let mut tabs = Self::new(reader);
        let mut shown = plan.active.filter(|active| *active == first).map(|_| 0);
        for (at, reader) in loaded {
            if plan.active == Some(at) {
                shown = Some(tabs.tabs.len());
            }
            tabs.tabs.push(reader);
        }
        tabs.shown = shown.unwrap_or(0);
        let shown = tabs.shown;
        tabs.tabs.iter_mut().enumerate().filter(|(tab, _)| *tab != shown).for_each(|(_, reader)| reader.hide());
        // The loop has yet to learn what this reader watches.
        tabs.rescope.set(true);
        Some(tabs)
    }

    /// Remembers this reader's tabs under `state`, as a reader outside a test does unasked.
    #[cfg(test)]
    fn remembering(mut self, state: PathBuf) -> Self {
        let memory = root_of(&self.tabs[0]).map(|root| Memory::new(state, root.to_path_buf()));
        self.memory = OnceCell::from(memory);
        self
    }

    fn memory(&self) -> Option<&Memory> {
        let find = || Some(Memory::new(rescue::state_dir()?, root_of(&self.tabs[0])?.to_path_buf()));
        self.memory.get_or_init(find).as_ref()
    }

    /// The tabs as a list keeps them: those of the worktree at `root`, in their order.
    fn list(&self, root: &Path) -> List {
        let mut list = List::default();
        for (tab, reader) in self.tabs.iter().enumerate() {
            if let Some(entry) = reader.listed(root) {
                if tab == self.shown {
                    list.active = list.entries.len();
                }
                list.entries.push(entry);
            }
        }
        list
    }

    /// Keeps the tabs as the last tabs of their worktree. A reader with nothing to remember leaves them alone.
    fn record(&self) {
        if let Some(memory) = self.memory() {
            let list = self.list(&memory.root);
            if !list.entries.is_empty() {
                memory.shelf().keep_last(&list);
            }
        }
    }

    /// Covers the tabs with the screen of lists.
    fn show_lists(&mut self) -> After {
        let Some(memory) = self.memory() else {
            self.reader_mut().say("Tabs are not remembered here.", Instant::now());
            return After::Stay;
        };
        let screen =
            Reopen::new(memory.shelf(), memory.root.clone(), Some(self.list(&memory.root)), memory.earlier.clone());
        self.reader_mut().cover();
        self.reopen = Some(screen);
        After::Stay
    }

    fn reopen_key(&mut self, key: Key) -> After {
        let Some(screen) = &mut self.reopen else { return After::Ignored };
        let step = screen.key(key);
        self.after_reopen(step)
    }

    fn after_reopen(&mut self, step: Step) -> After {
        match step {
            Step::Ignored => return After::Ignored,
            Step::Stay => {}
            Step::Close => self.reopen = None,
            Step::Reopen(plan) => {
                self.reopen = None;
                self.restore(&plan, Instant::now());
            }
        }
        After::Stay
    }

    /// Reopens the chosen tabs of a list. A file that has a tab keeps it as it is, draft and
    /// place included; the others are added after the tabs already here, in their saved order.
    fn restore(&mut self, plan: &Plan, now: Instant) {
        let Some(root) = self.memory().map(|memory| memory.root.clone()) else { return };
        let before = self.tabs.len();
        let (mut first, mut active, mut refused) = (None, None, false);
        for (at, entry) in plan.entries.iter().enumerate() {
            let Some(kept) = Kept::find(&root, entry) else {
                refused = true;
                continue;
            };
            let tab = self.tabs.iter().position(|tab| kept.matches(tab)).unwrap_or_else(|| {
                self.tabs.push(kept.load(entry.line));
                self.tabs.len() - 1
            });
            first = first.or(Some(tab));
            if plan.active == Some(at) {
                active = Some(tab);
            }
        }
        let show = active.or(first);
        for tab in (before..self.tabs.len()).filter(|tab| Some(*tab) != show) {
            self.tabs[tab].hide();
        }
        match show {
            // A tab just loaded is shown as it was read. Rescoping has the loop look at it again.
            Some(tab) if tab >= before => {
                self.reader_mut().hide();
                self.activate(tab);
            }
            Some(tab) => self.select(tab),
            None => {}
        }
        if refused {
            self.reader_mut().say("Some tabs could not be reopened.", now);
        } else if self.tabs.len() == before {
            self.reader_mut().say("Those tabs were already open.", now);
        }
    }

    fn reader(&self) -> &Reader {
        &self.tabs[self.shown]
    }

    fn reader_mut(&mut self) -> &mut Reader {
        &mut self.tabs[self.shown]
    }

    /// Shows the file asked for: in the tab that has it, or in a new one, and at
    /// the place asked for. A tab's draft stays as it is.
    /// Returns false if it could not be opened, which the shown tab then says.
    pub(super) fn open(&mut self, request: &Request, now: Instant) -> bool {
        let opened = if let Some(id) = &request.commit {
            CommitId::parse(id)
                .ok_or("That is not a commit ID.")
                .and_then(|id| CommittedTarget::resolve(&request.file, id))
                .map(|target| {
                    if !self.select_existing(|tab| target.matches(tab)) {
                        self.add(|| target.load());
                    }
                })
        } else {
            WorkingTarget::resolve(&request.file).ok_or("That file is not in a folder.").map(|target| {
                if !self.select_existing(|tab| target.matches(tab)) {
                    self.add(|| target.load());
                }
                if let Some(spot) = request.spot {
                    self.reader_mut().go_to(spot, now);
                }
            })
        };
        match opened {
            Ok(()) => true,
            Err(why) => {
                self.reader_mut().say(why, now);
                false
            }
        }
    }

    fn select_existing(&mut self, matches: impl FnMut(&Reader) -> bool) -> bool {
        let Some(tab) = self.tabs.iter().position(matches) else { return false };
        self.select(tab);
        true
    }

    fn add(&mut self, load: impl FnOnce() -> Reader) {
        self.reader_mut().hide();
        self.tabs.push(load());
        // The first snapshot was just loaded. Rescope so the pane loop still
        // reconciles it after installing the watcher, without forcing a second diff now.
        self.activate(self.tabs.len() - 1);
    }

    fn select(&mut self, tab: usize) {
        if tab != self.shown {
            self.show(tab);
        }
    }

    fn activate(&mut self, tab: usize) {
        self.shown = tab;
        self.closing = false;
        self.rescope.set(true);
        self.record();
    }

    /// Brings a tab forward as its file is now, not as it was when the tab was last shown.
    fn show(&mut self, tab: usize) {
        if tab != self.shown {
            self.reader_mut().hide();
        }
        self.activate(tab);
        let reader = self.reader_mut();
        if !reader.git_busy() {
            reader.filesystem_changed();
            reader.check();
        }
    }

    fn step(&mut self, forward: bool) -> After {
        let count = self.tabs.len();
        self.show((self.shown + if forward { 1 } else { count - 1 }) % count);
        After::Stay
    }

    /// Closes a tab, after its edit has ended: a draft with unsaved edits asks first.
    fn close(&mut self, tab: usize) -> After {
        self.select(tab);
        if !self.reader().editing() {
            return self.remove_shown();
        }
        self.closing = true;
        if self.pass(Key::Esc) == After::Leave { After::Leave } else { After::Stay }
    }

    fn remove_shown(&mut self) -> After {
        self.closing = false;
        if self.tabs.len() == 1 {
            return After::Leave;
        }
        self.tabs.remove(self.shown);
        self.shown = self.shown.min(self.tabs.len() - 1);
        self.show(self.shown);
        After::Stay
    }

    /// Ctrl-C ends the whole reader, so every draft with unsaved edits is asked about first, one at a time.
    fn quit(&mut self) -> After {
        match self.tabs.iter().position(Reader::unsaved) {
            Some(tab) => {
                self.select(tab);
                self.pass(Key::Ctrl(b'c'))
            }
            None => After::Leave,
        }
    }

    /// Gives a key to the shown tab, and closes the tab if that is what the key came to.
    fn pass(&mut self, key: Key) -> After {
        let after = self.reader_mut().key(key);
        if after == After::Leave || self.closing && !self.reader().editing() {
            return self.remove_shown();
        }
        // Going back from the question keeps the tab as well as the draft.
        self.closing &= self.reader().asking();
        after
    }

    fn sync_mark(&mut self) {
        if !self.tabs.iter().any(Reader::unsaved) {
            self.mark = None;
        } else if self.mark.is_none()
            && let Some(place) = &self.place
        {
            self.mark = Mark::set(&place.state, &place.block);
        }
    }

    /// The name a tab goes by: its file's.
    fn name(&self, tab: usize) -> &str {
        file_name(&self.tabs[tab].path)
    }

    /// The folder that tells a tab from another whose file has the same name, where there is such a tab.
    /// A commit already tells a file as it was from the file as it is.
    fn folder(&self, tab: usize) -> Option<&str> {
        let reader = &self.tabs[tab];
        let name = file_name(&reader.path);
        let shared = (0..self.tabs.len())
            .any(|other| other != tab && self.name(other) == name && self.commit(other) == self.commit(tab));
        if !shared {
            return None;
        }
        let within = reader.path.rsplit_once('/').map(|(folder, _)| file_name(folder));
        // A file at the top of its project is told apart by the project's own folder.
        within.or_else(|| reader.working_repo()?.root.file_name()?.to_str())
    }

    /// The commit a tab shows its file at, as the strip writes it after the name.
    fn commit(&self, tab: usize) -> Option<&str> {
        match &self.tabs[tab].source {
            Source::Committed { hash, .. } => Some(super::short_commit_id(hash)),
            Source::Working(_) | Source::Handed(_) => None,
        }
    }

    /// The columns a tab takes where the strip has room for all of it.
    fn width(&self, tab: usize) -> usize {
        let folder = self.folder(tab).map_or(0, |folder| str_width(folder).min(FOLDER_ROOM) + 1);
        let commit = self.commit(tab).map_or(0, |id| 1 + id.len());
        let close = if tab == self.shown { 1 + str_width(CLOSE) } else { 0 };
        let name = str_width(self.name(tab)).min(NAME_ROOM);
        2 + usize::from(self.tabs[tab].unsaved()) + folder + name + commit + close
    }

    /// The tabs drawn in a strip `w` columns wide: the shown one, then as many of its neighbours as fit whole.
    fn window(&self, w: usize) -> Range<usize> {
        let count = self.tabs.len();
        let room = |tabs: &Range<usize>| {
            w.saturating_sub(EDGE * usize::from(tabs.start > 0) + EDGE * usize::from(tabs.end < count))
        };
        let mut tabs = self.shown..self.shown + 1;
        let mut used = self.width(self.shown);
        loop {
            let after = tabs.start..tabs.end + 1;
            let before = tabs.start.saturating_sub(1)..tabs.end;
            if tabs.end < count && used + self.width(tabs.end) <= room(&after) {
                used += self.width(tabs.end);
                tabs = after;
            } else if tabs.start > 0 && used + self.width(before.start) <= room(&before) {
                used += self.width(before.start);
                tabs = before;
            } else {
                return tabs;
            }
        }
    }

    fn draw_strip(&mut self, s: &mut Screen) {
        let mut hits = std::mem::take(&mut self.hits);
        hits.clear();
        let tabs = self.window(s.w);
        let unsaved = |tabs: Range<usize>| self.tabs[tabs].iter().any(Reader::unsaved);
        let mut x = 0;
        if tabs.start > 0 {
            s.put(0, 0, 1, "‹", FAINT);
            if unsaved(0..tabs.start) {
                s.put(1, 0, 1, UNSAVED, Style::fg(YELLOW));
            }
            hits.push((0..EDGE, Hit::Show(tabs.start - 1)));
            x = EDGE;
        }
        let more = tabs.end < self.tabs.len();
        let end = s.w.saturating_sub(if more { EDGE } else { 0 });
        for tab in tabs.clone() {
            x += self.draw_tab(s, x, end.saturating_sub(x), tab, &mut hits);
        }
        if more {
            if unsaved(tabs.end..self.tabs.len()) {
                s.put(end, 0, 1, UNSAVED, Style::fg(YELLOW));
            }
            s.put(end + 1, 0, 1, "›", FAINT);
            hits.push((end..s.w, Hit::Show(tabs.end)));
        }
        if tabs == (0..self.tabs.len()) {
            // A tab ends in a blank column; one more keeps the keys clear of it, and one of the edge.
            let room = s.w.saturating_sub(x + 2);
            if let Some(keys) = KEYS.iter().find(|keys| str_width(keys) <= room) {
                s.put_right(s.w - 1, 0, keys, FAINT);
            }
        }
        self.hits = hits;
    }

    /// Draws one tab from column `x` in at most `room` columns. Returns the columns it took.
    fn draw_tab(&self, s: &mut Screen, x: usize, room: usize, tab: usize, hits: &mut Hits) -> usize {
        let shown = tab == self.shown;
        let unsaved = self.tabs[tab].unsaved();
        let name = self.name(tab);
        // What tells this tab from others of its name is shown where the whole tab fits.
        let whole = self.width(tab) <= room;
        let folder = self.folder(tab).filter(|_| whole);
        let commit = self.commit(tab).filter(|_| whole);
        let close = 1 + str_width(CLOSE);
        let closable = shown && room >= 2 + usize::from(unsaved) + NAME_LEAST + close;
        let end = x + room;
        let mut at = (x + 1).min(end);
        if unsaved {
            at += s.put(at, 0, end - at, UNSAVED, Style::fg(YELLOW));
        }
        let style = if shown { Style::PLAIN.bold() } else { FAINT };
        if let Some(folder) = folder {
            at += s.put_fit(at, 0, FOLDER_ROOM, folder, FAINT);
            at += s.put(at, 0, 1, "/", FAINT);
        }
        let after_name = 1 + commit.map_or(0, |id| 1 + id.len()) + if closable { close } else { 0 };
        let name_room = (end - at).saturating_sub(after_name).min(NAME_ROOM);
        at += s.put_middle(at, 0, name_room, name, style);
        if let Some(id) = commit {
            at += s.put(at, 0, 1, "@", FAINT);
            at += s.put(at, 0, id.len(), id, FAINT);
        }
        if closable {
            s.put(at + 1, 0, 1, CLOSE, FAINT);
            hits.push((at..at + close + 1, Hit::Close(tab)));
            at += close;
        }
        let end = (at + 1).min(end);
        if shown {
            s.wash(x, 0, end - x, Wash::Bar);
        }
        hits.push((x..end, Hit::Show(tab)));
        end - x
    }

    fn click_strip(&mut self, x: usize) -> After {
        match self.hits.iter().find(|(columns, _)| columns.contains(&x)).map(|(_, hit)| *hit) {
            Some(Hit::Show(tab)) if tab == self.shown => After::Ignored,
            Some(Hit::Show(tab)) => {
                self.show(tab);
                After::Stay
            }
            Some(Hit::Close(tab)) => self.close(tab),
            None => After::Ignored,
        }
    }
}

impl Drop for Tabs {
    /// However the reader ends, the tabs it still had are the last tabs, where each was read to.
    fn drop(&mut self) {
        self.record();
    }
}

impl Pane for Tabs {
    fn draw(&mut self, s: &mut Screen) {
        if let Some(screen) = &mut self.reopen {
            self.strip_rows = 0;
            screen.draw(s);
            self.sync_mark();
            return;
        }
        // Under the strip there must still be a heading, a line of the file and the hints.
        self.strip_rows = usize::from(self.tabs.len() > 1 && s.h >= 4 && s.w > 0);
        let shown = self.shown;
        s.below(self.strip_rows, |s| self.tabs[shown].draw(s));
        if self.strip_rows > 0 {
            self.draw_strip(s);
        }
        self.sync_mark();
    }

    fn help(&self) -> Page {
        use help::{HelpRow, heading, key, note};

        if let Some(screen) = &self.reopen {
            return screen.help();
        }
        let mut page = self.reader().help();
        // First on the page: these keys are what a reader of one file alone does not have.
        let several = [
            key("^N ^P", "next/previous tab"),
            key("^W   ", "close this tab"),
            note("Click a tab, or its ×."),
            note("* marks unsaved edits."),
        ];
        let lists = [key("^T   ", "reopen/save tabs"), HelpRow::new()];
        let several = several.into_iter().filter(|_| self.tabs.len() > 1);
        page.lead(std::iter::once(heading("Tabs")).chain(several).chain(lists));
        page
    }

    fn cover(&mut self) {
        self.reader_mut().cover();
    }

    fn tracks_hover(&self) -> bool {
        self.reopen.is_none() && self.reader().tracks_hover()
    }

    fn hover_span(&self, x: usize, y: usize) -> Option<Range<usize>> {
        if self.reopen.is_some() {
            return None;
        }
        self.reader().hover_span(x, y.checked_sub(self.strip_rows)?)
    }

    fn notice(&self) -> Option<&'static str> {
        self.reader().notice()
    }

    fn watch_scope(&self) -> Scope {
        self.reader().watch_scope()
    }

    fn watch_scope_after_input(&self, current: &Scope) -> Option<Scope> {
        if self.rescope.take() { Some(self.watch_scope()) } else { self.reader().watch_scope_after_input(current) }
    }

    fn git_busy(&self) -> bool {
        self.reader().git_busy()
    }

    fn check(&mut self) -> bool {
        self.reader_mut().check()
    }

    fn external_fd(&self) -> Option<RawFd> {
        self.inbox.as_ref().map(Inbox::fd)
    }

    fn check_external(&mut self) -> External {
        let requests = self.inbox.as_mut().map(Inbox::take).unwrap_or_default();
        if requests.is_empty() {
            return External::Unchanged;
        }
        let now = Instant::now();
        for request in &requests {
            self.open(request, now);
        }
        if let Some(root) = self.memory().map(|memory| memory.root.clone())
            && self.reopen.is_some()
        {
            let list = self.list(&root);
            self.reopen.as_mut().expect("checked above").set_open(list);
        }
        // A tab brought forward is watched from here on; a refusal only has to be read.
        if self.rescope.take() { External::NewScope } else { External::Redraw }
    }

    fn filesystem_changed(&mut self) {
        self.reader_mut().filesystem_changed();
    }

    fn next_frame(&self) -> Option<Duration> {
        self.reader().next_frame()
    }

    fn editing(&self) -> bool {
        self.reopen.as_ref().map_or_else(|| self.reader().editing(), Reopen::typing)
    }

    fn holds_drafts(&self) -> bool {
        self.tabs.iter().any(Reader::unsaved)
    }

    fn paste(&mut self, text: Option<&str>) -> After {
        match &mut self.reopen {
            Some(screen) => {
                let step = text.map_or(Step::Ignored, |text| screen.paste(text));
                self.after_reopen(step)
            }
            None => self.reader_mut().paste(text),
        }
    }

    fn key(&mut self, key: Key) -> After {
        if self.reopen.is_some() {
            return self.reopen_key(key);
        }
        let several = self.tabs.len() > 1;
        match key {
            Key::Ctrl(b'c') if !self.reader().editing() => self.quit(),
            Key::Click(_, y) if y < self.strip_rows && self.reader().actions.is_some() => After::Ignored,
            Key::Click(x, y) if y < self.strip_rows => self.click_strip(x),
            Key::Click(x, y) => self.pass(Key::Click(x, y - self.strip_rows)),
            key if self.reader().actions.is_some() => self.pass(key),
            Key::Ctrl(b't') if !self.reader().asking() => self.show_lists(),
            Key::Ctrl(b'n') if several => self.step(true),
            Key::Ctrl(b'p') if several => self.step(false),
            Key::Ctrl(b'w') => self.close(self.shown),
            key => self.pass(key),
        }
    }
}

/// The screen of lists where no reader is open, and then the reader of the tabs chosen there.
pub struct TabSets {
    root: PathBuf,
    screen: Reopen,
    /// The reader, once a list's tabs were reopened. The screen is not shown again.
    tabs: Option<Tabs>,
}

impl TabSets {
    /// The lists of the worktree that holds `dir`. `last` starts at the preview of its last tabs.
    pub fn open(dir: &Path, last: bool) -> Result<Self, String> {
        let state = rescue::state_dir().ok_or("milo has no state folder to remember tabs in")?;
        let found = dir.canonicalize().ok().and_then(|dir| Repo::open(&dir));
        let repo = found.ok_or_else(|| format!("{} is not a directory", dir.display()))?;
        Ok(Self::at(state, repo.root, last))
    }

    fn at(state: PathBuf, root: PathBuf, last: bool) -> Self {
        let shelf = Shelf::new(state, &root);
        let last_tabs = shelf.last();
        let screen = Reopen::new(shelf, root.clone(), None, last_tabs);
        Self { root, screen: if last { screen.previewing_last() } else { screen }, tabs: None }
    }

    fn screen_key(&mut self, key: Key) -> After {
        match self.screen.key(key) {
            Step::Ignored => After::Ignored,
            Step::Stay => After::Stay,
            Step::Close => After::Leave,
            Step::Reopen(plan) => {
                match Tabs::reopened(&plan, &self.root) {
                    Some(tabs) => self.tabs = Some(tabs),
                    None => self.screen.say("Those tabs could not be reopened."),
                }
                After::Stay
            }
        }
    }
}

impl Pane for TabSets {
    fn draw(&mut self, s: &mut Screen) {
        match &mut self.tabs {
            Some(tabs) => tabs.draw(s),
            None => self.screen.draw(s),
        }
    }

    fn help(&self) -> Page {
        self.tabs.as_ref().map_or_else(|| self.screen.help(), Tabs::help)
    }

    fn cover(&mut self) {
        if let Some(tabs) = &mut self.tabs {
            tabs.cover();
        }
    }

    fn tracks_hover(&self) -> bool {
        self.tabs.as_ref().is_some_and(Tabs::tracks_hover)
    }

    fn hover_span(&self, x: usize, y: usize) -> Option<Range<usize>> {
        self.tabs.as_ref()?.hover_span(x, y)
    }

    fn notice(&self) -> Option<&'static str> {
        self.tabs.as_ref()?.notice()
    }

    fn watch_scope(&self) -> Scope {
        match &self.tabs {
            Some(tabs) => tabs.watch_scope(),
            None => Scope { roots: Vec::new(), ignored: Default::default(), complete: true },
        }
    }

    fn watch_scope_after_input(&self, current: &Scope) -> Option<Scope> {
        self.tabs.as_ref()?.watch_scope_after_input(current)
    }

    fn git_busy(&self) -> bool {
        self.tabs.as_ref().is_some_and(Tabs::git_busy)
    }

    fn check(&mut self) -> bool {
        self.tabs.as_mut().is_none_or(Tabs::check)
    }

    fn external_fd(&self) -> Option<RawFd> {
        self.tabs.as_ref()?.external_fd()
    }

    fn check_external(&mut self) -> External {
        self.tabs.as_mut().map_or(External::Unchanged, Tabs::check_external)
    }

    fn filesystem_changed(&mut self) {
        if let Some(tabs) = &mut self.tabs {
            tabs.filesystem_changed();
        }
    }

    fn next_frame(&self) -> Option<Duration> {
        self.tabs.as_ref()?.next_frame()
    }

    fn editing(&self) -> bool {
        self.tabs.as_ref().map_or_else(|| self.screen.typing(), Tabs::editing)
    }

    fn holds_drafts(&self) -> bool {
        self.tabs.as_ref().is_some_and(Tabs::holds_drafts)
    }

    fn paste(&mut self, text: Option<&str>) -> After {
        match (&mut self.tabs, text) {
            (Some(tabs), text) => tabs.paste(text),
            (None, Some(text)) => match self.screen.paste(text) {
                Step::Stay => After::Stay,
                _ => After::Ignored,
            },
            (None, None) => After::Ignored,
        }
    }

    fn key(&mut self, key: Key) -> After {
        match &mut self.tabs {
            Some(tabs) => tabs.key(key),
            None => self.screen_key(key),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::super::{Floated, Offer};
    use super::*;
    use crate::fixture::Fixture;
    use crate::native::{Action, tests::with_fake};

    const BLOCK: &str = "block:11111111-1111-1111-1111-111111111111";

    /// A state folder inside the fixture, so that a kept draft never reaches the developer's own.
    fn state(f: &Fixture) -> PathBuf {
        f.root.parent().unwrap().join("state")
    }

    fn reader(f: &Fixture, path: &str) -> Reader {
        let mut reader = Reader::open(&f.root.join(path)).unwrap();
        reader.state = Some(state(f));
        reader
    }

    /// A reader of the fixture's files, the first shown, as if each had been opened from the outline in turn.
    fn opened(f: &Fixture, paths: &[&str]) -> Tabs {
        let mut tabs = Tabs::placed(reader(f, paths[0]), None);
        for path in &paths[1..] {
            ask(&mut tabs, f, path);
        }
        tabs.select(0);
        tabs
    }

    /// Asks for one more file, as the outline does.
    fn ask(tabs: &mut Tabs, f: &Fixture, path: &str) {
        drawn(tabs, 80, 8);
        assert!(tabs.open(&Request { file: f.root.join(path), commit: None, spot: None }, Instant::now()));
        tabs.reader_mut().state = Some(state(f));
    }

    /// The frame, with the strip's reminder of its keys left off: one test below is about that.
    fn drawn(tabs: &mut Tabs, w: usize, h: usize) -> Vec<String> {
        let mut rows = with_keys(tabs, w, h);
        if let Some(keys) = KEYS.iter().find(|keys| rows[0].ends_with(*keys)) {
            let strip = rows[0][..rows[0].len() - keys.len()].trim_end().len();
            rows[0].truncate(strip);
        }
        rows
    }

    fn with_keys(tabs: &mut Tabs, w: usize, h: usize) -> Vec<String> {
        let mut screen = Screen::sized(w, h);
        tabs.draw(&mut screen);
        screen.text()
    }

    fn typed(tabs: &mut Tabs, text: &str) {
        for c in text.chars() {
            tabs.key(Key::Char(c));
        }
    }

    /// Clicks the first place the strip shows `text`.
    fn click(tabs: &mut Tabs, text: &str) -> After {
        let strip = drawn(tabs, 80, 8).remove(0);
        let at = strip.find(text).unwrap_or_else(|| panic!("no {text:?} in {strip:?}"));
        tabs.key(Key::Click(str_width(&strip[..at]), 0))
    }

    fn kept_drafts(f: &Fixture) -> Vec<String> {
        let mut drafts: Vec<String> = fs::read_dir(state(f).join("drafts"))
            .map(|entries| entries.map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap()).collect())
            .unwrap_or_default();
        drafts.sort();
        drafts
    }

    #[test]
    fn one_file_draws_as_the_reader_alone_and_a_second_adds_the_strip() {
        let f = Fixture::folder();
        f.write("a.txt", "first\n");
        f.write("b.txt", "second\n");
        let mut tabs = Tabs::placed(reader(&f, "a.txt"), None);
        let mut alone = reader(&f, "a.txt");
        let mut screen = Screen::sized(40, 6);
        alone.draw(&mut screen);
        assert_eq!(drawn(&mut tabs, 40, 6), screen.text());
        let help = tabs.help().text();
        assert!(help.contains("^T") && !help.contains("^N ^P"), "{help}");

        ask(&mut tabs, &f, "b.txt");
        let rows = drawn(&mut tabs, 40, 6);
        assert_eq!(rows[0], " a.txt  b.txt ×");
        assert!(rows[1].contains("b.txt") && rows[2].ends_with("second"), "{rows:?}");
        assert!(rows[5].contains("Actions"));
        let help = tabs.help();
        help.assert_rows_fit(26);
        assert!(help.text().contains("^N ^P") && help.text().contains("^W"), "{}", help.text());

        // A pane too short for a strip and a line of the file shows the file.
        let rows = drawn(&mut tabs, 40, 3);
        assert!(rows[0].contains("b.txt") && !rows[0].contains("a.txt"), "{rows:?}");
        assert!(rows[1].ends_with("second"), "{rows:?}");
    }

    #[test]
    fn a_new_working_tab_reconciles_a_save_before_its_watch_starts() {
        let f = Fixture::new();
        f.write("a.txt", "first\n");
        f.write("b.txt", "before opening\n");
        f.commit("two files");
        let mut tabs = Tabs::placed(reader(&f, "a.txt"), None);
        let previous = tabs.watch_scope();
        ask(&mut tabs, &f, "b.txt");
        f.write("b.txt", "saved in the gap\n");
        assert!(tabs.watch_scope_after_input(&previous).is_some());
        // No filesystem event: the pane's post-open reconciliation must see it.
        assert!(tabs.check());
        assert!(drawn(&mut tabs, 64, 10).iter().any(|row| row.ends_with("saved in the gap")));
    }

    #[test]
    fn a_pane_that_did_not_open_a_reader_reaches_it_only_for_the_readers_own_project() {
        use super::super::ask_reader;

        let f = Fixture::new();
        f.write("a.txt", "first\n");
        f.write("b.txt", "one\ntwo\nthree\n");
        f.commit("two files");
        let other = Fixture::new();
        other.write("b.txt", "another project\n");
        let root = f.root.canonicalize().unwrap();
        let place = Place { state: state(&f), block: BLOCK.into() };
        let mut tabs = Tabs::placed(reader(&f, "a.txt"), Some(place));
        let spot = Some(crate::reader::Spot { line: 2, column: None });

        let elsewhere = other.root.canonicalize().unwrap();
        assert!(!ask_reader(&state(&f), BLOCK, &elsewhere, &elsewhere.join("b.txt"), spot));
        assert!(matches!(tabs.check_external(), External::Unchanged));
        assert!(ask_reader(&state(&f), BLOCK, &root, &root.join("b.txt"), spot));
        assert!(matches!(tabs.check_external(), External::NewScope));
        let rows = drawn(&mut tabs, 40, 8);
        assert_eq!((rows[0].as_str(), rows[7].as_str()), (" a.txt  b.txt ×", " Line 2 · ? help"));
    }

    #[test]
    fn a_tab_keeps_its_place_draft_cursor_and_undo_history_while_another_is_shown() {
        let f = Fixture::folder();
        let long: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        f.write("long.txt", &long);
        f.write("b.txt", "one\ntwo\nthree\n");
        let mut tabs = Tabs::placed(reader(&f, "long.txt"), None);
        drawn(&mut tabs, 40, 8);
        for _ in 0..5 {
            tabs.key(Key::Char('j'));
        }
        let rows = drawn(&mut tabs, 40, 8);
        assert!(rows[1].starts_with("  6   line 6"), "{rows:?}");

        ask(&mut tabs, &f, "b.txt");
        tabs.key(Key::Char('e'));
        drawn(&mut tabs, 40, 8);
        tabs.key(Key::Down);
        tabs.key(Key::End);
        typed(&mut tabs, "!");
        let rows = drawn(&mut tabs, 40, 8);
        assert_eq!(rows[0], " long.txt  *b.txt ×");
        assert!(rows[1].ends_with("unsaved") && rows[3].ends_with("two!"), "{rows:?}");

        // The first file is where it was left, and reads as a file, not as the other tab's draft.
        assert_eq!(tabs.key(Key::Ctrl(b'p')), After::Stay);
        let rows = drawn(&mut tabs, 40, 8);
        assert_eq!(rows[0], " long.txt ×  *b.txt");
        assert!(rows[2].starts_with("  6   line 6") && rows[7].contains("Actions"), "{rows:?}");
        assert!(!tabs.editing());
        tabs.key(Key::Char('j'));

        // Back in the draft, the cursor is still after what was typed, and undo still takes the typing back.
        assert_eq!(tabs.key(Key::Ctrl(b'n')), After::Stay);
        assert!(tabs.editing());
        typed(&mut tabs, "?");
        let rows = drawn(&mut tabs, 40, 8);
        assert!(rows[3].ends_with("two!?"), "{rows:?}");
        tabs.key(Key::Ctrl(b'z'));
        let rows = drawn(&mut tabs, 40, 8);
        assert_eq!(rows[0], " long.txt  b.txt ×");
        assert!(rows[1].ends_with("editing") && rows[3].ends_with("two"), "{rows:?}");
        tabs.key(Key::Ctrl(b'y'));
        assert!(drawn(&mut tabs, 40, 8)[3].ends_with("two!?"));
        assert_eq!(fs::read_to_string(f.root.join("b.txt")).unwrap(), "one\ntwo\nthree\n");

        tabs.key(Key::Ctrl(b'n'));
        assert!(drawn(&mut tabs, 40, 8)[2].starts_with("  7   line 7"));
    }

    #[test]
    fn native_actions_use_the_shown_tab_even_while_another_has_a_draft() {
        let f = Fixture::folder();
        f.write("draft.txt", "original\n");
        f.write("page.md", "# Preview\n");
        let mut tabs = opened(&f, &["draft.txt", "page.md"]);
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "draft ");
        let draft = drawn(&mut tabs, 80, 6);
        assert!(tabs.editing() && tabs.holds_drafts());

        assert_eq!(tabs.key(Key::Ctrl(b'n')), After::Stay);
        assert!(!tabs.editing() && tabs.holds_drafts());
        assert!(tabs.reader().markdown().is_some() && !tabs.reader().markdown_source);
        let target = f.root.join("page.md");
        let ((), opened) = with_fake(Ok(()), || {
            assert_eq!(tabs.key(Key::Ctrl(b'o')), After::Stay);
            assert!(drawn(&mut tabs, 80, 6)[5].contains("Opened in default app."));
            assert_eq!(tabs.key(Key::Ctrl(b'r')), After::Stay);
            assert!(drawn(&mut tabs, 80, 6)[5].contains("Revealed in Finder."));
        });
        assert!(
            matches!(opened.as_slice(), [(Action::Open, first), (Action::Reveal, second)] if first == &target && second == &target)
        );
        assert!(tabs.reader().markdown().is_some() && !tabs.reader().markdown_source);

        let denied = "Native actions are unavailable here.";
        let (after, opened) = with_fake(Err(denied), || tabs.key(Key::Ctrl(b'r')));
        assert_eq!(after, After::Stay);
        assert!(opened.is_empty());
        assert!(drawn(&mut tabs, 80, 6)[5].contains(denied));

        assert_eq!(tabs.key(Key::Ctrl(b'p')), After::Stay);
        assert_eq!(drawn(&mut tabs, 80, 6), draft);
        assert_eq!(fs::read_to_string(f.root.join("draft.txt")).unwrap(), "original\n");
    }

    #[test]
    fn actions_capture_tab_keys_and_clicks_without_outliving_the_shown_tab() {
        let f = Fixture::folder();
        f.write("one.md", "# First\n");
        f.write("two.txt", "second\n");
        let mut tabs = opened(&f, &["one.md", "two.txt"]);
        let rows = drawn(&mut tabs, 80, 9);
        let at = str_width(rows[8].split_once("a Actions").unwrap().0);
        assert_eq!(tabs.hover_span(at, 8), Some(at..at + "a Actions".len()));
        assert_eq!(tabs.key(Key::Click(at, 8)), After::Stay);
        let menu = drawn(&mut tabs, 80, 9);
        assert_eq!(rows[0], menu[0]);
        assert!(menu[1].contains("one.md") && menu[1].contains("Actions"), "{menu:?}");
        assert!(menu.iter().any(|row| row.contains("tkmp")), "{menu:?}");
        assert_eq!(tabs.key(Key::Ctrl(b'n')), After::Stay);
        assert_eq!(tabs.key(Key::Ctrl(b'p')), After::Stay);
        assert_eq!(tabs.key(Key::Ctrl(b'w')), After::Stay);
        assert_eq!(click(&mut tabs, "two.txt"), After::Ignored);
        assert_eq!(click(&mut tabs, "×"), After::Ignored);
        assert_eq!(drawn(&mut tabs, 80, 9), menu);

        let edit = menu.iter().position(|row| row.contains("Edit in Milo")).unwrap();
        assert!(tabs.hover_span(3, edit).is_some());
        assert_eq!(tabs.key(Key::Click(3, edit)), After::Stay);
        assert!(tabs.editing() && tabs.reader().path == "one.md");
        assert_eq!(tabs.key(Key::Esc), After::Stay);

        assert_eq!(tabs.key(Key::Char('a')), After::Stay);
        assert!(drawn(&mut tabs, 80, 9)[1].contains("Actions"));
        // This request comes from outside the Reader, not from its tab strip.
        ask(&mut tabs, &f, "two.txt");
        assert!(drawn(&mut tabs, 80, 9)[2].ends_with("second"));
        assert_eq!(tabs.key(Key::Enter), After::Ignored);
        assert_eq!(tabs.key(Key::Ctrl(b'p')), After::Stay);
        assert_eq!(tabs.reader().path, "one.md");
        assert!(!drawn(&mut tabs, 80, 9)[1].contains("Actions"));
    }

    #[test]
    fn opening_a_file_that_has_a_tab_shows_that_tab_and_its_draft() {
        let f = Fixture::new();
        f.write("src/a.txt", "first\n");
        f.write("b.txt", "second\n");
        f.commit("first");
        let mut tabs = opened(&f, &["src/a.txt", "b.txt"]);
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "draft ");
        tabs.key(Key::Ctrl(b'n'));
        assert_eq!(drawn(&mut tabs, 40, 6)[0], " *a.txt  b.txt ×");

        // The same file by another spelling of its path is still the same file.
        let again = Request { file: f.root.join("src/../src/./a.txt"), commit: None, spot: None };
        assert!(tabs.open(&again, Instant::now()));
        let rows = drawn(&mut tabs, 40, 6);
        assert_eq!(rows[0], " *a.txt ×  b.txt");
        assert!(rows[2].ends_with("draft first"), "{rows:?}");

        // The file as of a commit is another thing to read, and is its own tab once.
        let mut repo = crate::git::Repo::open(&f.root).unwrap();
        repo.read();
        let hash = repo.history(1).unwrap()[0].hash.clone();
        let at_commit = Request { file: f.root.join("b.txt"), commit: Some(hash.clone()), spot: None };
        for _ in 0..2 {
            assert!(tabs.open(&at_commit, Instant::now()));
        }
        assert_eq!(drawn(&mut tabs, 60, 6)[0], format!(" *a.txt  b.txt  b.txt@{} ×", &hash[..7]));

        // What cannot be opened is said, and no tab is made for it.
        let not_hex = Request { file: "b.txt".into(), commit: Some("not hex".into()), spot: None };
        assert!(!tabs.open(&not_hex, Instant::now()));
        let rows = drawn(&mut tabs, 60, 6);
        assert_eq!(rows[5], " That is not a commit ID. · ? help");
        assert_eq!(rows[0].matches("b.txt").count(), 2);
    }

    #[test]
    fn a_file_asked_for_at_a_line_shows_that_line_and_leaves_a_draft_as_it_was() {
        let f = Fixture::new();
        let numbered: String = (1..=60).map(|n| format!("line {n}\n")).collect();
        f.write("long.txt", &numbered);
        f.write("b.txt", "second\n");
        f.commit("first");
        let mut tabs = opened(&f, &["b.txt"]);
        let at = |line, column| {
            let spot = Some(crate::reader::Spot { line, column });
            Request { file: f.root.join("long.txt"), commit: None, spot }
        };

        // A new tab opens with the line in view, a few lines above it, and says where it was sent.
        assert!(tabs.open(&at(40, Some(3)), Instant::now()));
        tabs.reader_mut().state = Some(state(&f));
        let rows = drawn(&mut tabs, 40, 9);
        assert_eq!(rows[0], " b.txt  long.txt ×");
        assert!(rows[2].trim_start().starts_with("37   line 37") && rows[5].trim_start().starts_with("40   line 40"));
        assert_eq!(rows[8], " Line 40:3 · ? help");

        // Editing starts at the place. A draft keeps its text and unsaved mark when the file
        // is asked for again; only its cursor goes to the new place.
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "draft ");
        tabs.key(Key::Ctrl(b'p'));
        assert!(tabs.open(&at(2, Some(6)), Instant::now()));
        let rows = drawn(&mut tabs, 40, 9);
        assert_eq!(rows[0], " b.txt  *long.txt ×");
        typed(&mut tabs, "!");
        tabs.key(Key::Ctrl(b's'));
        let saved = fs::read_to_string(f.root.join("long.txt")).unwrap();
        assert!(saved.starts_with("line 1\nline !2\n") && saved.contains("\nlidraft ne 40\n"), "{saved}");

        // A line past the end of the file shows the end.
        tabs.key(Key::Esc);
        assert!(tabs.open(&at(900, None), Instant::now()));
        let rows = drawn(&mut tabs, 40, 9);
        assert!(rows[7].contains("60   line 60") && rows[8].starts_with(" Line 900"), "{rows:?}");
    }

    #[test]
    fn working_tabs_keep_directory_aliases_distinct_and_follow_retargets_when_shown() {
        use std::os::unix::fs::symlink;

        let f = Fixture::new();
        f.write("one/page.txt", "original body\n");
        f.write("two/page.txt", "retargeted body\n");
        f.commit("two directories");
        let alias = f.root.join("alias");
        symlink("one", &alias).unwrap();
        let mut tabs = Tabs::placed(reader(&f, "one/page.txt"), None);
        ask(&mut tabs, &f, "alias/page.txt");
        assert_eq!(tabs.tabs.len(), 2);
        assert!(drawn(&mut tabs, 80, 6)[2].ends_with("original body"));
        tabs.key(Key::Ctrl(b'p'));

        fs::remove_file(&alias).unwrap();
        symlink("two", &alias).unwrap();
        ask(&mut tabs, &f, "alias/page.txt");
        assert_eq!(tabs.tabs.len(), 2);
        assert!(drawn(&mut tabs, 80, 6)[2].ends_with("retargeted body"));
    }

    #[test]
    fn working_tab_identity_is_not_the_lossy_label() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let f = Fixture::folder();
        f.write("page\u{fffd}.txt", "body of the UTF-8 file\n");
        let mut tabs = Tabs::placed(reader(&f, "page\u{fffd}.txt"), None);
        let request = Request { file: f.root.join(OsStr::from_bytes(b"page\xff.txt")), commit: None, spot: None };
        for _ in 0..2 {
            assert!(tabs.open(&request, Instant::now()));
        }
        assert_eq!(tabs.tabs.len(), 2);
        assert_eq!(tabs.shown, 1);
        assert_eq!(tabs.tabs[0].path, tabs.tabs[1].path);
    }

    #[test]
    fn a_replaced_project_root_does_not_reuse_a_working_tab() {
        let f = Fixture::folder();
        f.write("page.txt", "old body\n");
        let mut tabs = Tabs::placed(reader(&f, "page.txt"), None);
        fs::rename(&f.root, f.root.with_file_name("previous-project")).unwrap();
        f.write("page.txt", "new body\n");
        ask(&mut tabs, &f, "page.txt");
        assert_eq!(tabs.tabs.len(), 2);
        assert!(drawn(&mut tabs, 80, 6)[2].ends_with("new body"));
    }

    #[test]
    fn commit_tabs_use_the_full_hash_and_refuse_an_abbreviation_that_becomes_ambiguous() {
        const FIRST: &str = "1c299ef8d83dfc68f9618e553977b5d69c5baae3";
        const SECOND: &str = "1c299ef48afd14d6aede3a4a71160692f42f1f79";
        const SHORT: &str = "1c299ef";

        // These SHA-1 commit objects have the same seven visible hex digits.
        let f = Fixture::folder();
        f.git(&["init", "-q", "--template=", "-b", "main", "--object-format=sha1"]);
        let put_commit = |text, tree, subject| {
            f.write("page.txt", text);
            f.git(&["add", "--", "page.txt"]);
            f.git(&["write-tree"]);
            let object = f.root.parent().unwrap().join("commit-object");
            fs::write(
                &object,
                format!(
                    "tree {tree}\nauthor Milo Test <test@example.com> 981173106 +0000\n\
                     committer Milo Test <test@example.com> 981173106 +0000\n\n{subject}\n"
                ),
            )
            .unwrap();
            f.git(&["hash-object", "-w", "-t", "commit", object.to_str().unwrap()]);
        };
        put_commit("original commit body\n", "ca3065e5729199ac4562442eea80513faff45a76", "First 21709");
        let mut tabs = Tabs::placed(reader(&f, "page.txt"), None);
        let open = |tabs: &mut Tabs, id: &str| {
            let request = Request { file: f.root.join("page.txt"), commit: Some(id.into()), spot: None };
            tabs.open(&request, Instant::now())
        };
        assert!(open(&mut tabs, SHORT));
        assert!(open(&mut tabs, FIRST));
        assert_eq!(tabs.tabs.len(), 2);
        assert!(drawn(&mut tabs, 80, 6)[2].ends_with("original commit body"));
        put_commit("colliding commit body\n", "2f530c2257e83f9c24bf86c70ccd31251ec81f0d", "Second 3305");

        tabs.key(Key::Ctrl(b'p'));
        assert!(!open(&mut tabs, SHORT));
        assert_eq!(tabs.shown, 0);
        assert!(open(&mut tabs, SECOND));
        assert_eq!(tabs.tabs.len(), 3);
        assert!(drawn(&mut tabs, 80, 6)[2].ends_with("colliding commit body"));
    }

    #[test]
    fn a_commit_tab_does_not_bypass_a_new_shallow_boundary_or_match_another_project() {
        let f = Fixture::new();
        f.write("page.txt", "parent body\n");
        f.commit("parent");
        f.write("page.txt", "commit body\n");
        f.commit("child");
        let mut repo = crate::git::Repo::open(&f.root).unwrap();
        repo.read();
        let hash = repo.history(1).unwrap()[0].hash.clone();
        let other = Fixture::folder();
        other.git(&["clone", "-q", "--no-local", f.root.to_str().unwrap(), "other"]);
        let mut tabs = Tabs::placed(reader(&f, "page.txt"), None);
        let open = |tabs: &mut Tabs, root: &Path| {
            let request = Request { file: root.join("page.txt"), commit: Some(hash.clone()), spot: None };
            tabs.open(&request, Instant::now())
        };
        assert!(open(&mut tabs, &f.root));
        assert!(open(&mut tabs, &other.root.join("other")));
        assert_eq!(tabs.tabs.len(), 3);
        fs::write(f.root.join(".git/shallow"), format!("{hash}\n")).unwrap();
        assert!(!open(&mut tabs, &f.root));
        assert_eq!(tabs.shown, 2);
        assert!(drawn(&mut tabs, 80, 6)[5].contains("Git did not answer for this commit."));
    }

    #[test]
    fn the_strip_says_which_tab_is_shown_and_which_have_unsaved_edits_however_narrow() {
        let f = Fixture::folder();
        for path in ["alpha.txt", "beta.txt", "gamma.txt", "src/mod.rs", "tests/mod.rs"] {
            f.write(path, "text\n");
        }
        let mut tabs = opened(&f, &["alpha.txt", "beta.txt", "gamma.txt"]);
        click(&mut tabs, "beta");
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "x");
        tabs.key(Key::Ctrl(b'n'));

        assert_eq!(drawn(&mut tabs, 60, 6)[0], " alpha.txt  *beta.txt  gamma.txt ×");
        // Tabs that do not fit whole are counted out at the edge, with their unsaved edits.
        assert_eq!(drawn(&mut tabs, 30, 6)[0], "‹  *beta.txt  gamma.txt ×");
        assert_eq!(drawn(&mut tabs, 12, 6)[0], "‹* gamma… ×");
        assert_eq!(drawn(&mut tabs, 9, 6)[0], "‹* gamm…");
        assert_eq!(drawn(&mut tabs, 3, 6)[0], "‹*");
        tabs.key(Key::Ctrl(b'n'));
        assert_eq!(drawn(&mut tabs, 15, 6)[0], " alpha.txt × *›");
        tabs.key(Key::Ctrl(b'n'));
        assert_eq!(drawn(&mut tabs, 17, 6)[0], "‹  *beta.txt ×  ›");

        // The edge is a way to the tab beyond it.
        assert_eq!(tabs.key(Key::Click(16, 0)), After::Stay);
        assert_eq!(drawn(&mut tabs, 60, 6)[0], " alpha.txt  *beta.txt  gamma.txt ×");

        // Files of one name are told apart by their folders.
        ask(&mut tabs, &f, "src/mod.rs");
        ask(&mut tabs, &f, "tests/mod.rs");
        assert_eq!(drawn(&mut tabs, 80, 6)[0], " alpha.txt  *beta.txt  gamma.txt  src/mod.rs  tests/mod.rs ×");
    }

    #[test]
    fn the_strip_names_its_keys_where_every_tab_is_shown_and_room_is_left() {
        let f = Fixture::folder();
        for name in ["alpha.txt", "beta.txt", "gamma.txt"] {
            f.write(name, "text\n");
        }
        let mut tabs = opened(&f, &["alpha.txt", "beta.txt", "gamma.txt"]);
        let strip = " alpha.txt ×  beta.txt  gamma.txt";
        assert_eq!(with_keys(&mut tabs, 60, 6)[0], format!("{strip}     ^N ^P tabs · ^W close"));
        // Two columns stay between the last tab and the keys.
        assert_eq!(with_keys(&mut tabs, 57, 6)[0], format!("{strip}  ^N ^P tabs · ^W close"));
        assert_eq!(with_keys(&mut tabs, 56, 6)[0], format!("{strip}            ^N ^P tabs"));
        assert_eq!(with_keys(&mut tabs, 46, 6)[0], format!("{strip}  ^N ^P tabs"));
        assert_eq!(with_keys(&mut tabs, 45, 6)[0], strip);
        // Where tabs are counted out at an edge, the names keep the room.
        assert_eq!(with_keys(&mut tabs, 17, 6)[0], " alpha.txt ×    ›");
        // A click on the keys is a click on nothing.
        assert_eq!(tabs.key(Key::Click(50, 0)), After::Ignored);
        // The help page says them first, where a short pane shows them without scrolling.
        assert!(tabs.help().text().starts_with("Tabs\n  ^N ^P  next/previous tab\n  ^W     close this tab\n"));
    }

    #[test]
    fn a_click_shows_a_tab_or_closes_it_and_a_click_below_the_strip_reaches_the_file() {
        let f = Fixture::folder();
        f.write("a.txt", "one\ntwo\nthree\n");
        f.write("b.txt", "second\n");
        let mut tabs = opened(&f, &["a.txt", "b.txt"]);
        assert_eq!(click(&mut tabs, "a.txt"), After::Ignored);
        assert_eq!(click(&mut tabs, "b.txt"), After::Stay);
        assert_eq!(drawn(&mut tabs, 40, 8)[0], " a.txt  b.txt ×");
        assert_eq!(click(&mut tabs, "a.txt"), After::Stay);

        // The draft's first line is the third row of the pane now, and a click there is on that line.
        tabs.key(Key::Char('e'));
        let rows = drawn(&mut tabs, 40, 8);
        let column = str_width(&rows[3][..rows[3].find("two").unwrap()]);
        tabs.key(Key::Click(column + 3, 3));
        typed(&mut tabs, "!");
        assert!(drawn(&mut tabs, 40, 8)[3].ends_with("two!"));
        tabs.key(Key::Ctrl(b'z'));

        assert_eq!(click(&mut tabs, "×"), After::Stay);
        let rows = drawn(&mut tabs, 40, 8);
        assert!(rows[0].contains("b.txt") && !rows[0].contains('×') && rows[1].ends_with("second"), "{rows:?}");
        assert_eq!(tabs.key(Key::Char('q')), After::Leave);
    }

    #[test]
    fn closing_a_tab_asks_about_unsaved_edits_and_the_last_tab_closes_the_reader() {
        let f = Fixture::folder();
        f.write("a.txt", "first\n");
        f.write("b.txt", "second\n");
        f.write("c.txt", "third\n");
        let mut tabs = opened(&f, &["a.txt", "b.txt", "c.txt"]);
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "draft ");

        // Going back from the question keeps the tab and the draft.
        assert_eq!(tabs.key(Key::Ctrl(b'w')), After::Stay);
        let rows = drawn(&mut tabs, 60, 6);
        assert_eq!(rows[5], " Unsaved edits: s save · d discard · Esc back");
        tabs.key(Key::Esc);
        let rows = drawn(&mut tabs, 60, 6);
        assert_eq!(rows[0], " *a.txt ×  b.txt  c.txt");
        assert!(rows[2].ends_with("draft first"), "{rows:?}");
        // Having gone back, ending the edit some other way does not close the tab after all.
        tabs.key(Key::Ctrl(b's'));
        tabs.key(Key::Esc);
        assert_eq!(drawn(&mut tabs, 60, 6)[0], " a.txt ×  b.txt  c.txt");
        assert_eq!(fs::read_to_string(f.root.join("a.txt")).unwrap(), "draft first\n");

        // Saving from the question closes the tab; so does giving the draft up, and neither touches another file.
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "more ");
        tabs.key(Key::Ctrl(b'w'));
        assert_eq!(tabs.key(Key::Char('s')), After::Stay);
        assert_eq!(drawn(&mut tabs, 60, 6)[0], " b.txt ×  c.txt");
        assert_eq!(fs::read_to_string(f.root.join("a.txt")).unwrap(), "more draft first\n");
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "lost ");
        click(&mut tabs, "×");
        assert_eq!(tabs.key(Key::Char('d')), After::Stay);
        assert_eq!(fs::read_to_string(f.root.join("b.txt")).unwrap(), "second\n");
        assert!(kept_drafts(&f).is_empty());

        // One tab is left: no strip, and closing it closes the reader as it always did.
        let rows = drawn(&mut tabs, 60, 6);
        assert!(rows[0].contains("c.txt") && rows[1].ends_with("third"), "{rows:?}");
        tabs.key(Key::Char('e'));
        assert_eq!(tabs.key(Key::Ctrl(b'w')), After::Leave);
    }

    #[test]
    fn ctrl_c_asks_about_each_unsaved_tab_before_it_closes_the_reader() {
        let f = Fixture::folder();
        f.write("a.txt", "first\n");
        f.write("b.txt", "second\n");
        f.write("c.txt", "third\n");
        let mut tabs = opened(&f, &["a.txt", "b.txt", "c.txt"]);
        assert!(!tabs.holds_drafts());
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "draft ");
        tabs.key(Key::Ctrl(b'n'));
        tabs.key(Key::Ctrl(b'n'));
        assert!(tabs.holds_drafts() && !tabs.editing());

        // From a tab that is only being read, Ctrl-C goes to the draft that would be lost.
        assert_eq!(tabs.key(Key::Ctrl(b'c')), After::Stay);
        let rows = drawn(&mut tabs, 60, 6);
        assert_eq!(rows[0], " *a.txt ×  b.txt  c.txt");
        assert_eq!(rows[5], " Unsaved edits: s save · d discard · Esc back");
        tabs.key(Key::Char('s'));
        assert_eq!(fs::read_to_string(f.root.join("a.txt")).unwrap(), "draft first\n");
        assert_eq!(drawn(&mut tabs, 60, 6)[0], " a.txt ×  b.txt  c.txt");
        assert!(!tabs.holds_drafts());
        assert_eq!(tabs.key(Key::Ctrl(b'c')), After::Leave);
    }

    #[test]
    fn a_reader_ended_from_outside_keeps_the_draft_of_every_tab_and_offers_each_again() {
        let f = Fixture::folder();
        f.write("a.txt", "first\n");
        f.write("b.txt", "second\n");
        f.write("c.txt", "third\n");
        let mut tabs = opened(&f, &["a.txt", "b.txt", "c.txt"]);
        for draft in ["one ", "two "] {
            tabs.key(Key::Char('e'));
            typed(&mut tabs, draft);
            tabs.key(Key::Ctrl(b'n'));
        }
        drop(tabs);
        assert_eq!(kept_drafts(&f), ["one first\n", "two second\n"]);
        assert_eq!(fs::read_to_string(f.root.join("a.txt")).unwrap(), "first\n");

        let mut tabs = opened(&f, &["c.txt", "b.txt"]);
        tabs.key(Key::Ctrl(b'n'));
        tabs.key(Key::Char('e'));
        assert_eq!(drawn(&mut tabs, 60, 6)[5], " Kept draft found: r restore · d delete · Esc later");
        tabs.key(Key::Char('r'));
        tabs.key(Key::Ctrl(b's'));
        assert_eq!(fs::read_to_string(f.root.join("b.txt")).unwrap(), "two second\n");
        assert_eq!(kept_drafts(&f), ["one first\n"]);
    }

    #[test]
    fn a_tab_left_with_a_draft_still_refuses_to_save_over_a_file_changed_meanwhile() {
        let f = Fixture::folder();
        f.write("a.txt", "first\n");
        f.write("b.txt", "second\n");
        let mut tabs = opened(&f, &["a.txt", "b.txt"]);
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "draft ");
        tabs.key(Key::Ctrl(b'n'));
        f.write("a.txt", "changed by something else\n");
        // The tab that was only being read shows the file as it is now when it comes back.
        f.write("b.txt", "second, changed\n");
        tabs.key(Key::Ctrl(b'p'));
        tabs.key(Key::Ctrl(b's'));
        let rows = drawn(&mut tabs, 60, 6);
        assert_eq!(rows[5], " Changed on disk: c compare · o overwrite · Esc back");
        assert_eq!(fs::read_to_string(f.root.join("a.txt")).unwrap(), "changed by something else\n");
        tabs.key(Key::Esc);
        tabs.key(Key::Ctrl(b'n'));
        assert!(drawn(&mut tabs, 60, 6)[2].ends_with("second, changed"));
    }

    #[test]
    fn a_request_from_another_pane_opens_a_tab_and_the_block_is_marked_while_any_tab_is_unsaved() {
        let f = Fixture::folder();
        f.write("a.txt", "first\n");
        f.write("b.txt", "second\n");
        let state = state(&f);
        let place = Place { state: state.clone(), block: BLOCK.into() };
        let mut tabs = Tabs::placed(reader(&f, "a.txt"), Some(place));
        assert!(tabs.external_fd().is_some());
        assert!(matches!(tabs.check_external(), External::Unchanged));

        let open = Floated { block: BLOCK.into(), root: f.root.clone() };
        let elsewhere = Path::new("/another/project");
        assert_eq!(open.offer(&state, &f.root, &f.root.join("b.txt"), None), Offer::Taken);
        assert!(matches!(tabs.check_external(), External::NewScope));
        tabs.reader_mut().state = Some(state.clone());
        assert_eq!(drawn(&mut tabs, 40, 6)[0], " a.txt  b.txt ×");
        // Asked for again, the tab is not made twice.
        assert_eq!(open.offer(&state, &f.root, &f.root.join("b.txt"), None), Offer::Taken);
        assert!(matches!(tabs.check_external(), External::Redraw));
        assert_eq!(drawn(&mut tabs, 40, 6)[0], " a.txt  b.txt ×");

        // A file of another project is never a tab here: its reader is replaced, unless that loses edits.
        assert_eq!(open.offer(&state, elsewhere, &elsewhere.join("c.txt"), None), Offer::Replace);
        for tab in 0..2 {
            tabs.key(Key::Char('e'));
            typed(&mut tabs, "x");
            drawn(&mut tabs, 40, 6);
            assert_eq!(open.offer(&state, elsewhere, &elsewhere.join("c.txt"), None), Offer::Unsaved, "{tab}");
            tabs.key(Key::Ctrl(b'n'));
        }
        // One draft saved leaves the other, and the block stays marked for it.
        tabs.key(Key::Ctrl(b's'));
        drawn(&mut tabs, 40, 6);
        assert_eq!(open.offer(&state, elsewhere, &elsewhere.join("c.txt"), None), Offer::Unsaved);
        tabs.key(Key::Ctrl(b'n'));
        tabs.key(Key::Ctrl(b's'));
        drawn(&mut tabs, 40, 6);
        assert_eq!(open.offer(&state, elsewhere, &elsewhere.join("c.txt"), None), Offer::Replace);
        assert!(matches!(tabs.check_external(), External::Unchanged), "the other project's file never arrived");

        // A reader that is gone takes nothing, so its pane opens another.
        drop(tabs);
        assert_eq!(open.offer(&state, &f.root, &f.root.join("b.txt"), None), Offer::Replace);
    }

    /// A reader of the fixture's files, the first shown, whose tabs are remembered in the fixture's state folder.
    fn remembered(f: &Fixture, paths: &[&str]) -> Tabs {
        let mut tabs = Tabs::placed(reader(f, paths[0]), None).remembering(state(f));
        for path in &paths[1..] {
            ask(&mut tabs, f, path);
        }
        tabs.select(0);
        tabs
    }

    fn shelf(tabs: &Tabs) -> Shelf {
        tabs.memory().unwrap().shelf()
    }

    fn paths(list: &List) -> Vec<&str> {
        list.entries.iter().map(|entry| entry.path.as_str()).collect()
    }

    fn pressed(tabs: &mut impl Pane, keys: &[Key]) {
        for key in keys {
            tabs.key(*key);
        }
    }

    /// Saves the reader's tabs as `name` through the screen of lists, and leaves that screen.
    fn save_as(tabs: &mut Tabs, name: &str) {
        pressed(tabs, &[Key::Ctrl(b't'), Key::Char('s')]);
        typed(tabs, name);
        pressed(tabs, &[Key::Enter, Key::Esc]);
    }

    fn frame(pane: &mut impl Pane, w: usize, h: usize) -> Vec<String> {
        let mut screen = Screen::sized(w, h);
        pane.draw(&mut screen);
        screen.text()
    }

    fn head(f: &Fixture) -> String {
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        repo.history(1).unwrap()[0].hash.clone()
    }

    fn numbered(lines: usize) -> String {
        (1..=lines).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn closing_a_tab_drops_it_from_the_last_tabs_and_closing_the_reader_keeps_the_rest() {
        let f = Fixture::new();
        f.write("a.txt", "first\n");
        f.write("b.txt", "second\n");
        f.write("long.txt", &numbered(60));
        f.commit("three files");
        let mut tabs = remembered(&f, &["a.txt", "b.txt", "long.txt"]);
        let shelf = shelf(&tabs);
        assert_eq!(paths(&shelf.last().unwrap()), ["a.txt", "b.txt", "long.txt"]);

        // One tab closed is one tab forgotten.
        pressed(&mut tabs, &[Key::Ctrl(b'n'), Key::Ctrl(b'w')]);
        let last = shelf.last().unwrap();
        assert_eq!((paths(&last), last.active), (vec!["a.txt", "long.txt"], 1));

        // The reader ends with two tabs, and they stay, each where it was read to.
        drawn(&mut tabs, 40, 10);
        for _ in 0..25 {
            tabs.key(Key::Char('j'));
        }
        assert_eq!(tabs.key(Key::Ctrl(b'c')), After::Leave);
        drop(tabs);
        let last = shelf.last().unwrap();
        assert_eq!(paths(&last), ["a.txt", "long.txt"]);
        assert_eq!((last.active, last.entries[1].line, &last.entries[1].commit), (1, 25, &None));

        // The last tab of a reader closes the reader, so it is still the last tabs.
        let mut tabs = Tabs::placed(reader(&f, "b.txt"), None).remembering(state(&f));
        assert_eq!(tabs.key(Key::Char('q')), After::Leave);
        drop(tabs);
        assert_eq!(paths(&shelf.last().unwrap()), ["b.txt"]);
        assert!(shelf.named().is_empty());
    }

    #[test]
    fn a_set_is_previewed_with_its_versions_and_only_what_is_there_and_selected_reopens() {
        const NOWHERE: &str = "ca3065e5729199ac4562442eea80513faff45a76";
        let f = Fixture::new();
        f.write("a.txt", "first\n");
        f.write("b.txt", "as it was\n");
        f.write("gone.txt", "soon gone\n");
        f.commit("one");
        let old = head(&f);
        f.write("b.txt", "as it is\n");
        f.commit("two");
        let mut tabs = remembered(&f, &["a.txt", "b.txt", "gone.txt"]);
        let at_commit = Entry { path: "b.txt".into(), commit: Some(old.clone()), line: 0 };
        tabs.restore(&Plan { entries: vec![at_commit], active: Some(0) }, Instant::now());
        save_as(&mut tabs, "Rex compatibility");
        let (shelf, root) = (shelf(&tabs), tabs.memory().unwrap().root.clone());
        drop(tabs);

        // A file is deleted, and a set names a commit this repository never had.
        fs::remove_file(f.root.join("gone.txt")).unwrap();
        let mut list = shelf.named().remove(0).list;
        assert_eq!((list.entries.len(), list.active, list.entries[3].commit.as_deref()), (4, 3, Some(old.as_str())));
        list.entries.push(Entry { path: "a.txt".into(), commit: Some(NOWHERE.into()), line: 0 });
        shelf.save("Rex compatibility", &list, crate::reader::sets::Collision::Replace).unwrap();

        let mut sets = TabSets::at(state(&f), root, false);
        let rows = frame(&mut sets, 72, 12);
        assert!(rows[0].starts_with(" Reader tabs"), "{rows:?}");
        assert!(rows[1].starts_with(" › Last tabs") && rows[1].ends_with("4 tabs"), "{rows:?}");
        assert!(rows[2].starts_with("   Rex compatibility") && rows[2].ends_with("5 tabs"), "{rows:?}");
        sets.help().assert_rows_fit(26);

        pressed(&mut sets, &[Key::Down, Key::Enter]);
        let rows = frame(&mut sets, 72, 12);
        assert!(rows[0].starts_with(" Reopen  Rex compatibility") && rows[0].ends_with("3 of 5 selected"), "{rows:?}");
        assert!(rows[1].starts_with(" › [x] a.txt") && rows[1].ends_with(" working file"), "{rows:?}");
        assert!(rows[2].starts_with("   [x] b.txt") && rows[2].ends_with(" working file"), "{rows:?}");
        assert!(rows[3].starts_with("    –  gone.txt") && rows[3].ends_with(" missing file"), "{rows:?}");
        assert!(rows[4].starts_with("   [x] b.txt"), "{rows:?}");
        assert!(rows[4].ends_with(&format!(" shown · commit {}", &old[..12])), "{rows:?}");
        assert!(rows[5].starts_with("    –  a.txt"), "{rows:?}");
        assert!(rows[5].ends_with(" commit ca3065e57291 unavailable"), "{rows:?}");
        // The line under the rows gives a commit ID whole, and says what is not done about a missing one.
        pressed(&mut sets, &[Key::Down, Key::Down, Key::Down]);
        assert_eq!(frame(&mut sets, 72, 12)[10], format!(" As of commit {old}"));
        sets.key(Key::Down);
        assert_eq!(frame(&mut sets, 80, 12)[10], format!(" Commit {NOWHERE} is not in this repository."));

        // What is not there cannot be selected. One tab is left out by choice.
        pressed(&mut sets, &[Key::Char(' '), Key::Char('g'), Key::Char(' ')]);
        assert!(frame(&mut sets, 72, 12)[0].ends_with("2 of 5 selected"));
        sets.key(Key::Enter);
        let rows = frame(&mut sets, 72, 8);
        assert!(rows[0].starts_with(&format!(" b.txt  b.txt@{} ×", &old[..7])), "{rows:?}");
        assert!(rows.iter().any(|row| row.ends_with("as it was")), "{rows:?}");
        sets.key(Key::Ctrl(b'p'));
        assert!(frame(&mut sets, 72, 8).iter().any(|row| row.ends_with("as it is")));
    }

    #[test]
    fn reopening_over_open_tabs_keeps_their_drafts_and_places_and_adds_the_others_in_order() {
        let f = Fixture::new();
        f.write("a.txt", &numbered(60));
        f.write("b.txt", "second\n");
        f.write("c.txt", &numbered(40));
        f.commit("three files");
        let mut tabs = remembered(&f, &["b.txt", "a.txt", "c.txt"]);
        drawn(&mut tabs, 40, 10);
        tabs.key(Key::Ctrl(b'p'));
        for _ in 0..12 {
            tabs.key(Key::Char('j'));
        }
        tabs.key(Key::Ctrl(b'n'));
        save_as(&mut tabs, "Trio");
        let shelf = shelf(&tabs);

        // Only a.txt stays open, scrolled and with a draft.
        pressed(&mut tabs, &[Key::Ctrl(b'w'), Key::Ctrl(b'n'), Key::Ctrl(b'w')]);
        for _ in 0..5 {
            tabs.key(Key::Char('j'));
        }
        tabs.key(Key::Char('e'));
        typed(&mut tabs, "draft ");
        let before = drawn(&mut tabs, 40, 10);
        assert!(before[0].contains("a.txt") && before[1].contains("draft line 6"), "{before:?}");

        // The lists open over a draft, and the preview says which tab is kept.
        pressed(&mut tabs, &[Key::Ctrl(b't'), Key::Down, Key::Enter]);
        let rows = frame(&mut tabs, 60, 10);
        assert!(rows[1].ends_with("shown · working file") && rows[2].ends_with("open · working file"), "{rows:?}");
        tabs.key(Key::Enter);
        assert_eq!(drawn(&mut tabs, 40, 10)[0], " *a.txt  b.txt ×  c.txt");
        // The tab that was open is as it was; the one that was not is where it was saved.
        tabs.key(Key::Ctrl(b'p'));
        assert_eq!(drawn(&mut tabs, 40, 11)[1..10], before[..9]);
        pressed(&mut tabs, &[Key::Ctrl(b'p')]);
        assert!(drawn(&mut tabs, 40, 10)[2].ends_with("line 13"));

        // The set is what was saved, whatever opened and closed since; the last tabs follow the reader.
        assert_eq!(paths(&shelf.named()[0].list), ["b.txt", "a.txt", "c.txt"]);
        assert_eq!(paths(&shelf.last().unwrap()), ["a.txt", "b.txt", "c.txt"]);
        // Reopening what is open says so and changes nothing.
        pressed(&mut tabs, &[Key::Ctrl(b't'), Key::Down, Key::Enter, Key::Enter]);
        let rows = drawn(&mut tabs, 60, 10);
        assert_eq!(rows[0], " *a.txt  b.txt ×  c.txt");
        assert!(rows[9].contains("Those tabs were already open."), "{rows:?}");
        assert_eq!(kept_drafts(&f), Vec::<String>::new());
    }

    #[test]
    fn a_named_set_is_replaced_only_when_asked_and_renamed_or_deleted_alone() {
        let f = Fixture::new();
        f.write("a.txt", "first\n");
        f.write("b.txt", "second\n");
        f.commit("two files");
        let mut tabs = remembered(&f, &["a.txt"]);
        save_as(&mut tabs, "Parser");
        let shelf = shelf(&tabs);
        ask(&mut tabs, &f, "b.txt");

        // The same name again asks, and keeps the set unless told to replace it.
        pressed(&mut tabs, &[Key::Ctrl(b't'), Key::Char('s')]);
        assert_eq!(tabs.paste(Some("Par\nser")), After::Stay);
        assert!(tabs.editing());
        let rows = frame(&mut tabs, 60, 10);
        assert!(rows[8].starts_with(" Name: Parser") && rows[9].contains("Enter save"), "{rows:?}");
        tabs.key(Key::Enter);
        let rows = frame(&mut tabs, 60, 10);
        assert_eq!(rows[8], " Replace “Parser” with the 2 open tabs?");
        assert_eq!(rows[9], " y replace · n keep");
        tabs.key(Key::Char('n'));
        assert_eq!(paths(&shelf.named()[0].list), ["a.txt"]);
        pressed(&mut tabs, &[Key::Down, Key::Char('o'), Key::Char('y')]);
        assert_eq!(paths(&shelf.named()[0].list), ["a.txt", "b.txt"]);
        let rows = frame(&mut tabs, 60, 10);
        assert_eq!(rows[8], " Saved 2 tabs as “Parser”.");
        assert!(rows[2].starts_with(" › Parser") && rows[2].ends_with("2 tabs"), "{rows:?}");

        // A rename keeps the list; a name that is taken is refused and stays to be changed.
        tabs.key(Key::Char('s'));
        typed(&mut tabs, "Lexer");
        pressed(&mut tabs, &[Key::Enter, Key::Down, Key::Char('r'), Key::Ctrl(b'u')]);
        typed(&mut tabs, "Lexer");
        tabs.key(Key::Enter);
        let rows = frame(&mut tabs, 60, 10);
        assert_eq!((rows[7].as_str(), rows[8].as_str()), (" Another set has that name.", " New name: Lexer"));
        typed(&mut tabs, " 2");
        tabs.key(Key::Enter);
        let names: Vec<_> = shelf.named().into_iter().map(|set| set.name).collect();
        assert_eq!(names, ["Lexer", "Lexer 2"]);

        // Deleting asks, and takes the saved list and nothing else.
        tabs.key(Key::Char('d'));
        assert_eq!(frame(&mut tabs, 60, 10)[8], " Delete “Lexer 2”? Files and open tabs stay.");
        tabs.key(Key::Char('y'));
        assert_eq!(shelf.named().len(), 1);
        tabs.key(Key::Esc);
        assert_eq!(drawn(&mut tabs, 40, 6)[0], " a.txt  b.txt ×");
        assert!(f.root.join("a.txt").exists() && f.root.join("b.txt").exists());
    }

    #[test]
    fn the_last_tabs_reopen_where_they_were_read_to_or_as_near_as_a_changed_file_allows() {
        let f = Fixture::new();
        f.write("long.txt", &numbered(60));
        f.commit("one file");
        let mut tabs = remembered(&f, &["long.txt"]);
        let root = tabs.memory().unwrap().root.clone();
        drawn(&mut tabs, 40, 10);
        for _ in 0..30 {
            tabs.key(Key::Char('j'));
        }
        drop(tabs);

        let mut sets = TabSets::at(state(&f), root.clone(), true);
        assert!(frame(&mut sets, 60, 10)[0].starts_with(" Reopen  Last tabs"));
        sets.key(Key::Enter);
        assert!(frame(&mut sets, 40, 10)[1].ends_with("line 31"));

        // The file is shorter now: the reader shows its end, and what is on disk.
        f.write("long.txt", &numbered(12));
        f.commit("shorter");
        let mut sets = TabSets::at(state(&f), root, true);
        sets.key(Key::Enter);
        let rows = frame(&mut sets, 40, 10);
        assert!(rows[8].contains("line 12") && !rows.iter().any(|row| row.ends_with("line 31")), "{rows:?}");
    }

    #[test]
    fn the_lists_fit_a_narrow_pane_and_take_clicks() {
        let f = Fixture::new();
        f.write("src/a rather long file name.txt", "first\n");
        f.write("b.txt", "second\n");
        f.commit("two files");
        let mut tabs = remembered(&f, &["src/a rather long file name.txt", "b.txt"]);
        save_as(&mut tabs, "Rex compatibility with preview.23");
        tabs.key(Key::Ctrl(b't'));
        let rows = frame(&mut tabs, 34, 7);
        assert_eq!(rows[1], " › Save these tabs as a …  2 tabs");
        assert_eq!(rows[2], "   Rex compatibility wit…  2 tabs");
        // A click selects a row, and a second one opens it.
        tabs.key(Key::Click(5, 2));
        assert_eq!(frame(&mut tabs, 34, 7)[6], " ↑↓ · Enter preview · Esc back");
        tabs.key(Key::Click(5, 2));
        let rows = frame(&mut tabs, 34, 7);
        assert_eq!(rows[0], " Reopen  Rex compatibility with …");
        assert_eq!(rows[1], " › [x] src/a rathe…e.txt  working");
        assert_eq!(rows[6], " ↑↓ · Enter reopen · Esc back");
        // In the preview a click selects or deselects the tab under it.
        tabs.key(Key::Click(5, 2));
        assert_eq!(frame(&mut tabs, 34, 7)[2], " › [ ] b.txt  open · working file");
        // A pane of a few rows keeps the rows that can be chosen, then the hints.
        assert_eq!(frame(&mut tabs, 34, 3)[1], " › [ ] b.txt  open · working file");
        assert_eq!(frame(&mut tabs, 20, 2), [" › [ ] b.t…  worki…", " ↑↓ · Esc back"]);
        frame(&mut tabs, 3, 1);
    }
}
