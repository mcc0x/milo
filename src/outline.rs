//! Tracks the project's files, what is pending, and what has changed since it opened.

mod attention;
mod draw;
mod input;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::git::{Commit, Ends, FileChange, Kind, Repo};
use crate::help::Page;
use crate::tree::{Folds, Tree};
use crate::view::View;
use crate::watch::Scope;
use crate::{rex, walk};

use self::attention::Attention;
use self::draw::Layout;

/// How long a row counts as being written right now, and pulses.
const LIVE: Duration = Duration::from_secs(3);
const PULSE: Duration = Duration::from_millis(500);
/// How long something said in place of the key hint stays.
const NOTE: Duration = Duration::from_secs(4);
/// Recent commits to keep for the timeline of dots and the list beneath it.
const HISTORY_LIMIT: usize = 300;

pub struct Outline {
    repo: Repo,
    /// Pending files, all uncommitted entries before the committed ones.
    /// A path can occur once in each group.
    files: Vec<FileChange>,
    /// Without git, the listing the tree was built from, to notice changes.
    listing: Vec<String>,
    tree: Tree,
    folds: Folds,
    /// Pending files, each once, most recently written first.
    recent: Vec<usize>,
    /// Recent commits, newest first, and what they were read for.
    history: Vec<Commit>,
    history_key: Ends,
    show_history: bool,
    /// Files that changed while the outline was open, by path, and when.
    file_marks: HashMap<String, Instant>,
    /// Commits that landed while the outline was open, by hash, and when.
    new_commits: HashMap<String, Instant>,
    attention: Attention,
    changed_only: bool,
    sel: usize,
    top: usize,
    /// The cursor was put where it is by a click, so the view stays put.
    by_click: bool,
    /// First column shown when the strip has more than fits.
    strip_col: usize,
    /// The layout last drawn, which clicks and paging refer to.
    painted: Layout,
    inside_rex: bool,
    /// The floating window the last file was opened in, to replace it.
    popup: Option<String>,
    /// The path last clicked and when, to tell a double click.
    clicked: Option<(String, Instant)>,
    /// Something to say in place of the key hint, and when it was said.
    note: Option<(String, Instant)>,
    /// A file being read here, in place of the outline, outside Rex.
    reading: Option<View>,
}

struct Replaced {
    files: Vec<FileChange>,
    history: Vec<Commit>,
}

impl Outline {
    pub fn new(repo: Repo, inside_rex: bool) -> Outline {
        let mut outline = Outline {
            repo,
            files: Vec::new(),
            listing: Vec::new(),
            tree: Tree::default(),
            folds: Folds::default(),
            recent: Vec::new(),
            history: Vec::new(),
            history_key: Ends::default(),
            show_history: true,
            file_marks: HashMap::new(),
            new_commits: HashMap::new(),
            attention: Attention::new(Instant::now()),
            changed_only: false,
            sel: 0,
            top: 0,
            by_click: false,
            strip_col: 0,
            painted: Layout::default(),
            inside_rex,
            popup: None,
            clicked: None,
            note: None,
            reading: None,
        };
        outline.load();
        // Start where the work is, not at the top of the tree.
        let first = outline.tree.rows.iter().position(|r| {
            let n = outline.tree.node(r.node);
            !n.dir && n.kind != Kind::None
        });
        outline.sel = first.unwrap_or(0);
        outline
    }

    /// What to watch for changes to the outline.
    pub fn watch(&self) -> Scope {
        self.repo.watch()
    }

    /// Refreshes the plain-folder listing, reporting whether its paths changed.
    fn relist(&mut self) -> bool {
        let listing = walk::list(&self.repo.root, |p| self.folds.is_open(p));
        if listing == self.listing {
            return false;
        }
        self.listing = listing;
        true
    }

    fn build_folder(&mut self) {
        let selected = self.selected_path();
        self.tree.rebuild(self.listing.iter().map(String::as_str), &self.files);
        self.relayout(selected);
    }

