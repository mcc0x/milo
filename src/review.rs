//! Shows the files of a handed-over patch one at a time, each with only the patch's edits marked,
//! and takes notes on its lines to copy out.

mod compose;
mod list;
#[cfg(test)]
mod note_tests;
mod notes;

use std::fmt::Write as _;
use std::ops::RangeInclusive;
use std::path::Path;

use crate::git::Repo;
use crate::handoff::{PATCH_LIMIT, Patch};
use crate::help::{self, Page};
use crate::keys::Key;
use crate::pane::{After, Pane};
use crate::read::{self, Content};
use crate::reader::{Handed, Picked, Reader, Viewed, handed_lines};
use crate::rex;
use crate::screen::{Screen, Style, Wash};
use compose::Draft;
use list::{Chosen, List};
use notes::{COPY_LIMIT, NOTES_LIMIT, Note};

/// The most rows of a note shown while it is written; a longer one scrolls.
const DRAFT_ROWS: usize = 6;

/// A reader over each file of one patch in turn. It reads the project and
/// changes nothing, in the project or outside it. Notes are kept in memory
/// and leave only through the terminal's clipboard.
pub struct Review {
    repo: Repo,
    /// The patch as it was when the review opened. The files are read again whenever one is shown.
    text: String,
    paths: Vec<String>,
    from: Option<String>,
    at: usize,
    reader: Reader,
    notes: Vec<Note>,
    mode: Mode,
    /// Text for the terminal's clipboard, sent with the next frame.
    to_copy: Option<String>,
}

/// What the keys are for at the moment.
enum Mode {
    Read,
    /// Moving the line cursor to what a note will be about.
    Pick,
    Write(Box<Writing>),
    List(List),
    /// Asked whether to close with notes that were never copied.
    Leaving,
}

struct Writing {
    draft: Draft,
    about: About,
    /// The columns the draft was last drawn in, which moving by rows goes by.
    width: usize,
    /// Asked whether to throw away what was typed.
    discarding: bool,
}

/// What a note being written is about, and what to go back to afterwards.
enum About {
    /// A new note on the picked lines. The file is shown from `from` again
    /// once the panel no longer covers part of it.
    Picked { picked: Picked, from: Viewed },
    /// A note of the list, being edited.
    Listed { note: usize, list: List },
}

/// A name to show for whoever handed the patch over: one short line of plain text.
pub fn name(text: &str) -> Option<&str> {
    let plain = !text.trim().is_empty() && !text.chars().any(char::is_control);
    (plain && text.chars().count() <= 40).then_some(text)
}

/// The patch's text. Its file is read once, like any other the user names, and never kept.
fn patch_text(file: &Path) -> Result<String, String> {
    let shown = file.display();
    match read::file(file, PATCH_LIMIT) {
        Content::Bytes(bytes) => String::from_utf8(bytes).map_err(|_| format!("{shown} is not UTF-8 text")),
        Content::TooLarge => Err(format!("{shown} is too large; a patch can be 2 MB")),
        Content::Gone => Err(format!("there is no patch at {shown}")),
        Content::Link(_) | Content::NotFile => Err(format!("{shown} is not a file")),
        Content::Unreadable => Err(format!("{shown} cannot be read")),
    }
}

fn project(dir: &Path) -> Result<Repo, String> {
    Repo::open(dir).ok_or_else(|| "the current directory cannot be read".to_string())
}

fn reader(repo: &Repo, patch: &Patch<'_>, paths: &[String], at: usize, from: Option<&str>) -> Reader {
    let handed = Handed { from: from.map(str::to_owned), number: at + 1, total: paths.len() };
    Reader::handed_over(repo, patch, &paths[at], handed)
}

impl Review {
    /// Opens the patch in `file` against the project that holds `dir`.
    pub fn open(dir: &Path, file: &Path, from: Option<&str>) -> Result<Self, String> {
        let repo = project(dir)?;
        let text = patch_text(file)?;
        let patch = Patch::parse(&text)?;
        let paths: Vec<String> = patch.paths().iter().map(|path| (*path).to_owned()).collect();
        let reader = reader(&repo, &patch, &paths, 0, from);
        let from = from.map(str::to_owned);
        Ok(Self { repo, text, paths, from, at: 0, reader, notes: Vec::new(), mode: Mode::Read, to_copy: None })
    }

