//! Holds the files open in one reader as tabs, and shows one of them at a time.
//!
//! Each tab is a whole reader, so its place in the file, its draft and that
//! draft's undo history stay with it while another tab is shown. A reader
//! with one file draws exactly as it does alone; a strip of names appears
//! above it with the second. A tab closes only once its file is no longer
//! being edited, which is where unsaved edits are asked about.

use std::cell::Cell;
use std::ops::Range;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::content::{CommittedTarget, WorkingTarget};
use super::inbox::{Inbox, Request};
use super::rescue::{self, Mark};
use super::{Reader, Source};
use crate::git::CommitId;
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
}

impl Tabs {
    /// A reader of one file, which in a Rex block takes requests to show more.
    pub fn new(reader: Reader) -> Self {
        let block = std::env::var("REX_BLOCK").ok().filter(|_| !cfg!(test) && rex::inside());
        Self::placed(reader, rescue::state_dir().zip(block).map(|(state, block)| Place { state, block }))
    }

    pub(super) fn placed(reader: Reader, place: Option<Place>) -> Self {
        Self {
            tabs: vec![reader],
            shown: 0,
            inbox: place.as_ref().and_then(|place| Inbox::open(&place.state, &place.block)),
            place,
            mark: None,
            strip_rows: 0,
            hits: Vec::new(),
            rescope: Cell::new(false),
            closing: false,
        }
    }

    fn reader(&self) -> &Reader {
        &self.tabs[self.shown]
    }

    fn reader_mut(&mut self) -> &mut Reader {
        &mut self.tabs[self.shown]
    }

    /// Shows the file asked for: in the tab that has it, or in a new one.
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

impl Pane for Tabs {
    fn draw(&mut self, s: &mut Screen) {
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

        let mut page = self.reader().help();
        if self.tabs.len() > 1 {
            // First on the page: these keys are what a reader with one file does not have.
            page.lead([
                heading("Tabs"),
                key("^N ^P", "next/previous tab"),
                key("^W   ", "close this tab"),
                note("Click a tab, or its ×."),
                note("* marks unsaved edits."),
                HelpRow::new(),
            ]);
        }
        page
    }

    fn cover(&mut self) {
        self.reader_mut().cover();
    }

    fn tracks_hover(&self) -> bool {
        self.reader().tracks_hover()
    }

    fn hover_span(&self, x: usize, y: usize) -> Option<Range<usize>> {
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
        self.reader().editing()
    }

    fn holds_drafts(&self) -> bool {
        self.tabs.iter().any(Reader::unsaved)
    }

    fn paste(&mut self, text: Option<&str>) -> After {
        self.reader_mut().paste(text)
    }

    fn key(&mut self, key: Key) -> After {
        let several = self.tabs.len() > 1;
        match key {
            Key::Ctrl(b'c') if !self.reader().editing() => self.quit(),
            Key::Click(_, y) if y < self.strip_rows && self.reader().actions.is_some() => After::Ignored,
            Key::Click(x, y) if y < self.strip_rows => self.click_strip(x),
            Key::Click(x, y) => self.pass(Key::Click(x, y - self.strip_rows)),
            key if self.reader().actions.is_some() => self.pass(key),
            Key::Ctrl(b'n') if several => self.step(true),
            Key::Ctrl(b'p') if several => self.step(false),
            Key::Ctrl(b'w') => self.close(self.shown),
            key => self.pass(key),
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
        assert!(tabs.open(&Request { file: f.root.join(path), commit: None }, Instant::now()));
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
        assert!(!tabs.help().text().contains("Tabs"));

        ask(&mut tabs, &f, "b.txt");
        let rows = drawn(&mut tabs, 40, 6);
        assert_eq!(rows[0], " a.txt  b.txt ×");
        assert!(rows[1].contains("b.txt") && rows[2].ends_with("second"), "{rows:?}");
        assert_eq!(rows[5], " q close · Ctrl-A Actions · ? help");
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
        assert!(rows[2].starts_with("  6   line 6") && rows[7].contains("Ctrl-A Actions"), "{rows:?}");
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
        let at = str_width(rows[8].split_once("Ctrl-A Actions").unwrap().0);
        assert_eq!(tabs.hover_span(at, 8), Some(at..at + "Ctrl-A Actions".len()));
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

        tabs.key(Key::Ctrl(b'a'));
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
        let again = Request { file: f.root.join("src/../src/./a.txt"), commit: None };
        assert!(tabs.open(&again, Instant::now()));
        let rows = drawn(&mut tabs, 40, 6);
        assert_eq!(rows[0], " *a.txt ×  b.txt");
        assert!(rows[2].ends_with("draft first"), "{rows:?}");

        // The file as of a commit is another thing to read, and is its own tab once.
        let mut repo = crate::git::Repo::open(&f.root).unwrap();
        repo.read();
        let hash = repo.history(1).unwrap()[0].hash.clone();
        let at_commit = Request { file: f.root.join("b.txt"), commit: Some(hash.clone()) };
        for _ in 0..2 {
            assert!(tabs.open(&at_commit, Instant::now()));
        }
        assert_eq!(drawn(&mut tabs, 60, 6)[0], format!(" *a.txt  b.txt  b.txt@{} ×", &hash[..7]));

        // What cannot be opened is said, and no tab is made for it.
        assert!(!tabs.open(&Request { file: "b.txt".into(), commit: Some("not hex".into()) }, Instant::now()));
        let rows = drawn(&mut tabs, 60, 6);
        assert_eq!(rows[5], " That is not a commit ID. · ? help");
        assert_eq!(rows[0].matches("b.txt").count(), 2);
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
        let request = Request { file: f.root.join(OsStr::from_bytes(b"page\xff.txt")), commit: None };
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
            tabs.open(&Request { file: f.root.join("page.txt"), commit: Some(id.into()) }, Instant::now())
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
            tabs.open(&Request { file: root.join("page.txt"), commit: Some(hash.clone()) }, Instant::now())
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
}
