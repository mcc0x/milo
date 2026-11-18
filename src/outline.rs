//! Keeps the outline's tree, pending files and recent marks up to date.

mod attention;
mod draw;
mod input;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::git::{Kind, PendingFile, Repo};
use crate::help::Page;
use crate::keys::Key;
use crate::pane::{After, Pane};
use crate::reader::{OpenReader, ReadFile, Reader};
use crate::screen::Screen;
use crate::tree::{Folds, Tree};
use crate::walk;
use crate::watch::Scope;

use self::attention::Attention;
use self::draw::Layout;

/// How long a file mark pulses after a recent change.
const LIVE: Duration = Duration::from_secs(3);
/// Time between the two states of a pulsing mark.
const PULSE: Duration = Duration::from_millis(500);
/// How long something said in place of the key hint stays.
const NOTE: Duration = Duration::from_secs(4);
/// A second click on the same item within this long opens it.
pub(crate) const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub struct Outline {
    repo: Repo,
    /// Pending files, with all uncommitted entries before the unpushed entries.
    /// A path can occur once in each group.
    pending: Vec<PendingFile>,
    /// Without git, the listing the tree was built from, to notice changes.
    listing: Vec<String>,
    tree: Tree,
    folds: Folds,
    /// Pending files, each once, most recently written first.
    recent: Vec<usize>,
    /// Files that changed while the outline was open, by path, and when.
    file_marks: HashMap<String, Instant>,
    attention: Attention,
    pending_only: bool,
    sel: usize,
    top: usize,
    /// The cursor was put where it is by a click, so the view stays put.
    by_click: bool,
    /// First column shown when the strip has more than fits.
    strip_col: usize,
    /// The layout last drawn, which clicks and paging refer to.
    painted: Layout,
    opener: OpenReader,
    /// The path last clicked and when, to tell a double click.
    clicked: Option<(String, Instant)>,
    /// Something to say in place of the key hint, and when it was said.
    note: Option<(String, Instant)>,
    /// A file being read here, in place of the outline, outside Rex.
    reading: Option<Reader>,
}

