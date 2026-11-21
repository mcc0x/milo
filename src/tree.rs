//! Builds the file tree, counts pending files and lays out rows using folder folds.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::NonZeroU32;
use std::rc::Rc;

use crate::git::{Kind, PendingFile};
use crate::keys::Direction;

pub struct Node {
    name_at: usize,
    /// A folder ends in `/`, so it can coexist with a file of the same name.
    pub path: Rc<str>,
    parent: u32,
    /// Last row assigned here; it can go stale when a fold or filter hides this node.
    visible_at: usize,
    /// Number of pending files before this node in tree order.
    pending_before: u32,
    children: Vec<u32>,
    pub dir: bool,
    /// What happened here; for a directory, the strongest kind beneath it.
    pub kind: Kind,
    /// Lines gained and lost, summed over a directory.
    pub add: u64,
    pub del: u64,
    /// New files whose additions have yet to be measured.
    pub(crate) uncounted: u32,
    untracked: Option<NonZeroU32>,
    /// Pending files beneath a directory.
    pub pending: u32,
    /// This file or a file beneath this folder is uncommitted.
    pub uncommitted: bool,
    /// Keeps a path with multiple pending entries from counting twice.
    counted: bool,
}

impl Node {
    fn new(path: Rc<str>, name_at: usize, parent: u32, dir: bool) -> Node {
        Node {
            name_at,
            path,
            parent,
            visible_at: 0,
            pending_before: 0,
            children: Vec::new(),
            dir,
            kind: Kind::None,
            add: 0,
            del: 0,
            uncounted: 0,
            untracked: None,
            pending: 0,
            uncommitted: false,
            counted: false,
        }
    }

    /// The last part of the path, as it appears in the tree.
    pub fn name(&self) -> &str {
        self.path[self.name_at..].trim_end_matches('/')
    }

    pub(crate) fn untracked_index(&self) -> Option<usize> {
        self.untracked.map(|i| (i.get() - 1) as usize)
    }

    fn is_pending_file(&self) -> bool {
        !self.dir && self.kind != Kind::None
    }
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let first = digits.iter().position(|&c| c != b'0').unwrap_or(digits.len());
    &digits[first..]
}

/// Orders names the way a person would: without regard to case, and with
/// runs of digits compared as numbers, so `file2` comes before `file10`.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < x.len() && j < y.len() {
        if x[i].is_ascii_digit() && y[j].is_ascii_digit() {
            let run = |s: &[u8], from: usize| from + s[from..].iter().take_while(|c| c.is_ascii_digit()).count();
            let (ie, je) = (run(x, i), run(y, j));
            // Without leading zeros, the longer run is the larger number.
            let (n, m) = (trim_zeros(&x[i..ie]), trim_zeros(&y[j..je]));
            match n.len().cmp(&m.len()).then_with(|| n.cmp(m)) {
                Ordering::Equal => (i, j) = (ie, je),
                other => return other,
            }
        } else {
            match x[i].to_ascii_lowercase().cmp(&y[j].to_ascii_lowercase()) {
                Ordering::Equal => (i, j) = (i + 1, j + 1),
                other => return other,
            }
        }
    }
    // Equal apart from case or leading zeros: fall back to a stable total order.
    (x.len() - i).cmp(&(y.len() - j)).then_with(|| a.cmp(b))
}

/// Folders containing `path`, closest first, including the final `/` of each.
pub fn folders_above(path: &str) -> impl Iterator<Item = &str> {
    let mut path = path.strip_suffix('/').unwrap_or(path);
    std::iter::from_fn(move || {
        let slash = path.rfind('/')?;
        let folder = &path[..=slash];
        path = &path[..slash];
        Some(folder)
    })
}

struct FoldChoice {
    open: bool,
    // Navigation and unsaved changes can override this for the current visit.
    remembered: Option<bool>,
}

