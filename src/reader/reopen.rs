//! Shows a worktree's last tabs and named sets, previews one, and says which of its tabs to reopen.
//!
//! The screen covers a reader, or stands alone before there is one. It saves,
//! renames, replaces and deletes named sets itself. Reopening is the reader's
//! work: the screen hands back the tabs that were chosen, and never one whose
//! file is missing or whose commit the repository no longer has.

use std::path::{Path, PathBuf};

use super::content::{CommittedTarget, WorkingTarget};
use super::sets::{Collision, Entry, List, NAME_ROOM, Named, Refusal, Shelf};
use crate::git::CommitId;
use crate::help::{self, HintHits, Page};
use crate::keys::Key;
use crate::screen::{Screen, Style, YELLOW};
use crate::ui::{self, FAINT, HEADING};
use crate::width::str_width;

/// Characters of a commit ID shown in a row. The line under the rows gives it whole.
const ID_ROOM: usize = 12;

/// The tabs chosen to reopen, in their saved order.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub(super) entries: Vec<Entry>,
    /// The tab that was shown when the list was kept, if it was chosen.
    pub(super) active: Option<usize>,
}

pub(super) enum Step {
    Ignored,
    Stay,
    Close,
    Reopen(Plan),
}

/// Whether a tab of a list can be reopened now.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Found {
    Ready,
    /// The working file is not in the worktree.
    Missing,
    /// The repository does not have the commit, or Git could not say.
    NoCommit,
}

impl Found {
    fn of(root: &Path, entry: &Entry) -> Self {
        match &entry.commit {
            None => {
                let present = WorkingTarget::resolve(&root.join(&entry.path)).is_some_and(|target| target.present());
                if present { Self::Ready } else { Self::Missing }
            }
            Some(id) => {
                let resolved = CommitId::parse(id)
                    .is_some_and(|id| CommittedTarget::resolve_from(root, Path::new(&entry.path), id).is_ok());
                if resolved { Self::Ready } else { Self::NoCommit }
            }
        }
    }
}

struct Tab {
    entry: Entry,
    found: Found,
    /// The reader has this file open already, so its tab is kept as it is.
    open: bool,
    chosen: bool,
}

/// One list, tab by tab, before any of it is reopened.
struct Preview {
    title: String,
    tabs: Vec<Tab>,
    active: usize,
    cursor: usize,
}

impl Preview {
    fn new(title: String, list: &List, root: &Path, open: Option<&List>) -> Self {
        let tabs = list
            .entries
            .iter()
            .map(|entry| {
                let found = Found::of(root, entry);
                let open = open.is_some_and(|open| open.entries.iter().any(|tab| tab.same_file(entry)));
                Tab { entry: entry.clone(), found, open, chosen: found == Found::Ready }
            })
            .collect();
        Self { title, tabs, active: list.active, cursor: 0 }
    }

    fn chosen(&self) -> usize {
        self.tabs.iter().filter(|tab| tab.chosen).count()
    }