impl Outline {
    pub fn new(repo: Repo, inside_rex: bool) -> Outline {
        let mut outline = Outline {
            repo,
            pending: Vec::new(),
            listing: Vec::new(),
            tree: Tree::default(),
            folds: Folds::default(),
            recent: Vec::new(),
            file_marks: HashMap::new(),
            attention: Attention::new(Instant::now()),
            pending_only: false,
            sel: 0,
            top: 0,
            by_click: false,
            strip_col: 0,
            painted: Layout::default(),
            opener: OpenReader::new(inside_rex),
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
        self.tree.rebuild(self.listing.iter().map(String::as_str), &self.pending);
        self.relayout(selected);
    }

    /// Rebuilds what the outline shows, returning the old files so new work can be marked.
    fn load(&mut self) -> Option<Vec<PendingFile>> {
        if !self.repo.git {
            self.relist();
            self.build_folder();
            return None;
        }
        let selected = self.selected_path();
        self.repo.read();
        let files = self.repo.pending()?;
        let tracked = self.repo.tracked()?;
        let files = std::mem::replace(&mut self.pending, files);
        self.tree.rebuild(tracked, &self.pending);
        self.update_recent();
        self.relayout(selected);
        Some(files)
    }

    fn update_recent(&mut self) {
        self.recent.clear();
        let mut once = HashSet::new();
        for (i, f) in self.pending.iter().enumerate() {
            if once.insert(f.path.as_str()) {
                self.recent.push(i);
            }
            // Pending work opens a folder unless the user closed it.
            // Suggested folds persist so committing work does not hide it.
            let mut p = f.path.as_str();
            while let Some((dir, _)) = p.rsplit_once('/') {
                self.folds.suggest(dir);
                p = dir;
            }
        }
        let files = &self.pending;
        self.recent.sort_by_key(|&i| std::cmp::Reverse(files[i].stamp.map(|s| s.written)));
    }

    /// Rebuilds the outline and marks changed pending files.
    fn reload(&mut self) {
        let Some(old) = self.load() else { return };
        let now = Instant::now();
        let following = self.attention.following(now);
        let newest = self.mark_files(&old, now).filter(|_| following).map(str::to_owned);
        if let Some(newest) = newest {
            self.follow(&newest);
        }
    }

    /// Marks files that changed, and finds the path written most recently.
    fn mark_files(&mut self, old: &[PendingFile], now: Instant) -> Option<&str> {
        let before: HashMap<(&str, bool), &PendingFile> =
            old.iter().map(|f| ((f.path.as_str(), f.unpushed), f)).collect();
        let mut newest: Option<&PendingFile> = None;
        for f in &self.pending {
            if before.get(&(f.path.as_str(), f.unpushed)).is_some_and(|old| *old == f) {
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

    /// Forgets marks and notes that have had their time.
    fn expire(&mut self, now: Instant) {
        let keep = |at| !self.attention.mark_left(at, now).is_zero();
        self.file_marks.retain(|_, at| keep(*at));
        if self.note.as_ref().is_some_and(|(_, at)| now.saturating_duration_since(*at) >= NOTE) {
            self.note = None;
        }
    }

    fn selected_path(&self) -> Option<String> {
        self.tree.row(self.sel).map(|n| n.path.clone())
    }

    /// Recomputes the visible rows, keeping the cursor on `selected`.
    fn relayout(&mut self, selected: Option<String>) {
        self.tree.layout(|p| self.folds.is_open(p), self.pending_only);
        let at = selected.and_then(|p| self.tree.row_of(&p)).unwrap_or(self.sel);
        self.sel = at.min(self.tree.rows.len().saturating_sub(1));
    }

    /// Applies the folder choices. A plain tree must list its open folders again
    /// because it does not keep the contents of closed folders.
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

    /// Brings the latest marked file into view and says why the cursor moved.
    /// If the user closed a folder, stop at the nearest visible ancestor.
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

    /// Opens the reader in this pane, or replaces the floating reader in Rex.
    fn open_file(&mut self, path: &str) {
        match self.opener.open(&self.repo, ReadFile::Working(path)) {
            Ok(reader) => self.reading = reader,
            Err(why) => self.say(why),
        }
    }
}

impl Pane for Outline {
    fn draw(&mut self, screen: &mut Screen) {
        if let Some(reader) = &mut self.reading {
            reader.draw(screen);
        } else {
            self.draw_outline(screen);
        }
    }

    fn key(&mut self, key: Key) -> After {
        self.outline_key(key)
    }

    fn watch_scope(&self) -> Scope {
        self.repo.watch_scope()
    }

    /// Checks both the outline and the reader when they share a pane.
    fn check(&mut self) -> bool {
        let reader_settled = self.reading.as_mut().is_none_or(Pane::check);
        if self.repo.busy() {
            return false;
        }
        if self.repo.git {
            if self.repo.poll(&self.pending) {
                self.reload();
            }
        } else if self.relist() {
            self.build_folder();
        }
        reader_settled
    }

    fn next_frame(&self) -> Option<Duration> {
        if self.reading.is_some() {
            return None;
        }
        let now = Instant::now();
        let mark = self
            .file_marks
            .values()
            .map(|&at| if now.saturating_duration_since(at) < LIVE { PULSE } else { self.attention.mark_left(at, now) })
            .min();
        let note = self.note.as_ref().map(|(_, at)| NOTE.saturating_sub(now.saturating_duration_since(*at)));
        [mark, note].into_iter().flatten().min()
    }

    fn help(&self) -> Page {
        self.reading.as_ref().map_or_else(|| Page::outline(self.repo.git), Pane::help)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::fixture::Fixture;

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
        let pending: Vec<_> = o.pending.iter().map(|f| (f.path.as_str(), f.unpushed)).collect();
        assert_eq!(pending, [("note.txt", false), ("page.txt", true)]);
        assert!(o.tree.row(o.tree.row_of("note.txt").unwrap()).unwrap().uncommitted);
        assert!(!o.tree.row(o.tree.row_of("page.txt").unwrap()).unwrap().uncommitted);

        f.push();
        assert!(o.check());
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("1 to commit"));
        let pending: Vec<_> = o.pending.iter().map(|f| (f.path.as_str(), f.unpushed)).collect();
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

        let mut reader = Reader::open(&f.root.join("edit.txt")).unwrap();
        let mut screen = Screen::sized(65, 12);
        reader.draw(&mut screen);
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

        o.key(Key::Char('p'));
        o.key(Key::Enter);
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
        o.key(Key::Char('e'));
        let all = draw(&mut o, 40, 12).text();
        assert!(all.iter().any(|row| row.contains("▾ deep")));
        assert!(all.iter().any(|row| row.contains("util.rs")));
        o.key(Key::Char('e'));
        assert_eq!(draw(&mut o, 40, 12).text(), before);
    }

    #[test]
    fn pending_only_hides_untouched_files_and_the_hint_says_how_to_show_them_again() {
        let f = project();
        let mut o = open(&f);
        let all = draw(&mut o, 65, 12).text();
        assert!(all.iter().any(|row| row.contains("README.md")));
        assert!(all[11].contains("c changed only"));

        o.key(Key::Char('c'));
        let pending = draw(&mut o, 65, 12).text();
        assert!(!pending.iter().any(|row| row.contains("README.md")));
        assert!(pending.iter().any(|row| row.contains("main.rs")));
        assert!(pending.iter().any(|row| row.contains("notes.txt")));
        assert!(pending[11].contains("c all files"));
        o.key(Key::Char('c'));
        assert_eq!(draw(&mut o, 65, 12).text(), all);
    }

    #[test]
    fn the_outline_follows_writes_only_when_the_keyboard_is_elsewhere() {
        let f = project();
        let mut o = open(&f);
        o.key(Key::Focus(true));
        let before = o.selected_path().unwrap();
        assert_ne!(before, "src/deep/util.rs");
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        assert!(o.check());
        assert_eq!(o.selected_path().as_deref(), Some(before.as_str()));
        let focused = draw(&mut o, 65, 12).text();
        assert!(focused.iter().any(|row| row.contains("util.rs")));
        assert!(!focused[11].contains("following"));

        o.key(Key::Focus(false));
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
    fn the_twelfth_row_shows_the_key_hint() {
        let f = project();
        let mut o = open(&f);
        assert!(!draw(&mut o, 65, 11).text().iter().any(|row| row.contains("? help")));
        let tall = draw(&mut o, 65, 12).text();
        assert!(tall[11].contains("? help"), "{tall:?}");
    }

    #[test]
    fn the_outline_hint_uses_key_names_and_keeps_help_last_when_narrow() {
        let f = Fixture::folder();
        f.write("page.txt", "a file\n");
        let mut o = open(&f);
        assert_eq!(draw(&mut o, 60, 12).text()[11], " Enter open · e open all · q quit · ? help");
        assert_eq!(draw(&mut o, 24, 12).text()[11], " Enter open · ? help");
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
        let mut screen = draw(&mut o, 60, 11);
        let y = screen.text().iter().position(|row| row.contains("README.md")).unwrap();
        let before = o.selected_path();
        screen.resize(90, 11);
        o.key(Key::Click(65, y));
        assert_eq!(o.selected_path(), before);

        o.draw(&mut screen);
        o.key(Key::Click(65, y));
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
        o.key(Key::End);
        assert_eq!(o.strip_col, 2);
        o.key(Key::Left);
        assert_eq!(o.strip_col, 1);
    }

    #[test]
    fn next_and_previous_pending_follow_tree_order_open_folders_and_wrap() {
        let f = project();
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        let mut o = open(&f);
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));
        o.key(Key::Char('p'));
        o.key(Key::Enter);
        assert!(!draw(&mut o, 40, 12).text().iter().any(|row| row.contains("util.rs")));
        o.key(Key::Char('n'));
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));
        assert!(draw(&mut o, 40, 12).text().iter().any(|row| row.contains("util.rs")));
        for (key, paths) in [
            ('n', ["src/main.rs", "notes.txt", "src/deep/util.rs"]),
            ('N', ["notes.txt", "src/main.rs", "src/deep/util.rs"]),
        ] {
            for path in paths {
                o.key(Key::Char(key));
                assert_eq!(o.selected_path().as_deref(), Some(path));
            }
        }
    }

    #[test]
    fn reading_in_the_outline_pane_returns_to_the_same_cursor_on_quit() {
        let f = project();
        let mut o = open(&f);
        o.key(Key::Char('n'));
        let before = draw(&mut o, 65, 12);
        let rows = before.text();
        let cursor = rows.iter().position(|row| row.contains("notes.txt")).unwrap();
        assert!(before.bar_at(cursor));
        assert_eq!(o.key(Key::Enter), After::Stay);
        let reader = draw(&mut o, 65, 12).text();
        assert!(reader[0].contains("notes.txt"));
        assert_eq!(reader[1], " 1 + one");
        assert_eq!(o.key(Key::Char('q')), After::Stay);
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
        o.key(Key::Enter);
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