/// Choices for folder paths (ending in `/`) override the default.
#[derive(Default)]
pub struct Folds {
    choice: HashMap<String, FoldChoice>,
    /// Automatic suggestions last for this visit, but are not remembered.
    suggested: HashSet<String>,
    /// The choices to restore when the default goes back to closed.
    before_all: Option<HashMap<String, FoldChoice>>,
}

impl Folds {
    pub fn remembered(choice: BTreeMap<String, bool>) -> Self {
        let choice = choice.into_iter().map(|(path, open)| (path, FoldChoice { open, remembered: Some(open) }));
        Self { choice: choice.collect(), ..Self::default() }
    }

    pub fn is_open(&self, path: &str) -> bool {
        self.choice.get(path).map_or_else(|| self.all() || self.suggested.contains(path), |choice| choice.open)
    }

    pub fn set(&mut self, path: &str, open: bool, remembered: bool) {
        if let Some(choice) = self.choice.get_mut(path) {
            choice.open = open;
            if remembered {
                choice.remembered = Some(open);
            }
        } else {
            self.choice.insert(path.to_owned(), FoldChoice { open, remembered: remembered.then_some(open) });
        }
    }

    /// "Open all" is temporary and cannot overwrite the choices it will restore.
    pub fn remember(&self, path: &str) -> bool {
        !self.before_all.as_ref().is_some_and(|before| before.contains_key(path))
    }

    /// Forget transient folds for missing folders, not choices saved for the worktree.
    pub fn prune(&mut self, exists: impl Fn(&str) -> bool) {
        for choices in std::iter::once(&mut self.choice).chain(self.before_all.iter_mut()) {
            choices.retain(|path, choice| {
                if choice.remembered == Some(choice.open) || exists(path) {
                    true
                } else if let Some(open) = choice.remembered {
                    choice.open = open;
                    true
                } else {
                    false
                }
            });
        }
        self.suggested.retain(|path| exists(path));
    }

    /// Opens a folder unless the user has already decided about it.
    pub fn suggest(&mut self, path: &str) {
        if !self.suggested.contains(path) {
            self.suggested.insert(path.to_owned());
        }
    }

    /// Opens every folder above `path`, returning whether any were closed.
    pub fn open_above(&mut self, path: &str) -> bool {
        let mut opened = false;
        for dir in folders_above(path) {
            opened |= !self.is_open(dir);
            self.set(dir, true, false);
        }
        opened
    }

    pub fn all(&self) -> bool {
        self.before_all.is_some()
    }

    /// Opens every folder, or goes back to how things were before that.
    pub fn toggle_all(&mut self) {
        if let Some(mut choice) = self.before_all.take() {
            // Earlier choices win; keep decisions first made while everything was open.
            for (path, open) in self.choice.drain() {
                choice.entry(path).or_insert(open);
            }
            self.choice = choice;
        } else {
            self.before_all = Some(std::mem::take(&mut self.choice));
        }
    }
}

/// A visible node and how many folders it is drawn beneath.
#[derive(Clone, Copy)]
pub struct TreeRow {
    pub node: u32,
    pub depth: u32,
}

#[derive(Default)]
pub struct Tree {
    nodes: Vec<Node>,
    index: HashMap<Rc<str>, u32>,
    pending_order: Vec<u32>,
    /// One child cursor per ancestor, so wide trees need no second row buffer.
    layout_stack: Vec<(u32, usize)>,
    pub rows: Vec<TreeRow>,
}