    fn toggle(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.cursor).filter(|tab| tab.found == Found::Ready) {
            tab.chosen = !tab.chosen;
        }
    }

    /// Chooses every tab that can be reopened, or none if they all are.
    fn toggle_all(&mut self) {
        let all = self.tabs.iter().all(|tab| tab.chosen || tab.found != Found::Ready);
        for tab in &mut self.tabs {
            tab.chosen = !all && tab.found == Found::Ready;
        }
    }

    fn plan(&self) -> Plan {
        let mut plan = Plan { entries: Vec::new(), active: None };
        for (i, tab) in self.tabs.iter().enumerate().filter(|(_, tab)| tab.chosen) {
            if i == self.active {
                plan.active = Some(plan.entries.len());
            }
            plan.entries.push(tab.entry.clone());
        }
        plan
    }

    /// What the row of a tab says after its path: where it stood in the reader, and which version it is.
    fn status(tab: &Tab, active: bool, brief: bool) -> (String, Style) {
        let id = |length: usize| tab.entry.commit.as_deref().map_or("", |id| &id[..id.len().min(length)]);
        let (text, style) = match (tab.found, brief) {
            (Found::Missing, _) => ("missing file".to_owned(), Style::fg(YELLOW)),
            (Found::NoCommit, true) => ("unavailable".to_owned(), Style::fg(YELLOW)),
            (Found::NoCommit, false) => (format!("commit {} unavailable", id(ID_ROOM)), Style::fg(YELLOW)),
            (Found::Ready, true) if tab.entry.commit.is_some() => (format!("@{}", id(7)), FAINT),
            (Found::Ready, false) if tab.entry.commit.is_some() => (format!("commit {}", id(ID_ROOM)), FAINT),
            (Found::Ready, true) => ("working".to_owned(), FAINT),
            (Found::Ready, false) => ("working file".to_owned(), FAINT),
        };
        let lead = match (active && !brief, tab.open && !brief) {
            (true, true) => "shown · open · ",
            (true, false) => "shown · ",
            (false, true) => "open · ",
            (false, false) => "",
        };
        (format!("{lead}{text}"), style)
    }

    /// The whole of what a row had to shorten, for the tab under the cursor.
    fn detail(&self) -> String {
        let Some(tab) = self.tabs.get(self.cursor) else { return String::new() };
        let id = tab.entry.commit.as_deref().unwrap_or_default();
        match tab.found {
            Found::Missing => "Not in the worktree now, so it is not reopened.".to_owned(),
            Found::NoCommit => format!("Commit {id} is not in this repository."),
            Found::Ready if tab.open => "Already open: its tab stays as it is.".to_owned(),
            Found::Ready if tab.entry.commit.is_some() => format!("As of commit {id}"),
            Found::Ready => "The working file, as it is on disk now.".to_owned(),
        }
    }
}

/// A row of the first screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    /// Saves the reader's tabs under a new name.
    Save,
    Last,
    Named(usize),
}

enum Naming {
    Save,
    Rename(String),
}

enum Question {
    Replace(String),
    Delete(String),
}

enum Stage {
    Lists,
    Preview(Preview),
    Naming { purpose: Naming, text: String },
    Asking(Question),
}

pub(super) struct Reopen {
    shelf: Shelf,
    root: PathBuf,
    /// The tabs of the reader this screen covers. None where it stands alone.
    open: Option<List>,
    last: Option<List>,
    sets: Vec<Named>,
    cursor: usize,
    stage: Stage,
    /// The answer to the last thing asked for, until the next key.
    said: String,
    /// The list on screen: its first row, how many rows it showed, and the line of the screen they began at.
    window: (usize, usize, usize),
    footer: HintHits<'static>,
}

impl Reopen {
    /// `open` is the list of the reader this covers, and `last` the last tabs to offer.
    pub(super) fn new(shelf: Shelf, root: PathBuf, open: Option<List>, last: Option<List>) -> Self {
        let sets = shelf.named();
        let open = open.filter(|open| !open.entries.is_empty());
        Self {
            shelf,
            root,
            open,
            last,
            sets,
            cursor: 0,
            stage: Stage::Lists,
            said: String::new(),
            window: (0, 0, 0),
            footer: HintHits::default(),
        }
    }

    /// Starts at the preview of the last tabs, where there are some.
    pub(super) fn previewing_last(mut self) -> Self {
        if let Some(at) = self.rows().iter().position(|row| *row == Row::Last) {
            self.cursor = at;
            self.preview();
        }
        self
    }

    /// Takes in the reader's tabs as they are now, after one opened or closed under the screen.
    pub(super) fn set_open(&mut self, open: List) {
        self.open = Some(open).filter(|open| !open.entries.is_empty());
        self.cursor = self.cursor.min(self.rows().len().saturating_sub(1));
    }

    /// Says `text` above the hints until the next key.
    pub(super) fn say(&mut self, text: &str) {
        text.clone_into(&mut self.said);
    }

    /// Whether keys are text for a name.
    pub(super) fn typing(&self) -> bool {
        matches!(self.stage, Stage::Naming { .. })
    }

    fn rows(&self) -> Vec<Row> {
        let save = self.open.as_ref().map(|_| Row::Save);
        let last = self.last.as_ref().map(|_| Row::Last);
        save.into_iter().chain(last).chain((0..self.sets.len()).map(Row::Named)).collect()
    }