    fn show(&mut self, at: usize) -> After {
        // The text parsed when the review opened, so it parses now.
        if let Ok(patch) = Patch::parse(&self.text) {
            self.reader = reader(&self.repo, &patch, &self.paths, at, self.from.as_deref());
            self.at = at;
            self.mark_notes();
        }
        After::Stay
    }

    /// The shown file's name as the heading has it: a patch can put anything in a name.
    fn shown_path(&self) -> String {
        self.paths[self.at].chars().filter(|c| !c.is_control()).collect()
    }

    fn waiting(&self) -> usize {
        self.notes.iter().filter(|note| !note.copied).count()
    }

    /// Tells the reader how many notes wait and which of its lines have one.
    /// A note whose lines the file no longer shows there is kept and marks nothing.
    fn mark_notes(&mut self) {
        let path = self.shown_path();
        let mut noted: Vec<RangeInclusive<usize>> = Vec::new();
        let mut here: Vec<&Note> = self.notes.iter().filter(|note| note.path == path).collect();
        here.sort_by_key(|note| note.at);
        for note in here.into_iter().filter(|note| self.reader.holds(note.at, &note.lines)) {
            let end = note.at + note.len - 1;
            match noted.last_mut() {
                Some(last) if *last.end() + 1 >= note.at => *last = *last.start()..=end.max(*last.end()),
                _ => noted.push(note.at..=end),
            }
        }
        self.reader.mark_noted(noted);
        self.reader.count_notes((!self.notes.is_empty()).then(|| self.waiting()));
    }

    /// Hands `text` to the clipboard with the next frame, unless it is too long for a terminal to take.
    fn copy(&mut self, text: String) -> bool {
        let fits = text.len() <= COPY_LIMIT;
        if fits {
            self.to_copy = Some(text);
        }
        fits
    }

    fn open_list(&mut self) -> After {
        if self.notes.is_empty() {
            self.reader.tell("No notes yet");
        } else {
            self.mode = Mode::List(List::open(&self.notes));
        }
        After::Stay
    }

    fn read_key(&mut self, key: Key) -> After {
        let files = self.paths.len();
        match key {
            Key::Char(']') if files > 1 => self.show((self.at + 1) % files),
            Key::Char('[') if files > 1 => self.show((self.at + files - 1) % files),
            Key::Char('r') => self.show(self.at),
            Key::Char('i') => {
                if self.notes.len() >= NOTES_LIMIT {
                    self.reader.tell("Too many notes; remove one first");
                } else if self.reader.start_pick() {
                    self.mode = Mode::Pick;
                } else {
                    self.reader.tell("No lines to note here");
                }
                After::Stay
            }
            Key::Char('p') => self.open_list(),
            Key::Char('q') | Key::Esc | Key::Ctrl(b'c') if self.waiting() > 0 => {
                self.mode = Mode::Leaving;
                After::Stay
            }
            _ => self.reader.key(key),
        }
    }

    fn pick_key(&mut self, key: Key) -> After {
        match key {
            Key::Enter | Key::Char('i') => {
                if let Some(picked) = self.reader.picked() {
                    let about = About::Picked { picked, from: self.reader.place() };
                    let writing = Writing { draft: Draft::new(""), about, width: 0, discarding: false };
                    self.mode = Mode::Write(Box::new(writing));
                }
                After::Stay
            }
            Key::Esc | Key::Char('q') | Key::Ctrl(b'c') => {
                self.reader.end_pick();
                self.mode = Mode::Read;
                After::Stay
            }
            _ if self.reader.pick_key(key) => After::Stay,
            _ => After::Ignored,
        }
    }

    fn write_key(&mut self, key: Key) -> After {
        let Mode::Write(writing) = &mut self.mode else { return After::Ignored };
        if writing.discarding {
            return match key {
                Key::Char('y') => self.close_draft(None),
                Key::Focus(_) => After::Ignored,
                _ => {
                    writing.discarding = false;
                    After::Stay
                }
            };
        }
        match key {
            Key::Enter | Key::Ctrl(b'y') if writing.draft.text().is_empty() => After::Ignored,
            Key::Enter => self.keep_draft(false),
            Key::Ctrl(b'y') => self.keep_draft(true),
            Key::Esc | Key::Ctrl(b'c') => {
                let unchanged = match &writing.about {
                    About::Picked { .. } => writing.draft.text().is_empty(),
                    About::Listed { note, .. } => self.notes[*note].text == writing.draft.text(),
                };
                if unchanged {
                    return self.close_draft(None);
                }
                writing.discarding = true;
                After::Stay
            }
            _ if writing.draft.key(key, writing.width) => After::Stay,
            _ => After::Ignored,
        }
    }