    /// Reads the current project. Returns the lists replaced by a git refresh.
    fn load(&mut self) -> Option<Replaced> {
        if !self.repo.git {
            self.relist();
            self.build_folder();
            return None;
        }
        let selected = self.selected_path();
        self.repo.read();
        let files = self.repo.pending()?;
        let tracked = self.repo.tracked()?;
        let files = std::mem::replace(&mut self.files, files);
        self.tree.rebuild(tracked, &self.files);
        self.update_recent();
        self.relayout(selected);
        let history = self.load_history();
        Some(Replaced { files, history })
    }

    /// History only changes when the branch or what is pushed does.
    fn load_history(&mut self) -> Vec<Commit> {
        if self.repo.ends() != &self.history_key
            && let Some(history) = self.repo.history(HISTORY_LIMIT)
        {
            self.history_key.clone_from(self.repo.ends());
            return std::mem::replace(&mut self.history, history);
        }
        Vec::new()
    }

    fn update_recent(&mut self) {
        self.recent.clear();
        let mut once = HashSet::new();
        for (i, f) in self.files.iter().enumerate() {
            if once.insert(f.path.as_str()) {
                self.recent.push(i);
            }
            // Pending work opens a folder unless you closed it. A commit
            // shouldn't fold away a folder you've been looking at.
            let mut p = f.path.as_str();
            while let Some((dir, _)) = p.rsplit_once('/') {
                self.folds.suggest(dir);
                p = dir;
            }
        }
        let files = &self.files;
        self.recent.sort_by_key(|&i| std::cmp::Reverse(files[i].stamp.map(|s| s.written)));
    }

    /// Reloads the project, highlighting new commits and recent file changes.
    fn reload(&mut self) {
        let Some(old) = self.load() else { return };
        let now = Instant::now();
        self.highlight_commits(&old.history, now);
        let following = self.attention.following(now);
        let newest = self.mark_files(&old.files, now).filter(|_| following).map(str::to_owned);
        if let Some(newest) = newest {
            self.follow(&newest);
        }
    }

    fn highlight_commits(&mut self, before: &[Commit], now: Instant) {
        // With nothing known before, every commit would count as new.
        if before.is_empty() {
            return;
        }
        let known: HashSet<&str> = before.iter().map(|c| c.hash.as_str()).collect();
        for c in self.history.iter().filter(|c| !known.contains(c.hash.as_str())) {
            self.new_commits.insert(c.hash.clone(), now);
        }
    }

    /// Marks files that changed, and finds the path written most recently.
    fn mark_files(&mut self, old: &[FileChange], now: Instant) -> Option<&str> {
        let before: HashMap<(&str, bool), &FileChange> =
            old.iter().map(|f| ((f.path.as_str(), f.committed), f)).collect();
        let mut newest: Option<&FileChange> = None;
        for f in &self.files {
            if before.get(&(f.path.as_str(), f.committed)).is_some_and(|old| *old == f) {
                continue;
            }
            if let Some(at) = self.file_marks.get_mut(&f.path) {
                *at = now;
            } else {
                self.file_marks.insert(f.path.clone(), now);
            }
            if newest.is_none_or(|n| n.stamp.map(|s| s.written) <= f.stamp.map(|s| s.written)) {
                newest = Some(f);
            }
        }
        newest.map(|f| f.path.as_str())
    }

    /// Looks for changes made outside the outline, and takes them in.
    /// Returns false if it is too soon to tell, and worth looking again.
    pub fn check(&mut self) -> bool {
        // A file being read here is kept up to date, and so is what it covers.
        if let Some(view) = &mut self.reading {
            view.check();
        }
        if self.repo.busy() {
            // Git is part-way through changing many files at once.
            return false;
        }
        if self.repo.git {
            if self.repo.poll(&self.files) {
                self.reload();
            }
        } else if self.relist() {
            self.build_folder();
        }
        true
    }

    /// How long the screen stays right if nothing happens: until a mark
    /// fades or pulses, or a note has been up long enough.
    pub fn next_frame(&self) -> Option<Duration> {
        if self.reading.is_some() {
            return None;
        }
        let now = Instant::now();
        let marks = self.file_marks.values().chain(self.new_commits.values());
        let mark = marks
            .map(|&at| if now.saturating_duration_since(at) < LIVE { PULSE } else { self.attention.mark_left(at, now) })
            .min();
        let note = self.note.as_ref().map(|(_, at)| NOTE.saturating_sub(now.saturating_duration_since(*at)));
        [mark, note].into_iter().flatten().min()
    }