impl Tree {
    /// Builds the tree from paths relative to the root; a trailing `/` marks a
    /// folder. Reuses the tree's storage and counts pending files on their ancestors.
    ///
    /// `files` lists uncommitted entries before unpushed entries.
    pub fn rebuild<'a>(&mut self, paths: impl Iterator<Item = &'a str>, files: &[PendingFile]) {
        self.nodes.clear();
        self.index.clear();
        self.rows.clear();
        self.pending_order.clear();
        self.nodes.push(Node::new(Rc::from(""), 0, 0, true));
        for path in paths {
            self.insert(path);
        }
        for (i, file) in files.iter().enumerate() {
            self.count(i, file);
        }
        for i in 0..self.nodes.len() {
            let mut children = std::mem::take(&mut self.nodes[i].children);
            children.sort_unstable_by(|&a, &b| {
                let (a, b) = (self.node(a), self.node(b));
                b.dir.cmp(&a.dir).then_with(|| natural_cmp(a.name(), b.name()))
            });
            self.nodes[i].children = children;
        }
        if !files.is_empty() {
            self.index_pending(0);
        }
    }

    fn index_pending(&mut self, id: u32) {
        let before = u32::try_from(self.pending_order.len()).expect("at most one pending file per node");
        let node = self.node_mut(id);
        node.pending_before = before;
        let children = node.children.len();
        if node.is_pending_file() {
            self.pending_order.push(id);
        }
        for i in 0..children {
            self.index_pending(self.node(id).children[i]);
        }
    }

    fn insert(&mut self, path: &str) -> u32 {
        if let Some(&i) = self.index.get(path) {
            return i;
        }
        let name_path = path.trim_end_matches('/');
        let dir = name_path.len() != path.len();
        let name_at = name_path.rfind('/').map_or(0, |slash| slash + 1);
        let parent = if name_at == 0 { 0 } else { self.insert(&path[..name_at]) };
        let i = u32::try_from(self.nodes.len()).expect("too many nodes in the project tree");
        let path: Rc<str> = Rc::from(path);
        self.nodes.push(Node::new(Rc::clone(&path), name_at, parent, dir));
        self.node_mut(parent).children.push(i);
        self.index.insert(path, i);
        i
    }

    /// Records one pending file on its node and on every directory above it.
    /// Use the uncommitted entry when the same path is also unpushed.
    fn count(&mut self, index: usize, file: &PendingFile) {
        let mut i = self.insert(&file.path);
        let first = !std::mem::replace(&mut self.node_mut(i).counted, true);
        if first && file.kind == Kind::Untracked {
            let index = u32::try_from(index + 1).expect("at most one untracked entry per node");
            self.node_mut(i).untracked = NonZeroU32::new(index);
        }
        loop {
            let n = self.node_mut(i);
            n.uncommitted |= !file.unpushed;
            if first {
                if n.kind < file.kind {
                    n.kind = file.kind;
                }
                n.add += u64::from(file.add);
                n.del += u64::from(file.del);
                n.uncounted += u32::from(file.lines_pending);
                n.pending += 1;
            }
            if i == 0 {
                break;
            }
            i = n.parent;
        }
    }

    /// Updates one count without rebuilding the layout or resumming other children.
    pub(crate) fn update_untracked(
        &mut self,
        index: usize,
        file: &PendingFile,
        before: (u32, bool),
        mut update: impl FnMut(&Node),
    ) {
        let Some(&id) = self.index.get(file.path.as_str()) else { return };
        // Only the first pending entry for this path contributed to these totals.
        if self.node(id).untracked_index() != Some(index) {
            return;
        }
        let (old_add, old_uncounted) = (u64::from(before.0), before.1);
        let add = u64::from(file.add);
        let mut i = id;
        loop {
            let n = self.node_mut(i);
            n.add = n.add - old_add + add;
            n.uncounted = n.uncounted - u32::from(old_uncounted) + u32::from(file.lines_pending);
            if i == 0 {
                break;
            }
            update(n);
            i = n.parent;
        }
    }

    /// Recomputes the visible rows. `open` says which directories are
    /// expanded; `pending_only` drops everything that is not pending.
    pub fn layout(&mut self, open: impl Fn(&str) -> bool, pending_only: bool) {
        self.rows.clear();
        if self.nodes.first().is_none_or(|root| root.children.is_empty()) {
            return;
        }
        let stack = &mut self.layout_stack;
        stack.push((0, 0));
        while let Some((parent, next)) = stack.last_mut() {
            let Some(&node) = self.nodes[*parent as usize].children.get(*next) else {
                stack.pop();
                continue;
            };
            *next += 1;
            let n = &mut self.nodes[node as usize];
            if pending_only && n.kind == Kind::None {
                continue;
            }
            let depth = u32::try_from(stack.len() - 1).expect("at most one ancestor per node");
            n.visible_at = self.rows.len();
            self.rows.push(TreeRow { node, depth });
            if n.dir && !n.children.is_empty() && open(&n.path) {
                stack.push((node, 0));
            }
        }
    }

    pub fn node(&self, id: u32) -> &Node {
        &self.nodes[id as usize]
    }

    fn node_mut(&mut self, id: u32) -> &mut Node {
        &mut self.nodes[id as usize]
    }

    pub fn row(&self, i: usize) -> Option<&Node> {
        self.rows.get(i).map(|r| self.node(r.node))
    }

    pub fn row_of(&self, path: &str) -> Option<usize> {
        self.row_of_node(*self.index.get(path)?)
    }

    /// Files in the same visibility scope as the tree, irrespective of folds.
    pub(crate) fn files(&self, pending_only: bool) -> impl Iterator<Item = &Rc<str>> {
        self.nodes.iter().filter(move |n| !n.dir && (!pending_only || n.is_pending_file())).map(|n| &n.path)
    }

    fn row_of_node(&self, id: u32) -> Option<usize> {
        let at = self.node(id).visible_at;
        self.rows.get(at).filter(|row| row.node == id).map(|_| at)
    }

    /// The index contains folders even when hidden by a fold or change filter.
    pub fn has_folder(&self, path: &str) -> bool {
        self.index.get(path).is_some_and(|&id| self.node(id).dir)
    }

    /// The row of the folder that row `i` is in, if it is in one.
    pub fn parent_row(&self, i: usize) -> Option<usize> {
        self.row_of_node(self.row(i)?.parent)
    }

    /// The next or previous folder among the visible rows.
    pub fn folder_row(&self, from: usize, direction: Direction) -> Option<usize> {
        let is_dir = |r: &TreeRow| self.node(r.node).dir;
        if direction == Direction::Next {
            self.rows.iter().skip(from + 1).position(is_dir).map(|i| from + 1 + i)
        } else {
            self.rows.iter().take(from).rposition(is_dir)
        }
    }

    /// The next or previous row at the same depth in the same folder.
    pub fn sibling_row(&self, from: usize, direction: Direction) -> Option<usize> {
        let depth = self.rows.get(from)?.depth;
        // The folder's rows end at the first one shallower than this.
        if direction == Direction::Next {
            let ahead = self.rows[from + 1..].iter().take_while(|r| r.depth >= depth);
            ahead.enumerate().find(|(_, r)| r.depth == depth).map(|(i, _)| from + 1 + i)
        } else {
            let behind = self.rows[..from].iter().rev().take_while(|r| r.depth >= depth);
            behind.enumerate().find(|(_, r)| r.depth == depth).map(|(i, _)| from - 1 - i)
        }
    }

    pub(crate) fn has_pending_files(&self) -> bool {
        !self.pending_order.is_empty()
    }

    /// The nearest pending file after (or before) row `from` in tree order,
    /// wrapping around, whether or not its folders are open.
    pub fn next_pending(&self, from: usize, direction: Direction) -> Option<&Rc<str>> {
        let len = self.pending_order.len();
        let current = self.row(from);
        let at = current.map_or(0, |node| node.pending_before as usize);
        let here = current.is_some_and(Node::is_pending_file);
        if len <= usize::from(here) {
            return None;
        }
        let next = match direction {
            Direction::Next => (at + usize::from(here)) % len,
            Direction::Previous if at == 0 => len - 1,
            Direction::Previous => at - 1,
        };
        Some(&self.node(self.pending_order[next]).path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, kind: Kind, add: u32, unpushed: bool) -> PendingFile {
        let mut file = PendingFile::new(path, kind);
        (file.add, file.unpushed, file.lines_pending) = (add, unpushed, false);
        file
    }

    fn names(tree: &Tree) -> Vec<&str> {
        tree.rows.iter().map(|r| tree.node(r.node).path.trim_end_matches('/')).collect()
    }

    impl Tree {
        fn build<'a>(paths: impl Iterator<Item = &'a str>, files: &[PendingFile]) -> Tree {
            let mut tree = Tree::default();
            tree.rebuild(paths, files);
            tree
        }
    }

    #[test]
    fn names_sort_as_a_person_would() {
        let mut names = ["File10.txt", "Zebra.md", "file2.txt", "apple.md", "a", "A"];
        names.sort_unstable_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["A", "a", "apple.md", "file2.txt", "File10.txt", "Zebra.md"]);
    }

    #[test]
    fn natural_order_is_total_and_consistent() {
        let names = ["a1", "a01", "a001", "A1", "a1b", "a10", "a2", "", "1", "01"];
        for a in names {
            for b in names {
                assert_eq!(natural_cmp(a, b), natural_cmp(b, a).reverse(), "{a:?} vs {b:?}");
                for c in names {
                    if natural_cmp(a, b).is_le() && natural_cmp(b, c).is_le() {
                        assert!(natural_cmp(a, c).is_le(), "{a:?} {b:?} {c:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn folders_come_first_and_sum_what_is_beneath() {
        let files = [file("src/b.rs", Kind::Modified, 3, false), file("src/sub/c.rs", Kind::Added, 2, true)];
        let mut tree = Tree::build(["z.txt", "src/a.rs", "src/b.rs"].into_iter(), &files);
        tree.layout(|_| true, false);
        assert_eq!(names(&tree), ["src", "src/sub", "src/sub/c.rs", "src/a.rs", "src/b.rs", "z.txt"]);
        let src = tree.row(0).unwrap();
        assert_eq!((src.kind, src.add, src.pending, src.uncommitted), (Kind::Modified, 5, 2, true));
        let sub = tree.row(1).unwrap();
        assert_eq!((sub.kind, sub.pending, sub.uncommitted), (Kind::Added, 1, false));
        assert_eq!(tree.row(2).unwrap().name(), "c.rs");
    }

    #[test]
    fn folder_totals_can_exceed_one_files_line_count() {
        let mut files =
            [file("src/a.rs", Kind::Modified, u32::MAX, false), file("src/new.rs", Kind::Untracked, 1, false)];
        files[0].del = u32::MAX;
        files[1].del = 2;
        let mut tree = Tree::build(std::iter::empty(), &files);
        let max = u64::from(u32::MAX);
        let totals = |tree: &Tree| {
            let src = tree.node(tree.index["src/"]);
            (src.add, src.del)
        };
        assert_eq!(totals(&tree), (max + 1, max + 2));

        files[1].add = 3;
        tree.update_untracked(1, &files[1], (1, false), |_| {});
        assert_eq!(totals(&tree), (max + 3, max + 2));
    }

    #[test]
    fn a_tracked_path_listed_twice_makes_one_node() {
        let mut tree = Tree::build(["café/猫.rs", "café/猫.rs"].into_iter(), &[]);
        tree.layout(|_| true, false);
        assert_eq!(names(&tree), ["café", "café/猫.rs"]);
        assert_eq!(tree.row(1).unwrap().name(), "猫.rs");
        assert_eq!(tree.nodes.len(), 3);
    }

    #[test]
    fn a_file_listed_twice_counts_once_as_first_seen() {
        let files = [file("a.rs", Kind::Modified, 1, false), file("a.rs", Kind::Added, 9, true)];
        let tree = Tree::build(["a.rs"].into_iter(), &files);
        let a = tree.node(1);
        assert_eq!((a.kind, a.add, a.uncommitted), (Kind::Modified, 1, true));
        assert_eq!(tree.node(0).pending, 1);
    }

    #[test]
    fn a_directory_replaced_by_a_file_still_shows_what_it_held() {
        // The order git reports them in: what was deleted, then what is new.
        let files = [file("dir/a.txt", Kind::Deleted, 0, false), file("dir", Kind::Untracked, 1, false)];
        let mut tree = Tree::build(["dir"].into_iter(), &files);
        assert_eq!((tree.node(0).pending, tree.node(0).add), (2, 1));
        tree.layout(|_| true, false);
        assert_eq!(names(&tree), ["dir", "dir/a.txt", "dir"]);
        assert_eq!(tree.row(2).unwrap().kind, Kind::Untracked);
    }

    #[test]
    fn pending_only_keeps_closed_folders_closed() {
        let files = [file("src/a.rs", Kind::Modified, 1, false)];
        let mut tree = Tree::build(["src/a.rs", "docs/x.md"].into_iter(), &files);
        tree.layout(|_| false, true);
        assert_eq!(names(&tree), ["src"]);
    }

    #[test]
    fn jumps_find_siblings_folders_and_pending_files() {
        let files = [file("b/y.rs", Kind::Modified, 1, false)];
        let mut tree = Tree::build(["a/x.rs", "b/y.rs", "c.txt"].into_iter(), &files);
        tree.layout(|p| p == "a/", false);
        assert_eq!(names(&tree), ["a", "a/x.rs", "b", "c.txt"]);
        assert_eq!(tree.sibling_row(0, Direction::Next), Some(2));
        assert_eq!(tree.sibling_row(2, Direction::Previous), Some(0));
        assert_eq!(tree.folder_row(0, Direction::Next), Some(2));
        assert_eq!(tree.parent_row(1), Some(0));
        assert_eq!(tree.parent_row(0), None);
        // The pending file sits in a closed folder, and is still found.
        assert_eq!(tree.next_pending(0, Direction::Next).map(Rc::as_ref), Some("b/y.rs"));
    }

    #[test]
    fn row_lookups_follow_folds_and_change_filters() {
        let files = [file("src/z.rs", Kind::Modified, 1, false)];
        let mut tree = Tree::build(["src/nested/b.rs", "src/a.rs", "src/z.rs"].into_iter(), &files);
        tree.layout(|_| true, false);
        assert_eq!(tree.row_of("src/nested/b.rs"), Some(2));
        assert_eq!(tree.parent_row(2), Some(1));
        assert_eq!(tree.row_of("src/z.rs"), Some(4));

        tree.layout(|dir| dir == "src/", false);
        assert_eq!(tree.row_of("src/a.rs"), Some(2));
        assert_eq!(tree.row_of("src/nested/b.rs"), None);
        assert_eq!(tree.parent_row(2), Some(0));

        tree.layout(|_| true, true);
        assert_eq!(tree.row_of("src/z.rs"), Some(1));
        assert_eq!(tree.row_of("src/nested/"), None);
        assert_eq!(tree.parent_row(1), Some(0));
        assert_eq!(tree.parent_row(0), None);
    }

    #[test]
    fn opening_everything_can_be_undone() {
        let mut folds = Folds::default();
        folds.set("src", false, false);
        folds.suggest("src");
        folds.suggest("docs");
        assert!(!folds.is_open("src") && folds.is_open("docs") && !folds.is_open("other"));
        folds.toggle_all();
        assert!(folds.is_open("src") && folds.is_open("other"));
        folds.toggle_all();
        assert!(!folds.is_open("src") && folds.is_open("docs") && !folds.is_open("other"));
    }

    #[test]
    fn absent_folders_lose_transient_folds_but_keep_remembered_choices() {
        let mut folds = Folds::remembered(BTreeMap::from([("src/".into(), false)]));
        folds.open_above("src/file");
        folds.set("docs/", true, true);
        folds.suggest("work/");
        folds.set("tmp/", true, false);
        assert!(folds.is_open("src/"));

        folds.toggle_all();
        folds.prune(|_| false);
        folds.toggle_all();
        folds.suggest("src/");
        assert!(!folds.is_open("src/"));
        assert!(folds.is_open("docs/"));
        assert!(!folds.is_open("work/"));
        assert!(!folds.is_open("tmp/"));
    }
}
