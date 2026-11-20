//! Keeps the outline's tree, pending files and recent marks up to date.

mod attention;
mod draw;
mod input;
mod untracked;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::git::{Kind, PendingFile, Repo};
use crate::help::{self, Page};
use crate::keys::Key;
use crate::pane::{After, Pane};
use crate::preferences::{FoldStore, IconMode, Preferences, ProjectMode};
use crate::reader::{OpenReader, ReadFile, Reader};
use crate::screen::{Screen, Style};
use crate::tree::{Folds, Tree, folders_above};
use crate::ui::{self, ADD, DEL};
use crate::walk;
use crate::watch::{Events, Ignored, Scope};

use self::attention::Attention;
use self::draw::{ChangeColumns, Layout};

/// How long something said in place of the key hint stays.
const NOTE: Duration = Duration::from_secs(4);

pub struct Outline {
    repo: Repo,
    /// Pending files, with all uncommitted entries before the unpushed entries.
    /// A path can occur once in each group.
    pending: Vec<PendingFile>,
    untracked: untracked::Untracked,
    /// Without git, the listing the tree was built from, to notice changes.
    listing: walk::Listing,
    folder_filter: Option<Arc<Ignored>>,
    tree: Tree,
    /// Measured once per row layout, and only when there is room for details.
    change_columns: Option<ChangeColumns>,
    folds: Folds,
    fold_store: Option<FoldStore>,
    icons: IconMode,
    static_marks: bool,
    text_git_marks: bool,
    follow_activity: bool,
    remember: bool,
    peek: bool,
    promotion: Option<(ProjectMode, Option<&'static str>)>,
    /// Pending files, each once, most recently written first.
    recent: Vec<usize>,
    recent_dirty: bool,
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
    clicked: Option<input::Click>,
    /// Something to say in place of the key hint, and when it was said.
    note: Option<(String, Instant)>,
    /// A file being read here, in place of the outline, outside Rex.
    reading: Option<Reader>,
}

impl Outline {
    #[cfg(test)]
    pub(crate) fn new(repo: Repo, inside_rex: bool) -> Self {
        Self::build(repo, inside_rex, &Preferences::default(), false, None)
    }

    pub fn with_preferences(repo: Repo, inside_rex: bool, prefs: &Preferences) -> Self {
        let remember = !cfg!(test);
        let fold_store = if remember && repo.git { FoldStore::for_worktree(&repo.root) } else { None };
        Self::build(repo, inside_rex, prefs, remember, fold_store)
    }

    pub fn into_peek(mut self) -> Self {
        self.peek = true;
        self
    }

    fn build(repo: Repo, inside_rex: bool, prefs: &Preferences, remember: bool, fold_store: Option<FoldStore>) -> Self {
        let folds =
            fold_store.as_ref().and_then(|store| store.load().ok()).map_or_else(Folds::default, Folds::remembered);
        let pending_only = repo.git && prefs.changes_only;
        let icons = if cfg!(test) { prefs.icons } else { prefs.icons.effective() };
        let mut outline = Outline {
            repo,
            pending: Vec::new(),
            untracked: untracked::Untracked::default(),
            listing: walk::Listing::default(),
            folder_filter: None,
            tree: Tree::default(),
            change_columns: None,
            folds,
            fold_store,
            icons,
            static_marks: prefs.static_marks,
            text_git_marks: prefs.text_git_marks,
            follow_activity: prefs.follow_activity,
            remember,
            peek: false,
            promotion: None,
            recent: Vec::new(),
            recent_dirty: false,
            attention: Attention::new(Instant::now()),
            pending_only,
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
        if outline.repo.git {
            outline.load();
        } else {
            outline.refresh_folder(None);
        }
        // Start where the work is, not at the top of the tree.
        let first = outline.tree.rows.iter().position(|r| {
            let n = outline.tree.node(r.node);
            !n.dir && n.kind != Kind::None
        });
        outline.sel = first.unwrap_or(0);
        outline
    }

    fn refresh_folder(&mut self, selected: Option<Rc<str>>) {
        let listing = walk::list(&self.repo.root, |p| self.folds.is_open(p));
        let changed = listing.paths != self.listing.paths;
        if changed {
            self.tree.rebuild(listing.paths.iter().map(String::as_str), &[]);
            self.relayout(selected);
        }
        if cfg!(target_os = "macos") && (changed || self.folder_filter.is_none()) {
            let collapsed = listing
                .paths
                .iter()
                .filter(|p| {
                    p.ends_with('/')
                        && !self.folds.is_open(p)
                        // Git can begin sending descendant events before a new .git is valid.
                        && !p.eq_ignore_ascii_case(".git/")
                })
                .map(|p| self.repo.root.join(p))
                .collect();
            self.folder_filter = Some(Arc::new(Ignored { events: Events::Collapsed(collapsed), ..Ignored::default() }));
        }
        self.listing = listing;
    }

    fn active_folder_filter(&self) -> Option<&Arc<Ignored>> {
        // An inline Reader may need Git metadata inside a closed folder.
        self.folder_filter.as_ref().filter(|_| self.reading.is_none())
    }

    fn check_folder(&mut self) {
        let Some(repo) = self.repo.discover_git() else {
            self.refresh_folder(self.selected_path());
            return;
        };
        let (prefs, notice) = Preferences::for_pane(Some(&repo.root));
        self.promotion = Some((prefs.project_mode, notice));
        if self.remember {
            self.icons = prefs.icons.effective();
            self.static_marks = prefs.static_marks;
            self.text_git_marks = prefs.text_git_marks;
            self.follow_activity = prefs.follow_activity;
            self.pending_only = prefs.changes_only;
            self.fold_store = FoldStore::for_worktree(&repo.root);
            let choices = self.fold_store.as_ref().and_then(|store| store.load().ok());
            if let Some(choices) = choices.filter(|choices| !choices.is_empty()) {
                self.folds = Folds::remembered(choices);
            }
        }
        self.repo = repo;
        self.listing = walk::Listing::default();
        self.folder_filter = None;
        self.load();
    }

    /// Rebuilds what the outline shows, returning the old files so new work can be marked.
    fn load(&mut self) -> Option<Vec<PendingFile>> {
        let selected = self.selected_path();
        self.repo.read();
        let mut files = self.repo.pending()?;
        let tracked = self.repo.tracked()?;
        self.untracked.retain_details(&self.pending, &mut files);
        let files = std::mem::replace(&mut self.pending, files);
        self.tree.rebuild(tracked, &self.pending);
        self.folds.prune(|path| self.tree.has_folder(path));
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
            for dir in folders_above(&f.path) {
                self.folds.suggest(dir);
            }
        }
        self.recent_dirty = true;
    }