    /// Forgets marks and notes that have had their time.
    fn expire(&mut self, now: Instant) {
        let keep = |at| !self.attention.mark_left(at, now).is_zero();
        self.file_marks.retain(|_, at| keep(*at));
        self.new_commits.retain(|_, at| keep(*at));
        if self.note.as_ref().is_some_and(|(_, at)| now.saturating_duration_since(*at) >= NOTE) {
            self.note = None;
        }
    }

    fn selected_path(&self) -> Option<String> {
        self.tree.row(self.sel).map(|n| n.path.clone())
    }

    /// Recomputes the visible rows, keeping the cursor on `selected`.
    fn relayout(&mut self, selected: Option<String>) {
        self.tree.layout(|p| self.folds.is_open(p), self.changed_only);
        let at = selected.and_then(|p| self.tree.row_of(&p)).unwrap_or(self.sel);
        self.sel = at.min(self.tree.rows.len().saturating_sub(1));
    }

    /// Applies a change to which folders are open. Without git a folder's
    /// contents are only read once it is open, so that means listing again.
    fn refold(&mut self, selected: Option<String>) {
        if self.repo.git {
            self.relayout(selected);
        } else {
            self.load();
        }
    }

    /// Opens whatever folders hide `path` and puts the cursor on it.
    fn reveal(&mut self, path: String) {
        self.folds.open_above(&path);
        self.relayout(Some(path));
    }

    /// Brings the file being worked on into view and says so, since the
    /// view moving by itself would otherwise be a puzzle. Folders you
    /// closed stay closed: the cursor goes to the nearest visible one.
    fn follow(&mut self, path: &str) {
        let mut shown = path;
        self.sel = loop {
            if let Some(row) = self.tree.row_of(shown) {
                break row;
            }
            match shown.rsplit_once('/') {
                Some((dir, _)) => shown = dir,
                None => return,
            }
        };
        self.by_click = false;
        let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
        self.say(format!("following {name}"));
    }

    /// Shows a line in place of the key hint for a few seconds.
    fn say(&mut self, text: impl Into<String>) {
        self.note = Some((text.into(), Instant::now()));
    }

    /// Opens a file to read: in a floating window inside Rex, in place of
    /// the one opened before, and otherwise here in this pane.
    fn open_file(&mut self, path: &str) {
        if !self.inside_rex {
            self.reading = View::open(&self.repo.root.join(path));
            return;
        }
        match rex::open(&self.repo.root, path, self.popup.as_deref()) {
            Ok(layer) => self.popup = Some(layer),
            Err(why) => {
                self.popup = None;
                self.say(why);
            }
        }
    }