    fn row(&self) -> Option<Row> {
        self.rows().get(self.cursor).copied()
    }

    /// The named set under the cursor.
    fn named(&self) -> Option<&Named> {
        match self.row()? {
            Row::Named(set) => self.sets.get(set),
            Row::Save | Row::Last => None,
        }
    }

    fn preview(&mut self) {
        let (title, list) = match self.row() {
            Some(Row::Last) => ("Last tabs".to_owned(), self.last.as_ref()),
            Some(Row::Named(set)) => (self.sets[set].name.clone(), Some(&self.sets[set].list)),
            Some(Row::Save) | None => return,
        };
        if let Some(list) = list {
            self.stage = Stage::Preview(Preview::new(title, list, &self.root, self.open.as_ref()));
        }
    }

    /// Reads the sets again and puts the cursor on the one called `name`.
    fn reload(&mut self, name: Option<&str>) {
        self.sets = self.shelf.named();
        let rows = self.rows();
        let at = name.and_then(|name| {
            rows.iter().position(|row| matches!(row, Row::Named(set) if self.sets[*set].name == name))
        });
        self.cursor = at.unwrap_or(self.cursor).min(rows.len().saturating_sub(1));
    }

    /// Saves the reader's tabs as `name`, or asks before replacing a set of that name.
    /// False if the name was refused, which is then said.
    fn save(&mut self, name: &str, collision: Collision) -> bool {
        let Some(open) = &self.open else { return true };
        let count = open.entries.len();
        match self.shelf.save(name, open, collision) {
            Ok(()) => {
                let name = name.trim().to_owned();
                self.said = format!("Saved {count} {} as “{name}”.", plural(count));
                self.reload(Some(&name));
            }
            Err(Refusal::Taken) => self.stage = Stage::Asking(Question::Replace(name.trim().to_owned())),
            Err(Refusal::Said(why)) => {
                self.said = why.to_owned();
                return false;
            }
        }
        true
    }

    /// False if the new name was refused, which is then said.
    fn rename(&mut self, from: &str, to: &str) -> bool {
        match self.shelf.rename(from, to) {
            Ok(()) => {
                self.said = format!("Renamed to “{}”.", to.trim());
                self.reload(Some(to.trim()));
                true
            }
            Err(Refusal::Taken) => {
                self.said = "Another set has that name.".to_owned();
                false
            }
            Err(Refusal::Said(why)) => {
                self.said = why.to_owned();
                false
            }
        }
    }

    fn answer(&mut self, question: Question) {
        match question {
            Question::Replace(name) => {
                self.save(&name, Collision::Replace);
            }
            Question::Delete(name) => {
                self.said = if self.shelf.delete(&name) {
                    format!("Deleted “{name}”. Files and open tabs are as they were.")
                } else {
                    "The set could not be deleted.".to_owned()
                };
                self.reload(None);
            }
        }
    }

    /// Text pasted into a name. Anything else on the clipboard is not for this screen.
    pub(super) fn paste(&mut self, pasted: &str) -> Step {
        let Stage::Naming { text, .. } = &mut self.stage else { return Step::Ignored };
        let room = NAME_ROOM.saturating_sub(text.chars().count());
        text.extend(pasted.chars().filter(|c| !c.is_control()).take(room));
        Step::Stay
    }

    pub(super) fn key(&mut self, key: Key) -> Step {
        let key = match key {
            Key::Click(x, y) => match self.click(x, y) {
                Some(key) => key,
                None => return Step::Stay,
            },
            key => key,
        };
        if key == Key::Ctrl(b'c') {
            return Step::Close;
        }
        if matches!(key, Key::Focus(_) | Key::Hover(..)) {
            return Step::Ignored;
        }
        self.said.clear();
        match std::mem::replace(&mut self.stage, Stage::Lists) {
            Stage::Lists => self.lists_key(key),
            Stage::Preview(preview) => self.preview_key(preview, key),
            Stage::Naming { purpose, text } => self.naming_key(purpose, text, key),
            Stage::Asking(question) => {
                match key {
                    Key::Char('y' | 'Y') => self.answer(question),
                    Key::Char('n' | 'N' | 'q') | Key::Esc => {}
                    _ => self.stage = Stage::Asking(question),
                }
                Step::Stay
            }
        }
    }