    /// Rebuilds the outline and marks changed pending files.
    fn reload(&mut self) {
        let Some(old) = self.load() else { return };
        let now = Instant::now();
        let following = self.follow_activity && self.attention.following(now);
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
            self.attention.mark(&f.path, now);
            if newest.is_none_or(|n| n.stamp.map(|s| s.written) <= f.stamp.map(|s| s.written)) {
                newest = Some(f);
            }
        }
        newest.map(|f| f.path.as_str())
    }

    /// Forgets marks and notes that have had their time.
    fn expire(&mut self, now: Instant) {
        self.attention.expire_marks(now);
        if self.note.as_ref().is_some_and(|(_, at)| now.saturating_duration_since(*at) >= NOTE) {
            self.note = None;
        }
    }

    fn selected_path(&self) -> Option<Rc<str>> {
        self.tree.row(self.sel).map(|n| n.path.clone())
    }

    /// Recomputes rows, keeping the cursor on the selected path or its visible folder.
    fn relayout(&mut self, selected: Option<Rc<str>>) {
        self.tree.layout(|p| self.folds.is_open(p), self.pending_only);
        self.painted.tree_rows = 0..0;
        self.change_columns = None;
        let at = selected.as_deref().and_then(|path| self.visible_row(path)).unwrap_or(self.sel);
        self.sel = at.min(self.tree.rows.len().saturating_sub(1));
    }

    fn visible_row(&self, path: &str) -> Option<usize> {
        std::iter::once(path).chain(folders_above(path)).find_map(|p| self.tree.row_of(p))
    }

    /// Applies the folder choices. A plain tree must list its open folders again
    /// because it does not keep the contents of closed folders.
    fn refold(&mut self, selected: Option<Rc<str>>) {
        if self.repo.git {
            self.relayout(selected);
        } else {
            // Even an empty folder changes coverage when opened or closed.
            self.folder_filter = None;
            self.refresh_folder(selected);
        }
    }

    /// Opens whatever folders hide `path` and puts the cursor on it.
    fn reveal(&mut self, path: Rc<str>) {
        if let Some(row) = self.tree.row_of(&path) {
            self.sel = row;
        } else if self.folds.open_above(&path) {
            self.relayout(Some(path));
        }
    }

    /// Brings the latest marked file into view and says why the cursor moved.
    /// If the user closed a folder, stop at the nearest visible ancestor.
    fn follow(&mut self, path: &str) {
        let Some(row) = self.visible_row(path) else {
            return;
        };
        self.sel = row;
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
            Ok(reader) => {
                self.reading = reader;
                if self.reading.is_some() {
                    self.painted.tree_rows = 0..0;
                }
            }
            Err(why) => self.say(why),
        }
    }

    fn draw_at(&mut self, screen: &mut Screen, now: Instant) {
        self.scan_untracked(now, false);
        if let Some(reader) = &mut self.reading {
            reader.draw(screen);
        } else {
            self.draw_outline(screen, now);
        }
    }
}

impl Pane for Outline {
    fn draw(&mut self, screen: &mut Screen) {
        self.draw_at(screen, Instant::now());
    }

    fn key(&mut self, key: Key) -> After {
        self.outline_key(key, Instant::now())
    }