    /// Keeps the draft as a note, copying that one note now if asked. Other notes are left as they are.
    fn keep_draft(&mut self, copy: bool) -> After {
        let path = self.shown_path();
        let Mode::Write(writing) = &mut self.mode else { return After::Ignored };
        let text = writing.draft.text().to_owned();
        let at = match &mut writing.about {
            About::Picked { picked, .. } => {
                let picked = Picked { lines: std::mem::take(&mut picked.lines), ..*picked };
                self.notes.push(Note::new(path, picked, text));
                self.notes.len() - 1
            }
            About::Listed { note, .. } => {
                let kept = &mut self.notes[*note];
                kept.copied &= kept.text == text;
                kept.text = text;
                *note
            }
        };
        let copied = copy && self.copy(notes::one(&self.notes[at]));
        self.notes[at].copied |= copied;
        self.close_draft(Some(match (copy, copied) {
            (true, true) => "Copied this note",
            (true, false) => "Too long to copy; kept for review",
            (false, _) => "Kept for review",
        }))
    }

    /// Goes back to what the draft was opened from, saying `said` there.
    fn close_draft(&mut self, said: Option<&'static str>) -> After {
        let Mode::Write(writing) = std::mem::replace(&mut self.mode, Mode::Read) else { return After::Ignored };
        match writing.about {
            About::Picked { from, .. } => {
                self.reader.end_pick();
                self.reader.return_to(from);
                if let Some(said) = said {
                    self.reader.tell(said);
                }
            }
            About::Listed { mut list, .. } => {
                list.said = said;
                list.settle(&self.notes);
                self.mode = Mode::List(list);
            }
        }
        self.mark_notes();
        After::Stay
    }

    fn list_key(&mut self, key: Key) -> After {
        let Mode::List(list) = &mut self.mode else { return After::Ignored };
        if matches!(key, Key::Focus(_)) {
            return After::Ignored;
        }
        let removing = std::mem::take(&mut list.removing);
        list.said = None;
        match (key, list.chosen) {
            (Key::Char('y'), Chosen::Note(at)) if removing => {
                self.notes.remove(at);
                list.said = Some("Removed");
            }
            _ if removing => {}
            (Key::Esc | Key::Char('q' | 'p') | Key::Ctrl(b'c'), _) => self.mode = Mode::Read,
            (Key::Char('y'), _) | (Key::Enter, Chosen::Waiting) => return self.copy_chosen(),
            (Key::Char('e') | Key::Enter, Chosen::Note(note)) => {
                let (draft, about) = (Draft::new(&self.notes[note].text), About::Listed { note, list: *list });
                self.mode = Mode::Write(Box::new(Writing { draft, about, width: 0, discarding: false }));
            }
            (Key::Char('d') | Key::Delete | Key::Backspace, Chosen::Note(_)) => list.removing = true,
            _ => {
                let moved = key.rows(self.notes.len()).is_some_and(|delta| list.step(&self.notes, delta));
                return if moved { After::Stay } else { After::Ignored };
            }
        }
        if let Mode::List(list) = &mut self.mode {
            list.settle(&self.notes);
            if self.notes.is_empty() {
                self.mode = Mode::Read;
                self.reader.tell("No notes left");
            }
        }
        self.mark_notes();
        After::Stay
    }

    /// Copies what is chosen in the list: one note, or every waiting note as one message.
    fn copy_chosen(&mut self) -> After {
        let Mode::List(list) = &self.mode else { return After::Ignored };
        let (mut list, text) = (*list, list.text(&self.notes));
        let copied = text.is_some_and(|text| self.copy(text));
        list.said = Some(match (copied, list.chosen) {
            (false, _) => "Too long to copy at once; copy notes one by one",
            (true, Chosen::Waiting) => "Copied the waiting notes as one message",
            (true, Chosen::Note(_)) => "Copied this note",
        });
        if copied {
            let chosen = list.chosen;
            let sent = self.notes.iter_mut().enumerate().filter(|(at, _)| match chosen {
                Chosen::Waiting => true,
                Chosen::Note(note) => note == *at,
            });
            sent.for_each(|(_, note)| note.copied = true);
        }
        list.settle(&self.notes);
        self.mode = Mode::List(list);
        self.mark_notes();
        After::Stay
    }