    fn lists_key(&mut self, key: Key) -> Step {
        let saving = self.open.is_some();
        match key {
            Key::Esc | Key::Char('q') | Key::Ctrl(b't') => return Step::Close,
            Key::Enter if self.row() == Some(Row::Save) => self.name(Naming::Save),
            Key::Enter => self.preview(),
            Key::Char('s') if saving => self.name(Naming::Save),
            Key::Char('r') => {
                if let Some(name) = self.named().map(|set| set.name.clone()) {
                    self.stage = Stage::Naming { text: name.clone(), purpose: Naming::Rename(name) };
                }
            }
            Key::Char('o') if saving => {
                if let Some(set) = self.named() {
                    self.stage = Stage::Asking(Question::Replace(set.name.clone()));
                }
            }
            Key::Char('d') => {
                if let Some(set) = self.named() {
                    self.stage = Stage::Asking(Question::Delete(set.name.clone()));
                }
            }
            key => {
                if let Some(delta) = ui::selection_delta(key, self.window.1) {
                    self.cursor = self.cursor.saturating_add_signed(delta).min(self.rows().len().saturating_sub(1));
                }
            }
        }
        Step::Stay
    }

    fn name(&mut self, purpose: Naming) {
        self.stage = Stage::Naming { purpose, text: String::new() };
    }

    fn preview_key(&mut self, mut preview: Preview, key: Key) -> Step {
        match key {
            Key::Esc | Key::Char('q') => return Step::Stay,
            Key::Ctrl(b't') => return Step::Close,
            Key::Enter if preview.chosen() > 0 => return Step::Reopen(preview.plan()),
            Key::Enter => self.said = "No tab is selected.".to_owned(),
            Key::Char(' ' | 'x') => preview.toggle(),
            Key::Char('a') => preview.toggle_all(),
            key => {
                if let Some(delta) = ui::selection_delta(key, self.window.1) {
                    preview.cursor =
                        preview.cursor.saturating_add_signed(delta).min(preview.tabs.len().saturating_sub(1));
                }
            }
        }
        self.stage = Stage::Preview(preview);
        Step::Stay
    }

    fn naming_key(&mut self, purpose: Naming, mut text: String, key: Key) -> Step {
        match key {
            Key::Esc => return Step::Stay,
            Key::Enter => {
                let taken = match &purpose {
                    Naming::Save => self.save(&text, Collision::Refuse),
                    Naming::Rename(from) => self.rename(from, &text),
                };
                if taken {
                    return Step::Stay;
                }
                // A refused name stays as typed, to be changed.
            }
            Key::Backspace | Key::Ctrl(b'h') => drop(text.pop()),
            Key::Ctrl(b'u') => text.clear(),
            Key::Char(c) if !c.is_control() && text.chars().count() < NAME_ROOM => text.push(c),
            _ => {}
        }
        self.stage = Stage::Naming { purpose, text };
        Step::Stay
    }

    /// The key a click stands for: a hint's own key, or the row clicked, chosen at the second click.
    fn click(&mut self, x: usize, y: usize) -> Option<Key> {
        if let Some(item) = self.footer.item_at(x, y) {
            return Some(match item.split(' ').next().unwrap_or_default() {
                "↑↓" => {
                    if x == 1 {
                        Key::Up
                    } else {
                        Key::Down
                    }
                }
                "Enter" => Key::Enter,
                "Esc" => Key::Esc,
                "Space" => Key::Char(' '),
                word => Key::Char(word.chars().next()?),
            });
        }
        let row = self.window.0 + y.checked_sub(self.window.2).filter(|row| *row < self.window.1)?;
        match &mut self.stage {
            Stage::Lists if row == self.cursor => Some(Key::Enter),
            Stage::Lists => {
                self.cursor = row;
                None
            }
            Stage::Preview(preview) => {
                preview.cursor = row;
                Some(Key::Char(' '))
            }
            Stage::Naming { .. } | Stage::Asking(_) => None,
        }
    }