    fn notice(&self) -> Option<&'static str> {
        self.promotion.and_then(|(_, notice)| notice)
    }

    fn promoted_worktree(&self) -> Option<(&Path, ProjectMode)> {
        self.promotion.map(|(mode, _)| (self.repo.root.as_path(), mode))
    }

    fn watch_scope(&self) -> Scope {
        let mut scope = self.repo.watch_scope();
        if let Some(ignored) = self.active_folder_filter() {
            scope.ignored = Arc::clone(ignored);
        }
        scope
    }

    fn watch_scope_after_input(&self, current: &Scope) -> Option<Scope> {
        let changed = match self.active_folder_filter() {
            Some(ignored) => !Arc::ptr_eq(ignored, &current.ignored),
            None => matches!(current.ignored.events, Events::Collapsed(_)),
        };
        changed.then(|| self.watch_scope())
    }

    fn reader_opener(&mut self) -> Option<&mut OpenReader> {
        Some(&mut self.opener)
    }

    fn git_busy(&self) -> bool {
        self.repo.busy()
            || self
                .reading
                .as_ref()
                .and_then(Reader::working_repo)
                .is_some_and(|repo| repo.root != self.repo.root && repo.busy())
    }

    /// Checks both the outline and the reader when they share a pane.
    fn check(&mut self) -> bool {
        let reader_settled = self.reading.as_mut().is_none_or(Pane::check);
        if self.repo.git {
            self.check_untracked(Instant::now());
            if self.repo.poll(&self.pending) {
                self.reload();
            }
        } else {
            self.check_folder();
        }
        reader_settled
    }

    fn filesystem_changed(&mut self) {
        if self.repo.git {
            self.untracked.request_sweep();
        }
        if let Some(reader) = &mut self.reading {
            reader.filesystem_changed();
        }
    }

    fn next_frame(&self) -> Option<Duration> {
        let now = Instant::now();
        let sweep = self.untracked.next_sweep(now);
        if let Some(reader) = &self.reading {
            return [sweep, reader.next_frame()].into_iter().flatten().min();
        }
        let mark = self.attention.next_mark_frame(now, self.static_marks);
        let note = self.note.as_ref().map(|(_, at)| NOTE.saturating_sub(now.saturating_duration_since(*at)));
        let counts = (self.painted.body_rows > 0).then(|| self.untracked.next_frame()).flatten();
        [mark, note, counts, sweep].into_iter().flatten().min()
    }

    fn help(&self) -> Page {
        use help::{HelpRow, heading, key, sample};

        if let Some(reader) = &self.reading {
            return reader.help();
        }
        let mut rows = Vec::new();
        if self.repo.git {
            let name = |kind| ui::kind_style(kind).bold();
            rows.extend([
                heading("Files"),
                sample("edited  ", name(Kind::Modified), "changed"),
                sample("new     ", name(Kind::Untracked), "added"),
                sample("removed ", name(Kind::Deleted), "deleted"),
                sample("moved   ", name(Kind::Renamed), "renamed"),
                sample("conflict", name(Kind::Conflict), "needs merging"),
                sample("bold    ", Style::PLAIN.bold(), "to commit"),
                sample("regular ", Style::PLAIN, "to push"),
                sample("docs 3  ", Style::PLAIN, "3 changed"),
                sample("+2      ", ADD, "lines added"),
                sample("?       ", ui::FAINT, "not counted"),
                sample("−1      ", DEL, "lines removed"),
                sample(if self.static_marks { "•       " } else { "● ○     " }, Style::PLAIN.bold(), "being written"),
                sample("•       ", Style::PLAIN, "just changed"),
                HelpRow::new(),
            ]);
            if self.text_git_marks {
                rows.extend([
                    sample("M A D R U", Style::PLAIN, "edit/new/delete/move/conflict"),
                    sample("C P      ", Style::PLAIN, "to commit / to push"),
                    HelpRow::new(),
                ]);
            }
        }
        rows.extend([
            heading("Tree keys"),
            key("↑↓   ", "move (k j)"),
            key("Enter", "toggle folder"),
            key("Enter", "open file"),
            key("Space", "toggle folder"),
            key("←    ", "close or parent"),
            key("→    ", "open folder"),
            key("{ }  ", "prev/next folder"),
            key("K J  ", "prev/next sibling"),
            key("p    ", "parent folder"),
            key("e    ", "open all/restore"),
        ]);
        if self.repo.git {
            rows.extend([key("n N  ", "next/prev change"), key("c    ", "changed only")]);
        }
        rows.extend([key("r    ", "reload"), key("q    ", "quit")]);
        if self.repo.git {
            rows.extend([
                HelpRow::new(),
                heading("Strip keys"),
                key("←→   ", "scroll (h l)"),
                key("g G  ", "first/last"),
            ]);
        }
        Page::new(rows)
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

    fn draw_at(o: &mut Outline, w: usize, h: usize, now: Instant) -> Screen {
        let mut screen = Screen::sized(w, h);
        o.draw_at(&mut screen, now);
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
        assert!(draw(&mut o, 60, 10).text()[0].ends_with("✓ all committed"));

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
    fn a_remote_without_a_branch_to_compare_does_not_look_missing() {
        let f = Fixture::new();
        f.write("page.txt", "a file\n");
        f.commit("first");
        f.git(&["remote", "add", "origin", "https://example.invalid/project.git"]);
        let mut o = open(&f);
        assert!(draw(&mut o, 80, 24).text()[0].ends_with("✓ all committed"));
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
    fn untracked_line_counts_share_one_byte_budget_and_resume_on_the_next_frame() {
        let f = Fixture::new();
        let body = "x\n".repeat(128 << 10);
        for i in 0..6 {
            f.write(&format!("file{i}.txt"), &body);
        }
        f.write("zero.txt", "");

        let mut o = open(&f);
        assert!(o.pending.iter().all(|file| file.lines_pending && file.stamp.is_none()));
        let first = draw(&mut o, 80, 12).text();
        assert!(file_row(&first, "file0.txt").ends_with("+131072"));
        assert!(file_row(&first, "file3.txt").ends_with('?'));
        assert!(file_row(&first, "zero.txt").ends_with('?'));
        assert!(o.next_frame().is_some());

        let second = draw(&mut o, 80, 12).text();
        assert!(file_row(&second, "file5.txt").ends_with("+131072"));
        assert!(file_row(&second, "zero.txt").ends_with("new"));
        assert!(o.pending.iter().all(|file| !file.lines_pending));
    }

    #[test]
    fn untracked_counts_prioritize_the_scrolled_viewport() {
        let f = Fixture::new();
        for i in 0..96 {
            f.write(&format!("file{i:02}.txt"), "a line\nanother");
        }
        let mut o = open(&f);
        draw(&mut o, 40, 9);
        assert!(o.pending.iter().all(|file| file.lines_pending), "narrow trees have no count column");

        o.key(Key::End);
        let rows = draw(&mut o, 80, 9).text();
        let last = file_row(&rows, "file95.txt");
        assert!(last.trim_end_matches('▐').ends_with("+2"), "{last}");
        assert!(o.pending.iter().find(|f| f.path == "file00.txt").unwrap().lines_pending);
        assert_eq!(o.pending.iter().filter(|f| !f.lines_pending).count(), 8);
    }

    #[test]
    fn untracked_counts_survive_reload_and_are_invalidated_for_an_edited_path() {
        let f = Fixture::new();
        f.write("a.txt", "one\n");
        f.write("b.txt", "one\n");
        let mut o = open(&f);
        draw(&mut o, 80, 12);

        f.write("c.txt", "one\n");
        assert!(o.check());
        assert_eq!(o.pending.iter().map(|f| f.lines_pending).collect::<Vec<_>>(), [false, false, true]);
        let rows = draw(&mut o, 80, 12).text();
        assert!(file_row(&rows, "c.txt").ends_with("+1"));

        f.write("b.txt", "one\ntwo\n");
        assert!(!o.repo.poll(&o.pending), "Git does not report untracked content edits");
        assert!(o.check());
        assert_eq!(o.pending.iter().map(|f| f.lines_pending).collect::<Vec<_>>(), [false, true, false]);
        let rows = draw(&mut o, 80, 12).text();
        assert!(file_row(&rows, "a.txt").ends_with("+1"));
        assert!(file_row(&rows, "b.txt").ends_with("+2"));
        assert!(o.attention.marked_at("b.txt", false).is_some());
    }

    #[test]
    fn manual_reload_refreshes_an_offscreen_untracked_count_without_a_notification() {
        let f = Fixture::new();
        for i in 0..60 {
            f.write(&format!("file{i:02}.txt"), "one\n");
        }
        let mut o = open(&f);
        let tail = "file59.txt";
        o.key(Key::Focus(true));
        o.key(Key::End);
        assert!(file_row(&draw(&mut o, 80, 9).text(), tail).trim_end_matches('▐').ends_with("+1"));
        o.key(Key::Home);
        draw(&mut o, 80, 9);

        f.write(tail, "one\ntwo\n");
        let at = Instant::now();
        o.outline_key(Key::Char('r'), at);
        let wait = o.untracked.next_sweep(at).expect("reload should schedule an offscreen sweep");
        draw_at(&mut o, 80, 9, at + wait);
        o.outline_key(Key::End, at + wait);
        let rows = draw_at(&mut o, 80, 9, at + wait).text();
        assert!(file_row(&rows, tail).trim_end_matches('▐').ends_with("+2"));
    }

    #[test]
    fn one_notification_refreshes_a_large_offscreen_set_without_continued_checks() {
        const FILES: usize = 2_049;
        let f = Fixture::new();
        for i in 0..FILES {
            f.write(&format!("file{i:04}.txt"), "one\n");
        }
        let mut o = open(&f);
        let (head, tail) = ("file0000.txt", "file2048.txt");
        let tail_index = o.pending.iter().position(|f| f.path == tail).unwrap();
        let head_index = o.pending.iter().position(|f| f.path == head).unwrap();
        o.key(Key::Focus(true));
        o.key(Key::End);
        assert!(file_row(&draw(&mut o, 80, 9).text(), tail).trim_end_matches('▐').ends_with("+1"));
        o.key(Key::Home);
        assert!(file_row(&draw(&mut o, 80, 9).text(), head).trim_end_matches('▐').ends_with("+1"));
        for _ in 0..FILES {
            if o.untracked.next_frame().is_none() {
                break;
            }
            draw(&mut o, 80, 9);
        }
        assert_eq!(o.untracked.next_frame(), None, "initial timestamp discovery must finish");
        o.static_marks = true;

        f.write(head, "one\ntwo\n");
        f.write(tail, "one\ntwo\n");
        o.filesystem_changed();
        assert!(o.check());
        assert!(o.pending[head_index].lines_pending, "visible edits should be detected first");
        assert!(!o.pending[tail_index].lines_pending, "the first batch must not scan the whole set");
        let mut now = Instant::now();
        assert!(o.untracked.next_sweep(now).is_some());
        assert!(o.next_frame().is_some_and(|delay| delay < Duration::from_secs(1)));

        f.write("added-after-check", "not part of the timestamp sweep\n");
        let mut batches = 0;
        while let Some(wait) = o.untracked.next_sweep(now) {
            assert!(batches < 100, "the sweep must finish");
            now += wait;
            draw_at(&mut o, 80, 9, now);
            batches += 1;
        }
        assert!(batches >= 32, "offscreen work should stay in short batches");
        assert!(o.pending[tail_index].lines_pending);
        assert!(o.attention.marked_at(tail, false).is_some());
        assert_eq!(o.pending.len(), FILES, "timestamp batches should not refresh Git status");

        o.key(Key::End);
        let rows = draw_at(&mut o, 80, 9, now + Duration::from_secs(5)).text();
        assert!(file_row(&rows, tail).trim_end_matches('▐').ends_with("+2"));
        assert_eq!(o.next_frame(), None, "no further frames once sweep and marks have finished");
    }

    #[test]
    fn binary_and_large_files_are_not_retried_and_links_are_not_followed() {
        let f = Fixture::new();
        f.write("binary.txt", "a\0b");
        f.write("large.txt", &"x".repeat((256 << 10) + 1));
        std::os::unix::fs::symlink("missing.txt", f.root.join("link.txt")).unwrap();
        let mut o = open(&f);
        let rows = draw(&mut o, 80, 12).text();
        assert!(file_row(&rows, "link.txt").ends_with("+1"));
        assert!(file_row(&rows, "binary.txt").ends_with("new"));
        assert!(file_row(&rows, "large.txt").ends_with("new"));
        assert!(o.pending.iter().all(|f| !f.lines_pending));
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
    fn remembered_folds_survive_a_branch_round_trip_and_pane_restart() {
        let f = Fixture::new();
        f.write("src/main.rs", "fn main() {}\n");
        f.commit("base");
        f.git(&["branch", "without-deep"]);
        f.write("src/deep/util.rs", "fn util() {}\n");
        f.commit("add deep folder");

        let state = Fixture::folder();
        let store = || FoldStore::in_state_dir(&state.root, &f.root);
        let open =
            || Outline::build(Repo::open(&f.root).unwrap(), false, &Preferences::default(), false, Some(store()));
        let mut outline = open();
        outline.sel = outline.tree.row_of("src/").unwrap();
        outline.key(Key::Right);
        outline.sel = outline.tree.row_of("src/deep/").unwrap();
        outline.key(Key::Right);
        assert!(outline.tree.row_of("src/deep/util.rs").is_some());

        f.git(&["switch", "without-deep"]);
        assert!(outline.check());
        assert!(!outline.tree.has_folder("src/deep/"));
        assert_eq!(store().load().unwrap().get("src/deep/"), Some(&true));
        let mut restarted_while_absent = open();
        assert!(!restarted_while_absent.tree.has_folder("src/deep/"));

        f.git(&["switch", "main"]);
        for pane in [&mut outline, &mut restarted_while_absent] {
            assert!(pane.check());
            assert!(pane.tree.row_of("src/deep/util.rs").is_some());
        }
        assert!(open().tree.row_of("src/deep/util.rs").is_some());
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
    fn filtering_the_selected_file_keeps_the_cursor_in_its_nearest_visible_folder() {
        let f = project();
        let mut o = open(&f);
        o.key(Key::Char('e'));
        o.key(Key::Up);
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));

        o.key(Key::Char('c'));
        assert_eq!(o.selected_path().as_deref(), Some("src/"));
    }

    #[test]
    fn restoring_closed_folders_keeps_the_cursor_on_the_folder_that_hides_it() {
        let f = Fixture::folder();
        f.write("a/b/c.txt", "a file\n");
        f.write("other.txt", "another file\n");
        let mut o = open(&f);
        o.key(Key::Enter);
        o.key(Key::Char('e'));
        o.key(Key::Down);
        o.key(Key::Down);
        assert_eq!(o.selected_path().as_deref(), Some("a/b/c.txt"));

        o.key(Key::Char('e'));
        assert_eq!(o.selected_path().as_deref(), Some("a/b/"));
    }

    #[test]
    fn the_outline_follows_writes_only_when_the_keyboard_is_elsewhere() {
        let f = project();
        let mut o = open(&f);
        o.key(Key::Focus(true));
        let before = o.selected_path().unwrap();
        assert_ne!(before.as_ref(), "src/deep/util.rs");
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        assert!(o.check());
        assert_eq!(o.selected_path().as_deref(), Some(before.as_ref()));
        let focused = draw(&mut o, 65, 12).text();
        assert!(focused.iter().any(|row| row.contains("util.rs")));
        assert!(!focused[11].contains("following"));

        o.key(Key::Focus(false));
        f.write("src/deep/util.rs", "fn util() { even_more(); }\n");
        assert!(o.check());
        assert_eq!(o.selected_path().as_deref(), Some("src/deep/util.rs"));
        let at = o.note.as_ref().unwrap().1;
        assert_eq!(draw_at(&mut o, 65, 12, at).text()[11].trim_start(), "following util.rs");
    }

    #[test]
    fn outline_preferences_keep_selection_and_render_static_git_marks() {
        let f = project();
        let mut o = Outline::with_preferences(
            Repo::open(&f.root).unwrap(),
            false,
            &Preferences {
                icons: IconMode::Off,
                static_marks: true,
                text_git_marks: true,
                follow_activity: false,
                changes_only: true,
                ..Preferences::default()
            },
        );
        o.key(Key::Focus(false));
        let before = o.selected_path().unwrap();
        f.write("src/deep/util.rs", "fn util() { more(); }\n");
        assert!(o.check());
        assert_eq!(o.selected_path().as_deref(), Some(before.as_ref()));
        let rows = draw(&mut o, 35, 12).text();
        let row = file_row(&rows, "util.rs");
        assert!(row.starts_with('•') && row.contains("M C util.rs"), "{row}");
        assert!(!rows.iter().any(|row| row.contains('▾') || row.contains("README.md")), "{rows:?}");
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
    fn clicking_after_batched_scroll_still_selects_the_file_under_the_pointer() {
        let f = Fixture::folder();
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt", "f.txt", "g.txt"] {
            f.write(name, name);
        }
        let mut o = open(&f);
        let at = Instant::now();
        assert!(draw_at(&mut o, 40, 4, at).text()[3].contains("c.txt"));

        o.outline_key(Key::WheelDown, at);
        o.outline_key(Key::Click(4, 3), at);
        assert_eq!(o.selected_path().as_deref(), Some("c.txt"));
        assert!(draw_at(&mut o, 40, 4, at).text()[3].contains("c.txt"));
        o.outline_key(Key::Click(4, 3), at);
        assert!(o.reading.is_some());
    }

    #[test]
    fn unrelated_input_breaks_a_file_double_click() {
        let f = Fixture::folder();
        f.write("page.txt", "a file\n");
        for intervening in [Key::Down, Key::Click(3, 3), Key::Click(15, 1)] {
            let mut o = open(&f);
            let at = Instant::now();
            draw_at(&mut o, 40, 4, at);
            let target = Key::Click(3, 1);
            o.outline_key(target, at);
            o.outline_key(intervening, at);
            o.outline_key(target, at);
            assert!(o.reading.is_none(), "{intervening:?}");
            o.outline_key(target, at);
            assert!(o.reading.is_some(), "{intervening:?}");
        }
    }

    #[test]
    fn a_click_after_the_reader_closes_waits_for_a_tree_frame() {
        let f = Fixture::folder();
        f.write("a.txt", "first\n");
        f.write("b.txt", "second\n");
        let mut o = open(&f);
        let at = Instant::now();
        draw_at(&mut o, 40, 4, at);
        o.outline_key(Key::Enter, at);
        o.outline_key(Key::Char('q'), at);
        o.outline_key(Key::Click(3, 2), at);
        assert_eq!(o.selected_path().as_deref(), Some("a.txt"));

        draw_at(&mut o, 40, 4, at);
        o.outline_key(Key::Click(3, 2), at);
        assert_eq!(o.selected_path().as_deref(), Some("b.txt"));
    }

    #[test]
    fn clicks_after_a_fold_wait_until_new_rows_are_painted() {
        let f = Fixture::folder();
        f.write("a/nested.txt", "nested\n");
        f.write("b.txt", "root\n");
        let mut o = open(&f);
        let at = Instant::now();
        assert!(draw_at(&mut o, 40, 8, at).text()[2].contains("b.txt"));
        o.outline_key(Key::Click(4, 1), at);
        o.outline_key(Key::Click(4, 2), at);
        assert_eq!(o.selected_path().as_deref(), Some("a/"));

        assert!(draw_at(&mut o, 40, 8, at).text()[2].contains("nested.txt"));
        o.outline_key(Key::Click(4, 2), at);
        assert_eq!(o.selected_path().as_deref(), Some("a/nested.txt"));
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
    fn a_pending_jump_in_a_clean_tree_shows_a_timed_notice() {
        let f = Fixture::new();
        f.write("clean.txt", "unchanged\n");
        f.commit("first");
        let mut o = open(&f);
        draw(&mut o, 40, 12);

        for changes_only in [false, true] {
            if changes_only {
                o.key(Key::Char('c'));
            }
            for key in ['n', 'N'] {
                let selected = o.selected_path();
                assert_eq!(selected.is_none(), changes_only);
                assert_eq!(o.key(Key::Char(key)), After::Stay);
                assert_eq!(o.selected_path(), selected);
                let at = o.note.as_ref().unwrap().1;
                let just_before = at + NOTE - Duration::from_millis(1);
                assert_eq!(draw_at(&mut o, 40, 12, just_before).text()[11], " No changed files");
                assert_ne!(draw_at(&mut o, 40, 12, at + NOTE).text()[11], " No changed files");
            }
        }
    }

    #[test]
    fn a_pending_jump_finds_a_folded_singleton_and_only_notices_on_the_file() {
        let f = Fixture::new();
        f.write("src/edit.txt", "before\n");
        f.write("clean.txt", "unchanged\n");
        f.commit("first");
        f.write("src/edit.txt", "after\n");

        for key in ['n', 'N'] {
            let mut o = open(&f);
            o.key(Key::Char('c'));
            o.key(Key::Char('p'));
            o.key(Key::Enter);
            assert_eq!(o.selected_path().as_deref(), Some("src/"));
            assert!(o.tree.row_of("src/edit.txt").is_none());
            draw(&mut o, 40, 12);

            assert_eq!(o.key(Key::Char(key)), After::Stay);
            assert_eq!(o.selected_path().as_deref(), Some("src/edit.txt"));
            assert!(o.note.is_none());
            assert_eq!(o.key(Key::Char(key)), After::Stay);
            assert_eq!(o.selected_path().as_deref(), Some("src/edit.txt"));
            assert_eq!(draw(&mut o, 40, 12).text()[11], " Only changed file");
        }
    }

    #[test]
    fn jumps_and_change_columns_follow_a_rename_into_a_closed_folder() {
        let f = Fixture::new();
        let original = "a long line that stays the same\n";
        f.write("a.txt", original);
        f.write("b.txt", "b\n");
        f.write("folder/keep.txt", "keep\n");
        f.commit("first");
        f.write("a.txt", &format!("{original}an edit\n"));
        let mut o = open(&f);
        let before = draw(&mut o, 80, 24).text();
        assert!(file_row(&before, "a.txt").ends_with("edited +1"));
        o.key(Key::Char('g'));
        o.key(Key::Enter);
        o.key(Key::Enter);
        o.key(Key::Focus(true));

        f.git(&["mv", "a.txt", "folder/moved.txt"]);
        fs::remove_file(f.root.join("b.txt")).unwrap();
        assert!(o.check());
        let closed = draw(&mut o, 80, 24).text();
        assert!(closed.iter().any(|row| row.contains("▸ folder 1")), "{closed:?}");
        assert!(file_row(&closed, "b.txt").contains("removed"));
        assert!(!closed.iter().any(|row| row.contains("a.txt") || row.contains("moved.txt")));
        o.key(Key::Char('n'));
        assert_eq!(o.selected_path().as_deref(), Some("folder/moved.txt"));
        let opened = draw(&mut o, 80, 24).text();
        let moved = file_row(&opened, "moved.txt");
        let removed = file_row(&opened, "b.txt");
        let col = |row: &str, label| crate::width::str_width(row.split_once(label).unwrap().0);
        assert_eq!(col(moved, "moved "), col(removed, "removed"));
        o.key(Key::Char('n'));
        assert_eq!(o.selected_path().as_deref(), Some("b.txt"));
    }

    #[test]
    fn a_new_file_can_be_read_when_it_replaces_a_folder() {
        let f = Fixture::new();
        f.write("shape/old.txt", "old contents\n");
        f.commit("first");
        let mut o = open(&f);
        o.key(Key::Focus(true));
        fs::remove_dir_all(f.root.join("shape")).unwrap();
        f.write("shape", "new contents\n");
        assert!(o.check());
        let rows = draw(&mut o, 80, 24).text();
        assert!(rows.iter().any(|row| row.contains("▾ shape")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("old.txt") && row.contains("removed")), "{rows:?}");

        o.key(Key::Char('n'));
        o.key(Key::Char('n'));
        o.key(Key::Enter);
        let reader = draw(&mut o, 80, 24).text();
        assert!(reader.iter().any(|row| row.contains("new contents")), "{reader:?}");
    }

    #[test]
    fn a_live_change_keeps_the_file_visible_in_a_one_row_strip() {
        let f = Fixture::new();
        let mut o = open(&f);
        o.key(Key::Focus(false));
        f.write("new.txt", "a new file\n");
        assert!(o.check());
        let rows = draw(&mut o, 80, 1).text();
        assert_eq!(rows, [" • new.txt"]);
    }

    #[test]
    fn a_tree_note_does_not_cover_the_cursor() {
        let f = Fixture::folder();
        for name in ["a.txt", "b.txt", "c.txt", "last.txt"] {
            f.write(name, "a file\n");
        }
        let mut o = open(&f);
        o.key(Key::End);
        o.say("Cannot open this file");
        let at = o.note.as_ref().unwrap().1;
        let frame = draw_at(&mut o, 80, 5, at);
        let rows = frame.text();
        assert_eq!(rows[4], " Cannot open this file");
        assert!(rows[3].contains("last.txt") && frame.bar_at(3), "{rows:?}");
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
    fn a_giant_flat_folder_is_bounded_and_marks_the_partial_tree() {
        let f = Fixture::folder();
        for i in 0..walk::CAP {
            fs::File::create(f.root.join(format!("entry-{i:05}.txt"))).unwrap();
        }
        f.write(".DS_Store", "");
        let mut o = open(&f);
        assert_eq!(o.listing.paths.len(), walk::CAP);
        assert!(!o.listing.truncated, "exactly the limit is still a complete listing");

        f.write("one-more.txt", "");
        assert!(o.check());
        assert_eq!(o.listing.paths.len(), walk::CAP);
        assert!(o.listing.truncated);
        let views = [(60, 8, "Showing a partial listing"), (20, 2, "Partial"), (37, 1, "Partial listing")];
        for (width, height, notice) in views {
            let rows = draw(&mut o, width, height).text();
            assert!(rows[0].contains(notice), "{rows:?}");
            assert!(rows.iter().any(|row| row.contains("entry-")), "{rows:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unrepresentable_git_paths_are_not_reported_as_clean() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let f = Fixture::new();
        fs::write(f.root.join(OsStr::from_bytes(b"invalid-\xff")), "unrepresentable\n").unwrap();
        let mut o = open(&f);
        let views = [(3, 1, "!"), (40, 1, "partial listing"), (40, 2, "partial listing"), (70, 8, "partial listing")];
        for (width, height, notice) in views {
            let rows = draw(&mut o, width, height).text();
            assert!(rows[0].contains(notice), "{rows:?}");
            assert!(rows.iter().all(|r| !r.contains("committed") && !r.contains("No files")), "{rows:?}");
            o.key(Key::End);
        }

        f.write("known.txt", "representable\n");
        o.key(Key::Char('r'));
        let rows = draw(&mut o, 80, 8).text();
        assert!(rows[0].contains("1 to commit") && rows[0].contains("partial listing"), "{rows:?}");
        assert!(rows[1].contains("known.txt"), "{rows:?}");
    }

    #[test]
    fn a_plain_folder_outline_promotes_on_explicit_refresh_even_with_a_gitfile() {
        let f = Fixture::folder();
        f.write("page.txt", "before\n");
        let mut outline = open(&f);
        let git_dir = f.root.with_file_name("metadata");
        f.git(&["init", "-q", "--template=", "-b", "main", "--separate-git-dir", git_dir.to_str().unwrap()]);
        f.commit("first");
        f.write("page.txt", "after\n");

        assert_eq!(outline.key(Key::Char('r')), After::Stay);
        assert!(draw(&mut outline, 60, 8).text()[0].ends_with("1 to commit"));
        assert_eq!(outline.watch_scope().roots, [f.root.clone(), git_dir]);
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

    #[test]
    fn a_git_error_is_not_reported_as_clean_in_a_one_row_strip() {
        let f = project();
        let mut o = open(&f);
        f.write(".git/index", "corrupt index");
        assert!(o.check());
        assert_eq!(draw(&mut o, 80, 1).text(), [" git did not answer"]);
    }

    #[test]
    fn the_strip_overflow_arrow_leaves_the_last_filename_readable() {
        let f = Fixture::new();
        for path in ["a-long.txt", "b-long.txt", "c-long.txt"] {
            f.write(path, "pending\n");
        }
        let mut o = open(&f);
        let rows = draw(&mut o, 22, 1).text();
        assert!(rows[0].contains("-long.txt") && rows[0].ends_with('›'), "{rows:?}");
    }

    #[test]
    fn a_ten_column_pane_can_still_show_a_short_filename() {
        let f = Fixture::folder();
        f.write("猫.txt", "a file\n");
        let mut o = open(&f);
        let rows = draw(&mut o, 10, 20).text();
        assert!(rows.iter().any(|row| row.contains("猫.txt")), "{rows:?}");
    }

    #[test]
    fn a_folder_that_cannot_be_read_is_not_reported_as_empty() {
        let f = Fixture::folder();
        f.write("page.txt", "a file\n");
        let mut o = open(&f);
        let away = f.root.with_file_name("moved");
        fs::rename(&f.root, &away).unwrap();
        assert!(o.check());
        let rows = draw(&mut o, 40, 8).text();
        assert_eq!(rows[1], " Cannot read this folder.");

        fs::rename(away, &f.root).unwrap();
        assert!(o.check());
        assert!(draw(&mut o, 40, 8).text()[1].contains("page.txt"));
    }

    #[test]
    fn an_incomplete_plain_listing_warns_even_in_a_short_pane() {
        let f = Fixture::folder();
        f.write("page.txt", "a file\n");
        let mut o = open(&f);
        o.listing.unreadable = true;
        let rows = draw(&mut o, 40, 1).text();
        assert!(rows[0].contains("page.txt") && rows[0].contains("partial listing"), "{rows:?}");
        let rows = draw(&mut o, 24, 2).text();
        assert!(rows[0].contains("Cannot list all files") && rows[1].contains("page.txt"), "{rows:?}");
    }

    #[test]
    fn a_narrow_tree_shows_the_whole_closed_folder_count() {
        let f = Fixture::new();
        for i in 0..16 {
            f.write(&format!("abcdefghijklmnopqrstuv/file-{i}.txt"), "changed\n");
        }
        let mut o = open(&f);
        o.key(Key::Char('p'));
        o.key(Key::Char(' '));
        let rows = draw(&mut o, 28, 20).text();
        assert!(rows[1].ends_with("… 16"), "{rows:?}");
    }

    #[test]
    fn a_mark_seen_in_the_outline_fades_after_focus_leaves() {
        let f = project();
        let mut o = open(&f);
        let now = Instant::now();
        o.attention.focus(true, now - Duration::from_secs(1));
        o.mark_files(&[], now);
        let rows = draw_at(&mut o, 65, 12, now).text();
        assert!(!file_row(&rows, "main.rs").starts_with(' '), "{rows:?}");

        o.attention.focus(false, now + Duration::from_secs(1));
        let rows = draw_at(&mut o, 65, 12, now + Duration::from_secs(4)).text();
        assert!(file_row(&rows, "main.rs").starts_with(' '), "{rows:?}");
    }

    #[test]
    fn an_initial_git_error_keeps_the_outline_usable_until_git_answers() {
        let f = project();
        let index = f.root.join(".git/index");
        let saved = fs::read(&index).unwrap();
        f.write(".git/index", "corrupt index");
        let mut o = open(&f);
        let rows = draw(&mut o, 80, 24).text();
        assert!(rows[0].ends_with("git did not answer"), "{rows:?}");
        assert!(rows[1].is_empty(), "{rows:?}");
        assert!(draw(&mut o, 10, 20).text()[0].contains("! git"));

        for key in ['c', 'e'] {
            assert_eq!(o.key(Key::Char(key)), After::Stay);
            assert!(draw(&mut o, 80, 24).text()[0].ends_with("git did not answer"));
        }

        fs::write(index, saved).unwrap();
        assert!(o.check());
        assert!(draw(&mut o, 80, 24).text()[0].ends_with("2 to commit"));
    }

    #[test]
    fn clicks_on_a_note_cannot_open_a_hidden_file() {
        let f = Fixture::folder();
        f.write("page.txt", "a file\n");
        let mut o = open(&f);
        o.say("Cannot open this file");
        let at = o.note.as_ref().unwrap().1;
        draw_at(&mut o, 40, 1, at);
        o.outline_key(Key::Click(3, 0), at);
        o.outline_key(Key::Click(3, 0), at);
        assert!(o.reading.is_none());
        assert_eq!(draw_at(&mut o, 40, 1, at).text(), [" Cannot open this file"]);
    }
}