    fn leaving_key(&mut self, key: Key) -> After {
        match key {
            Key::Char('y') | Key::Ctrl(b'c') => After::Leave,
            Key::Focus(_) => After::Ignored,
            Key::Char('p') => self.open_list(),
            _ => {
                self.mode = Mode::Read;
                After::Stay
            }
        }
    }

    /// Draws the note being written under whatever it was opened from.
    fn draw_writing(&mut self, screen: &mut Screen) {
        let path = self.shown_path();
        let Mode::Write(writing) = &mut self.mode else { return };
        let width = screen.w.saturating_sub(2);
        writing.width = width;
        let text_rows = writing.draft.height(width).clamp(1, DRAFT_ROWS);
        let panel = (text_rows + 2).min(screen.h);
        // The panel's heading goes over the row of hints of what is under it: those keys are not live.
        let left = if panel >= 3 { panel - 1 } else { panel };
        let (path, place) = match &mut writing.about {
            About::Picked { picked, .. } => {
                screen.above(left, |screen| self.reader.draw(screen));
                (path, notes::place(picked.kept, picked.removed))
            }
            About::Listed { note, list } => {
                screen.above(left, |screen| list.draw(screen, &self.notes));
                (self.notes[*note].path.clone(), self.notes[*note].place())
            }
        };
        let top = screen.h - panel;
        let text_at = top + usize::from(panel >= 3);
        if panel >= 3 {
            screen.put(0, top, screen.w, &" ".repeat(screen.w), Style::PLAIN);
            list::put_place(screen, (1, top), width, "Note on ", &path, &place, Style::PLAIN.bold());
            screen.wash(0, top, screen.w, Wash::Bar);
        }
        let hint = usize::from(panel >= 2);
        writing.draft.draw(screen, 1, text_at, width, screen.h - text_at - hint);
        if hint == 1 && writing.discarding {
            screen.put_fit(1, screen.h - 1, width, "Discard this note? y discard · n keep writing", Style::PLAIN);
        } else if hint == 1 {
            help::draw_hint(screen, "Enter keep for review", &["^N new line", "^Y copy now"], "Esc cancel");
        }
    }

    /// Asks, over the hints, whether to close with notes that were never copied.
    fn draw_leaving(&self, screen: &mut Screen) {
        if screen.h == 0 {
            return;
        }
        let (y, waiting) = (screen.h - 1, self.waiting());
        let notes = if waiting == 1 { "note was" } else { "notes were" };
        let ask = format!("{waiting} {notes} not copied. y close anyway · p see them · n stay");
        screen.put(0, y, screen.w, &" ".repeat(screen.w), Style::PLAIN);
        screen.put_fit(1, y, screen.w.saturating_sub(2), &ask, Style::PLAIN);
    }
}

impl Pane for Review {
    fn draw(&mut self, screen: &mut Screen) {
        if let Some(text) = self.to_copy.take() {
            screen.copy(&text);
        }
        match &mut self.mode {
            Mode::Read | Mode::Pick => self.reader.draw(screen),
            Mode::Write(_) => self.draw_writing(screen),
            Mode::List(list) => list.draw(screen, &self.notes),
            Mode::Leaving => {
                self.reader.draw(screen);
                self.draw_leaving(screen);
            }
        }
    }

    fn help(&self) -> Page {
        use help::{heading, key, note};
        match self.mode {
            Mode::Read | Mode::Leaving => self.reader.help(),
            Mode::Pick => Page::new(vec![
                heading("Lines for a note"),
                key("↑↓   ", "move (k j)"),
                key("n N  ", "next/prev change"),
                key("v    ", "range start/end"),
                key("Enter", "write the note"),
                key("Esc  ", "back to reading"),
                note("A removed line keeps"),
                note("its old number."),
            ]),
            Mode::Write(_) => Page::new(vec![
                heading("Writing a note"),
                key("Enter", "keep for review"),
                key("^Y   ", "copy this one now"),
                key("^N   ", "new line"),
                key("Esc  ", "cancel"),
                note("Copying one note"),
                note("leaves the others."),
            ]),
            Mode::List(_) => Page::new(vec![
                heading("Notes"),
                key("↑↓   ", "choose (k j)"),
                key("y    ", "copy the choice"),
                key("e    ", "edit (Enter)"),
                key("d    ", "remove"),
                key("Esc  ", "back to the patch"),
                note("A copy goes to the"),
                note("terminal's clipboard."),
                note("Milo sends nothing to"),
                note("an agent: paste it."),
                note("Notes are not saved."),
            ]),
        }
    }