    pub(super) fn help(&self) -> Page {
        use help::{HelpRow, heading, key, note};

        let mut rows = vec![
            heading("Reader tabs"),
            key("Enter", "preview a list"),
            key("r    ", "rename a set"),
            key("d    ", "delete a set"),
        ];
        if self.open.is_some() {
            rows.extend([key("s    ", "save open tabs"), key("o    ", "replace a set")]);
        }
        rows.extend([
            key("Esc  ", "back"),
            note("A set changes only"),
            note("when you replace it."),
            HelpRow::new(),
            heading("Preview"),
            key("Space", "select a tab"),
            key("a    ", "all or none"),
            key("Enter", "reopen selected"),
            note("Open tabs keep their"),
            note("drafts and places."),
            note("A missing file or"),
            note("commit stays closed."),
        ]);
        Page::new(rows)
    }

    pub(super) fn draw(&mut self, s: &mut Screen) {
        s.clear();
        self.footer = HintHits::default();
        if s.h == 0 || s.w < 4 {
            return;
        }
        let heading = usize::from(s.h >= 3);
        // The last row is the hints; the one above it answers, asks or explains.
        let under = usize::from(s.h >= 2) + usize::from(s.h >= 4);
        let room = s.h.saturating_sub(heading + under);
        match &self.stage {
            Stage::Preview(preview) => {
                let first = ui::selection_start(preview.cursor, preview.tabs.len(), room);
                self.window = (first, room.min(preview.tabs.len()), heading);
                if heading == 1 {
                    Self::draw_heading(s, "Reopen", &preview.title);
                    let count = format!("{} of {} selected", preview.chosen(), preview.tabs.len());
                    if str_width(&count) + 12 + str_width(&preview.title) < s.w {
                        s.put_right(s.w - 1, 0, &count, FAINT);
                    }
                }
                for (i, tab) in preview.tabs.iter().enumerate().skip(first).take(room) {
                    Self::draw_tab(s, heading + i - first, tab, i == preview.cursor, i == preview.active);
                }
                let line = if self.said.is_empty() { preview.detail() } else { self.said.clone() };
                self.draw_under(s, &line, FAINT);
                if under > 0 {
                    // The arrows go unexplained so that a narrow pane still names Enter.
                    self.footer = help::draw_hint(s, "↑↓", &["Space select", "a all", "Enter reopen"], "Esc back");
                }
            }
            Stage::Lists | Stage::Naming { .. } | Stage::Asking(_) => self.draw_lists(s, heading, under, room),
        }
    }

    fn draw_heading(s: &mut Screen, title: &str, about: &str) {
        let room = s.w.saturating_sub(2);
        let used = ui::section_heading(s, 0, room, title);
        s.put_fit(used + 3, 0, room.saturating_sub(used + 2), about, FAINT);
    }

    /// The line above the hints.
    fn draw_under(&self, s: &mut Screen, text: &str, style: Style) {
        if s.h >= 4 {
            s.put_fit(1, s.h - 2, s.w.saturating_sub(2), text, style);
        }
    }

    fn draw_tab(s: &mut Screen, y: usize, tab: &Tab, cursor: bool, active: bool) {
        let ready = tab.found == Found::Ready;
        let style = match (ready, cursor) {
            (false, _) => FAINT,
            (true, true) => HEADING,
            (true, false) => Style::PLAIN,
        };
        if cursor {
            s.put(1, y, 1, "›", style);
        }
        let mark = match (ready, tab.chosen) {
            (false, _) => " – ",
            (true, true) => "[x]",
            (true, false) => "[ ]",
        };
        s.put(3, y, 3, mark, style);
        let end = s.w.saturating_sub(1);
        // A narrow pane keeps the path readable and says less after it.
        let (mut status, mut colour) = Preview::status(tab, active, false);
        if 7 + str_width(&tab.entry.path).min(16) + 2 + str_width(&status) > end {
            (status, colour) = Preview::status(tab, active, true);
        }
        let status_width = str_width(&status).min(end.saturating_sub(7 + 6));
        s.put_fit(end - status_width, y, status_width, &status, colour);
        s.put_middle(7, y, end.saturating_sub(7 + status_width + 2), &tab.entry.path, style);
    }