    /// The help page for whichever view is showing.
    pub fn help(&self) -> Page {
        if self.reading.is_some() { Page::reader() } else { Page::outline(self.repo.git) }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::fixture::Fixture;
    use crate::keys::Key;
    use crate::screen::Screen;

    fn project() -> Fixture {
        let f = Fixture::new();
        f.write("src/main.rs", "fn main() {}\n");
        f.write("src/deep/util.rs", "fn util() {}\n");
        f.write("README.md", "# hello\n");
        f.commit("first");
        f.write("src/main.rs", "fn main() {\n    run();\n}\n");
        f.write("notes.txt", "one\ntwo\n");
        f
    }

    fn open(f: &Fixture) -> Outline {
        Outline::new(Repo::open(&f.root).unwrap(), false)
    }

    fn draw(o: &mut Outline, w: usize, h: usize) -> Screen {
        let mut screen = Screen::sized(w, h);
        o.draw(&mut screen);
        screen
    }

    fn file_row<'a>(rows: &'a [String], name: &str) -> &'a str {
        rows.iter().find(|row| row.split_whitespace().any(|word| word == name)).unwrap().as_str()
    }

    #[test]
    fn the_header_distinguishes_work_to_commit_from_work_to_push() {
        let f = Fixture::new();
        f.write("page.txt", "before\n");
        f.commit("first");
        let mut o = open(&f);
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("✓ all committed · no remote"));

        f.push();
        assert!(o.check());
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("✓ all committed and pushed"));

        f.write("page.txt", "after\n");
        assert!(o.check());
        assert!(o.tree.row(o.tree.row_of("page.txt").unwrap()).unwrap().uncommitted);
        f.commit("second");
        f.write("note.txt", "not committed\n");
        assert!(o.check());
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("1 to commit · 1 to push"));
        assert!(draw(&mut o, 29, 10).text()[0].ends_with("○1 ↑1"));
        let pending: Vec<_> = o.files.iter().map(|f| (f.path.as_str(), f.committed)).collect();
        assert_eq!(pending, [("note.txt", false), ("page.txt", true)]);
        assert!(o.tree.row(o.tree.row_of("note.txt").unwrap()).unwrap().uncommitted);
        assert!(!o.tree.row(o.tree.row_of("page.txt").unwrap()).unwrap().uncommitted);

        f.push();
        assert!(o.check());
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("1 to commit"));
        let pending: Vec<_> = o.files.iter().map(|f| (f.path.as_str(), f.committed)).collect();
        assert_eq!(pending, [("note.txt", false)]);
        assert_eq!(o.tree.row(o.tree.row_of("page.txt").unwrap()).unwrap().kind, Kind::None);
    }

    #[test]
    fn file_rows_name_the_change_and_the_reader_agrees_on_its_size() {
        let f = Fixture::new();
        f.write("edit.txt", "before\nsame\n");
        f.write("gone.txt", "remove me\n");
        f.write("old.txt", "keep this text\n");
        f.commit("first");
        f.write("edit.txt", "after\nextra\nsame\n");
        f.write("staged.txt", "stage me\n");
        f.write("loose.txt", "not yet tracked\n");
        f.git(&["add", "staged.txt"]);
        f.git(&["mv", "old.txt", "moved.txt"]);
        fs::remove_file(f.root.join("gone.txt")).unwrap();

        let mut o = open(&f);
        let rows = draw(&mut o, 65, 12).text();
        for (name, label) in [
            ("edit.txt", "edited +2 −1"),
            ("staged.txt", "new +1"),
            ("gone.txt", "removed −1"),
            ("moved.txt", "moved"),
            ("loose.txt", "new +1"),
        ] {
            let row = file_row(&rows, name);
            let tail = row.split_once(name).unwrap().1.split_whitespace().collect::<Vec<_>>().join(" ");
            assert_eq!(tail, label, "{row}");
        }
        assert!(!rows.iter().any(|row| row.contains("old.txt")), "{rows:?}");

        let mut view = View::open(&f.root.join("edit.txt")).unwrap();
        let mut screen = Screen::sized(65, 12);
        view.draw(&mut screen);
        let counts = |row: &str| row.split_whitespace().rev().take(2).collect::<Vec<_>>().join(" ");
        assert_eq!(counts(file_row(&rows, "edit.txt")), counts(&screen.text()[0]));
    }

    #[test]
    fn a_folder_opens_for_pending_work_but_stays_closed_when_the_user_closes_it() {
        let f = project();
        let mut o = open(&f);
        let rows = draw(&mut o, 40, 12).text();
        assert!(rows.iter().any(|row| row == " ▾ src"));
        assert!(rows.iter().any(|row| row.contains("main.rs")));

        o.on_key(Key::Char('p'));
        o.on_key(Key::Enter);
        assert!(draw(&mut o, 40, 12).text().iter().any(|row| row.trim_start() == "▸ src 1"));
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        assert!(o.check());
        let rows = draw(&mut o, 40, 12).text();
        assert!(rows.iter().any(|row| row.contains("▸ src 2")), "{rows:?}");
        assert!(!rows.iter().any(|row| row.contains("main.rs") || row.contains("util.rs")), "{rows:?}");
    }

    #[test]
    fn opening_every_folder_twice_restores_the_previous_folds() {
        let f = project();
        let mut o = open(&f);
        let before = draw(&mut o, 40, 12).text();
        assert!(before.iter().any(|row| row.contains("▸ deep")));
        o.on_key(Key::Char('e'));
        let all = draw(&mut o, 40, 12).text();
        assert!(all.iter().any(|row| row.contains("▾ deep")));
        assert!(all.iter().any(|row| row.contains("util.rs")));
        o.on_key(Key::Char('e'));
        assert_eq!(draw(&mut o, 40, 12).text(), before);
    }

    #[test]
    fn changed_only_hides_untouched_files_and_the_hint_says_how_to_show_them_again() {
        let f = project();
        let mut o = open(&f);
        let all = draw(&mut o, 65, 12).text();
        assert!(all.iter().any(|row| row.contains("README.md")));
        assert!(all[11].contains("c changed only"));

        o.on_key(Key::Char('c'));
        let changed = draw(&mut o, 65, 12).text();
        assert!(!changed.iter().any(|row| row.contains("README.md")));
        assert!(changed.iter().any(|row| row.contains("main.rs")));
        assert!(changed.iter().any(|row| row.contains("notes.txt")));
        assert!(changed[11].contains("c all files"));
        o.on_key(Key::Char('c'));
        assert_eq!(draw(&mut o, 65, 12).text(), all);
    }

    #[test]
    fn the_outline_follows_writes_only_when_the_keyboard_is_elsewhere() {
        let f = project();
        let mut o = open(&f);
        o.on_key(Key::Focus(true));
        let before = o.selected_path().unwrap();
        assert_ne!(before, "src/deep/util.rs");
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        assert!(o.check());
        assert_eq!(o.selected_path().as_deref(), Some(before.as_str()));
        let focused = draw(&mut o, 65, 12).text();
        assert!(focused.iter().any(|row| row.contains("util.rs")));
        assert!(!focused[11].contains("following"));

        o.on_key(Key::Focus(false));
        f.write("src/deep/util.rs", "fn util() { even_more(); }\n");
        assert!(o.check());
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));
        assert_eq!(draw(&mut o, 65, 12).text()[11].trim_start(), "following util.rs");
    }

    #[test]
    fn at_seven_rows_the_tree_becomes_a_strip_of_pending_files() {
        let f = project();
        let mut o = open(&f);
        let tree = draw(&mut o, 40, 8).text();
        assert!(tree.iter().any(|row| row.contains("▾ src")));
        assert!(tree.iter().any(|row| row.contains("README.md")));
        let strip = draw(&mut o, 40, 7).text();
        assert!(strip.iter().any(|row| row.contains("main.rs src")));
        assert!(strip.iter().any(|row| row.contains("notes.txt")));
        assert!(!strip.iter().any(|row| row.contains("▾ src") || row.contains("README.md")));
    }

    #[test]
    fn a_twentieth_row_makes_room_for_history_beneath_the_tree() {
        let f = project();
        let mut o = open(&f);
        assert!(!draw(&mut o, 65, 19).text().iter().any(|row| row.contains("History")));
        let tall = draw(&mut o, 65, 20).text();
        let history = tall.iter().position(|row| row.trim_start() == "History").unwrap();
        assert!(tall[..history].iter().any(|row| row.contains("main.rs")));
        assert!(tall[history + 1..].iter().any(|row| row.contains("first")));
    }

    #[test]
    fn a_ninetieth_column_puts_history_beside_the_tree() {
        let f = project();
        let mut o = open(&f);
        assert!(!draw(&mut o, 89, 11).text().iter().any(|row| row.contains("History")));
        let wide = draw(&mut o, 90, 11).text();
        assert!(wide[0].starts_with(" project") && wide[0].ends_with("History"), "{wide:?}");
        assert!(wide.iter().any(|row| row.contains("first")));
    }

    #[test]
    fn a_wide_tree_has_columns_for_the_kind_and_size_of_a_change() {
        let f = project();
        let mut o = open(&f);
        let narrow = draw(&mut o, 51, 10).text();
        let wide = draw(&mut o, 52, 10).text();
        assert!(file_row(&narrow, "main.rs").ends_with("main.rs"));
        let row = file_row(&wide, "main.rs");
        assert!(row.contains("edited") && row.ends_with("+3 −1"), "{row}");
    }

    #[test]
    fn clicks_refer_to_the_painted_layout_until_the_next_frame() {
        let f = project();
        let mut o = open(&f);
        let rows = draw(&mut o, 90, 11).text();
        let y = rows.iter().position(|row| row.contains("README.md")).unwrap();
        let before = o.selected_path();
        o.on_key(Key::Char('t'));
        o.on_key(Key::Click(65, y));
        assert_eq!(o.selected_path(), before);

        draw(&mut o, 90, 11);
        o.on_key(Key::Click(65, y));
        assert_eq!(o.selected_path().as_deref(), Some("README.md"));
    }

    #[test]
    fn the_strip_can_step_back_from_its_last_column_before_redrawing() {
        let f = Fixture::new();
        for path in ["a.txt", "b.txt", "c.txt"] {
            f.write(path, "pending\n");
        }
        let mut o = open(&f);
        draw(&mut o, 20, 1);
        o.on_key(Key::End);
        assert_eq!(o.strip_col, 2);
        o.on_key(Key::Left);
        assert_eq!(o.strip_col, 1);
    }

    #[test]
    fn next_and_previous_pending_follow_tree_order_open_folders_and_wrap() {
        let f = project();
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        let mut o = open(&f);
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));
        o.on_key(Key::Char('p'));
        o.on_key(Key::Enter);
        assert!(!draw(&mut o, 40, 12).text().iter().any(|row| row.contains("util.rs")));
        o.on_key(Key::Char('n'));
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));
        assert!(draw(&mut o, 40, 12).text().iter().any(|row| row.contains("util.rs")));
        for (key, paths) in [
            ('n', ["src/main.rs", "notes.txt", "src/deep/util.rs"]),
            ('N', ["notes.txt", "src/main.rs", "src/deep/util.rs"]),
        ] {
            for path in paths {
                o.on_key(Key::Char(key));
                assert_eq!(o.selected_path().as_deref(), Some(path));
            }
        }
    }

    #[test]
    fn reading_in_the_outline_pane_returns_to_the_same_cursor_on_quit() {
        let f = project();
        let mut o = open(&f);
        o.on_key(Key::Char('n'));
        let before = draw(&mut o, 65, 12);
        let rows = before.text();
        let cursor = rows.iter().position(|row| row.contains("notes.txt")).unwrap();
        assert!(before.bar_at(cursor));
        assert!(!o.on_key(Key::Enter));
        let reader = draw(&mut o, 65, 12).text();
        assert!(reader[0].contains("notes.txt"));
        assert_eq!(reader[1], " 1 + one");
        assert!(!o.on_key(Key::Char('q')));
        let back = draw(&mut o, 65, 12);
        assert_eq!(back.text(), rows);
        assert!(back.bar_at(cursor));
    }

    #[test]
    fn a_plain_folder_has_no_git_counts_and_picks_up_new_files_on_check() {
        let f = Fixture::folder();
        f.write("sub/a.txt", "a\n");
        f.write("b.txt", "b\n");
        let mut o = open(&f);
        assert_eq!(draw(&mut o, 40, 8).text(), [" project", " ▸ sub", " • b.txt", "", "", "", "", ""]);
        o.on_key(Key::Enter);
        assert!(draw(&mut o, 40, 8).text().iter().any(|row| row.contains("a.txt")));
        f.write("sub/new.txt", "new\n");
        assert!(o.check());
        let rows = draw(&mut o, 40, 8).text();
        assert_eq!(rows[0], " project");
        assert!(rows.iter().any(|row| row.contains("new.txt")), "{rows:?}");
    }

    #[test]
    fn a_failed_git_read_keeps_the_tree_until_the_repository_answers_again() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        let mut o = open(&f);
        let before = draw(&mut o, 60, 10).text();
        let index = f.root.join(".git/index");
        let saved = fs::read(&index).unwrap();
        f.write(".git/index", "corrupt index");
        assert!(o.check());
        let failed = draw(&mut o, 60, 10).text();
        assert!(failed[0].ends_with("git did not answer"), "{failed:?}");
        assert_eq!(failed[1..], before[1..]);

        fs::write(index, saved).unwrap();
        assert!(o.check());
        assert_eq!(draw(&mut o, 60, 10).text(), before);
    }
}