    fn notice(&self) -> Option<&'static str> {
        self.reader.notice()
    }

    fn next_frame(&self) -> Option<std::time::Duration> {
        self.reader.next_frame()
    }

    fn editing(&self) -> bool {
        matches!(&self.mode, Mode::Write(writing) if !writing.discarding)
    }

    fn holds_drafts(&self) -> bool {
        self.waiting() > 0 || !matches!(self.mode, Mode::Read)
    }

    fn paste(&mut self, text: Option<&str>) -> After {
        match (&mut self.mode, text) {
            (Mode::Write(writing), Some(text)) => {
                writing.draft.paste(text);
                After::Stay
            }
            _ => After::Ignored,
        }
    }

    fn key(&mut self, key: Key) -> After {
        match self.mode {
            Mode::Read => self.read_key(key),
            Mode::Pick => self.pick_key(key),
            Mode::Write(_) => self.write_key(key),
            Mode::List(_) => self.list_key(key),
            Mode::Leaving => self.leaving_key(key),
        }
    }
}

/// Opens the review in a floating Rex pane beside whoever ran this, and says
/// what it will show. That is the only answer a program with no terminal gets,
/// so it names each file whose edits could not be shown.
pub fn float(dir: &Path, file: &Path, from: Option<&str>) -> Result<String, String> {
    let target = rex::Target::current()
        .ok_or("--float needs a Rex pane; run milo review PATCH without --float in a terminal")?;
    let repo = project(dir)?;
    let text = patch_text(file)?;
    let patch = Patch::parse(&text)?;
    let paths = patch.paths();
    let mut report = String::new();
    let mut shown = 0;
    for path in paths {
        match handed_lines(&repo, &patch, path) {
            Ok(_) => shown += 1,
            Err(why) => {
                let name: String = path.chars().filter(|c| !c.is_control()).collect();
                let _ = writeln!(report, "Not shown: {name}: {why}");
            }
        }
    }
    let file = std::path::absolute(file).map_err(|_| "the current directory cannot be read".to_string())?;
    rex::open_review(&target, &repo.root, &file, from)?;
    let files = if paths.len() == 1 { "file" } else { "files" };
    let total = paths.len();
    Ok(format!(
        "Opened a floating reader with the patch's edits in {shown} of {total} {files}.\n{report}\
         It shows the patch as handed over. Milo checked that the files still hold its lines, not who wrote them."
    ))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::fixture::Fixture;

    pub(super) fn drawn(review: &mut Review, w: usize, h: usize) -> Vec<String> {
        let mut screen = Screen::sized(w, h);
        review.draw(&mut screen);
        screen.text()
    }

    /// A project with committed work, then uncommitted edits by a person, by
    /// the agent whose patch is returned, and by a second agent, in one file.
    pub(super) fn mixed_project() -> (Fixture, std::path::PathBuf) {
        let f = Fixture::new();
        let lines = |edit: &dyn Fn(usize) -> String| (1..=12).map(edit).collect::<String>();
        f.write("app.txt", &lines(&|n| format!("line {n}\n")));
        f.write("staged.txt", "one\ntwo\nthree\n");
        f.commit("base");
        // Before the agent started: an unstaged edit, a staged edit and a file Git does not know.
        f.write("staged.txt", "one, staged by a person\ntwo\nthree\n");
        f.git(&["add", "staged.txt"]);
        f.write("scratch.txt", "a person's notes\n");
        f.write(
            "app.txt",
            &lines(&|n| match n {
                1 => "line 1, by a person\n".into(),
                4 => "line 4, by the agent\n".into(),
                8 => "line 8, by another agent\n".into(),
                12 => "line 12, by a person later\n".into(),
                n => format!("line {n}\n"),
            }),
        );
        f.write("staged.txt", "one, staged by a person\ntwo\nthree, by the agent\n");
        f.write("new.txt", "made by the agent\n");
        // What the agent hands over: its own three edits, against the last commit.
        let patch = "\
diff --git a/app.txt b/app.txt
--- a/app.txt
+++ b/app.txt
@@ -1,7 +1,7 @@
 line 1
 line 2
 line 3
-line 4
+line 4, by the agent
 line 5
 line 6
 line 7
diff --git a/new.txt b/new.txt
new file mode 100644
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+made by the agent
diff --git a/staged.txt b/staged.txt
--- a/staged.txt
+++ b/staged.txt
@@ -1,3 +1,3 @@
 one
 two
-three
+three, by the agent
";
        let file = f.root.parent().unwrap().join("handed.patch");
        std::fs::write(&file, patch).unwrap();
        (f, file)
    }

    #[test]
    fn only_the_handed_over_edits_are_marked_among_other_uncommitted_work() {
        let (f, patch) = mixed_project();
        let mut review = Review::open(&f.root, &patch, Some("the agent")).unwrap();
        let rows = drawn(&mut review, 70, 16);
        assert!(rows[0].contains("app.txt") && rows[0].ends_with("1 of 3 · patch from the agent"), "{rows:?}");
        let marked: Vec<_> = rows[1..15].iter().filter(|row| row.contains(" + ") || row.contains(" - ")).collect();
        assert_eq!(marked, ["    - line 4", "  4 + line 4, by the agent"], "{rows:?}");
        // The person's and the other agent's lines are in the file, unmarked.
        let plain = |text: &str| rows.iter().any(|row| row.ends_with(text) && !row.contains('+'));
        assert!(plain("line 1, by a person") && plain("line 8, by another agent"), "{rows:?}");

        review.key(Key::Char(']'));
        let rows = drawn(&mut review, 70, 6);
        assert!(rows[0].contains("new.txt") && rows[0].contains("2 of 3"), "{rows:?}");
        assert_eq!(rows[1], " 1 + made by the agent");

        review.key(Key::Char(']'));
        let rows = drawn(&mut review, 70, 6);
        assert_eq!(&rows[1..5], [" 1   one, staged by a person", " 2   two", "   - three", " 3 + three, by the agent"]);
        // A file of the project that the patch does not name is never shown.
        review.key(Key::Char(']'));
        assert!(drawn(&mut review, 70, 6)[0].contains("app.txt"));
    }

    #[test]
    fn a_narrow_review_still_says_which_file_it_shows_and_how_to_reach_the_others() {
        let (f, patch) = mixed_project();
        let mut review = Review::open(&f.root, &patch, Some("omp")).unwrap();
        let last = |review: &mut Review, w: usize| drawn(review, w, 8).pop().unwrap();
        // With the place in the heading, the hint names the keys alone.
        let rows = drawn(&mut review, 90, 8);
        assert!(rows[0].ends_with("1 of 3 · patch from omp"), "{rows:?}");
        assert_eq!(rows[7], " [ ] files · ↑↓ scroll · q close · c changes only · n N changes · i note · ? help");
        // Where they do not all fit, the key for a note outlasts the ones a reader already knows.
        assert_eq!(last(&mut review, 60), " [ ] files · c changes only · n N changes · i note · ? help");
        // A heading with no room for the place leaves it to the hint, which gives up other keys first.
        let rows = drawn(&mut review, 28, 8);
        assert!(rows[0].ends_with("patch from omp") && !rows[0].contains(" of "), "{rows:?}");
        assert_eq!(rows[7], " [ ] file 1 of 3 · ? help");
        assert_eq!(last(&mut review, 20), " [ ] 1/3 · ? help");
        review.key(Key::Char(']'));
        assert_eq!(last(&mut review, 28), " [ ] file 2 of 3 · ? help");
        // A file with no diff to move through keeps the same place and keys.
        let now = std::fs::read_to_string(f.root.join("staged.txt")).unwrap();
        f.write("staged.txt", &now.replace("three, by the agent", "three, rewritten"));
        review.key(Key::Char(']'));
        assert_eq!(last(&mut review, 28), " [ ] file 3 of 3 · ? help");
        assert_eq!(last(&mut review, 70), " [ ] files · q close · ? help");
        // A patch of one file has nowhere else to go.
        let single = f.root.parent().unwrap().join("single.patch");
        std::fs::write(&single, "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+made by the agent\n").unwrap();
        let mut review = Review::open(&f.root, &single, Some("omp")).unwrap();
        assert_eq!(last(&mut review, 28), " q close · i note · ? help");
    }

    #[test]
    fn a_file_whose_handed_over_lines_were_rewritten_says_so_and_shows_no_diff() {
        let (f, patch) = mixed_project();
        let now = std::fs::read_to_string(f.root.join("app.txt")).unwrap();
        f.write("app.txt", &now.replace("line 4, by the agent", "line 4, rewritten by a person"));
        let mut review = Review::open(&f.root, &patch, None).unwrap();
        let rows = drawn(&mut review, 70, 8);
        assert!(rows[0].ends_with("1 of 3 · handed-over patch"), "{rows:?}");
        let text = rows[1..].join(" ");
        assert!(text.contains("no longer holds the lines the patch left"), "{rows:?}");
        assert!(!text.contains("line 1, by a person") && !text.contains("rewritten"), "{rows:?}");

        // Reloading takes in a file that fits again.
        f.write("app.txt", &now);
        review.key(Key::Char('r'));
        assert!(drawn(&mut review, 70, 16).iter().any(|row| row.ends_with("+ line 4, by the agent")));
    }

    #[test]
    fn the_review_is_read_only_and_leaves_the_project_as_it_was() {
        let (f, patch) = mixed_project();
        let before = std::fs::read(f.root.join("app.txt")).unwrap();
        let mut review = Review::open(&f.root, &patch, None).unwrap();
        drawn(&mut review, 70, 16);
        review.key(Key::Char('e'));
        let rows = drawn(&mut review, 70, 16);
        assert_eq!(rows[15], " Read-only: this shows a patch · ? help");
        assert_eq!(std::fs::read(f.root.join("app.txt")).unwrap(), before);
        review.help().assert_rows_fit(26);
    }

    #[cfg(unix)]
    #[test]
    fn a_patch_cannot_show_a_file_outside_the_project_or_through_a_link() {
        let f = Fixture::folder();
        let outside = f.root.parent().unwrap().join("private.txt");
        std::fs::write(&outside, "secret\n").unwrap();
        std::os::unix::fs::symlink(&outside, f.root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(f.root.parent().unwrap(), f.root.join("up")).unwrap();
        let hunk = "@@ -0,0 +1 @@\n+secret\n";
        let patch = format!(
            "--- a/../private.txt\n+++ b/../private.txt\n{hunk}--- a/link.txt\n+++ b/link.txt\n{hunk}--- a/up/private.txt\n+++ b/up/private.txt\n{hunk}--- {0}\n+++ {0}\n{hunk}",
            outside.display()
        );
        let file = f.root.parent().unwrap().join("outside.patch");
        std::fs::write(&file, patch).unwrap();
        let mut review = Review::open(&f.root, &file, None).unwrap();
        for _ in 0..4 {
            let rows = drawn(&mut review, 90, 6);
            assert!(!rows.iter().any(|row| row.contains("secret")), "{rows:?}");
            assert!(rows[1..].iter().any(|row| row.contains("not a file inside") || row.contains("link")), "{rows:?}");
            review.key(Key::Char(']'));
        }
    }

    #[test]
    fn a_missing_empty_or_shapeless_patch_opens_nothing() {
        let f = Fixture::folder();
        let file = f.root.parent().unwrap().join("none.patch");
        let refused = |text: Option<&str>| {
            match text {
                Some(text) => std::fs::write(&file, text).unwrap(),
                None => drop(std::fs::remove_file(&file)),
            }
            Review::open(&f.root, &file, None).err().unwrap()
        };
        assert!(refused(None).starts_with("there is no patch at "));
        assert!(refused(Some("")).contains("the patch is empty"));
        assert!(refused(Some("M app.txt\nA new.txt\n")).contains("names no file"));
    }

    #[test]
    fn a_name_for_the_patch_is_one_short_plain_line() {
        assert_eq!(name("Claude Code"), Some("Claude Code"));
        assert!(name("").is_none() && name(" ").is_none() && name("a\u{1b}[31m").is_none());
        assert!(name(&"n".repeat(41)).is_none());
    }
}