    fn draw_lists(&mut self, s: &mut Screen, heading: usize, under: usize, room: usize) {
        let rows = self.rows();
        let first = ui::selection_start(self.cursor, rows.len(), room);
        self.window = (first, room.min(rows.len()), heading);
        if heading == 1 {
            let folder = self.root.file_name().unwrap_or_default().to_string_lossy();
            Self::draw_heading(s, "Reader tabs", &folder);
        }
        let choosing = matches!(self.stage, Stage::Lists);
        for (i, row) in rows.iter().enumerate().skip(first).take(room) {
            let y = heading + i - first;
            let selected = i == self.cursor;
            let style = if selected && choosing { HEADING } else { Style::PLAIN };
            if selected {
                s.put(1, y, 1, "›", style);
            }
            let (label, count) = match row {
                Row::Save => ("Save these tabs as a named set…", self.open.as_ref().map(|open| open.entries.len())),
                Row::Last => ("Last tabs", self.last.as_ref().map(|last| last.entries.len())),
                Row::Named(set) => (self.sets[*set].name.as_str(), Some(self.sets[*set].list.entries.len())),
            };
            let count = count.map_or_else(String::new, |count| format!("{count} {}", plural(count)));
            let end = s.w.saturating_sub(1);
            let count_width = str_width(&count).min(end.saturating_sub(3 + 8));
            s.put(end - count_width, y, count_width, &count, FAINT);
            s.put_fit(3, y, end.saturating_sub(3 + count_width + 2), label, style);
        }
        if rows.is_empty() && room > 0 {
            s.put_fit(1, heading, s.w.saturating_sub(2), "No tabs are remembered for this worktree yet.", FAINT);
        }
        if under == 0 {
            return;
        }
        let saving = self.open.is_some();
        self.footer = match &self.stage {
            Stage::Naming { purpose, text } => {
                let lead = match purpose {
                    Naming::Save => "Name: ",
                    Naming::Rename(_) => "New name: ",
                };
                self.draw_name(s, lead, text);
                help::draw_hint(s, "", &["Enter save"], "Esc cancel")
            }
            Stage::Asking(question) => {
                let (asked, yes): (String, &'static [&'static str]) = match question {
                    Question::Replace(name) => {
                        let count = self.open.as_ref().map_or(0, |open| open.entries.len());
                        (format!("Replace “{name}” with the {count} open {}?", plural(count)), &["y replace"])
                    }
                    Question::Delete(name) => (format!("Delete “{name}”? Files and open tabs stay."), &["y delete"]),
                };
                self.draw_under(s, &asked, Style::fg(YELLOW));
                help::draw_hint(s, "", yes, "n keep")
            }
            Stage::Lists | Stage::Preview(_) => {
                self.draw_under(s, &self.said, Style::PLAIN);
                let keys: &'static [&'static str] = match (self.row(), saving) {
                    (Some(Row::Save), _) => &["Enter name it"],
                    (Some(Row::Last), true) => &["s save", "Enter preview"],
                    (Some(Row::Last), false) => &["Enter preview"],
                    (Some(Row::Named(_)), true) => &["s save", "o replace", "r rename", "d delete", "Enter preview"],
                    (Some(Row::Named(_)), false) => &["r rename", "d delete", "Enter preview"],
                    (None, _) => &[],
                };
                help::draw_hint(s, "↑↓", keys, "Esc back")
            }
        };
    }

    /// The name being typed, with the place the next letter goes picked out.
    fn draw_name(&self, s: &mut Screen, lead: &str, text: &str) {
        if s.h < 4 {
            return;
        }
        let (y, room) = (s.h - 2, s.w.saturating_sub(2));
        if !self.said.is_empty() && s.h >= 5 {
            s.put_fit(1, y - 1, room, &self.said, Style::fg(YELLOW));
        }
        let mut x = 1 + s.put(1, y, room, lead, FAINT);
        // The end of a name too long for the row is the part being typed.
        let shown = text.char_indices().rev().take(room.saturating_sub(x + 1)).last().map_or("", |(at, _)| &text[at..]);
        x += s.put(x, y, room.saturating_sub(x), shown, Style::PLAIN);
        s.put(x, y, 1, " ", Style::PLAIN.reverse());
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "tab" } else { "tabs" }
}
