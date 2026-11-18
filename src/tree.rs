//! The whole project as a tree, each node knowing what is pending beneath it.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::git::{FileChange, Kind};

pub struct Node {
    pub name: String,
    pub path: String,
    pub parent: u32,
    pub children: Vec<u32>,
    pub dir: bool,
    /// What happened here; for a directory, the strongest kind beneath it.
    pub kind: Kind,
    /// Lines gained and lost, summed over a directory.
    pub add: u32,
    pub del: u32,
    /// Pending files beneath a directory.
    pub pending: u32,
    /// Holds work that is not committed yet.
    pub uncommitted: bool,
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

/// Which folders are open. A folder is open or closed because someone said
/// so; one nobody has spoken for follows `all`.
#[derive(Default)]
pub struct Folds {
    choice: HashMap<String, bool>,
    all: bool,
    /// The choices made before everything was opened, to go back to.
    before_all: HashMap<String, bool>,
}

impl Folds {
    pub fn is_open(&self, path: &str) -> bool {
        self.choice.get(path).copied().unwrap_or(self.all)
    }

    pub fn set(&mut self, path: &str, open: bool) {
        self.choice.insert(path.to_string(), open);
    }

    /// Opens a folder unless the reader has already decided about it.
    pub fn suggest(&mut self, path: &str) {
        if !self.choice.contains_key(path) {
            self.choice.insert(path.to_string(), true);
        }
    }

    /// Opens every folder above `path`, whatever was decided before.
    pub fn open_above(&mut self, path: &str) {
        let mut p = path;
        while let Some((dir, _)) = p.rsplit_once('/') {
            self.set(dir, true);
            p = dir;
        }
    }

    pub fn all(&self) -> bool {
        self.all
    }

    /// Opens every folder, or goes back to how things were before that.
    pub fn toggle_all(&mut self) {
        self.all = !self.all;
        if self.all {
            self.before_all = std::mem::take(&mut self.choice);
        } else {
            // What was decided before wins; anything first decided while
            // everything was open is kept.
            let mut choice = std::mem::take(&mut self.before_all);
            for (path, open) in self.choice.drain() {
                choice.entry(path).or_insert(open);
            }
            self.choice = choice;
        }
    }
}

#[derive(Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    index: HashMap<String, u32>,
    /// Visible rows as (node, depth).
    pub rows: Vec<(u32, u16)>,
}

impl Tree {
    /// Builds the tree of `paths`, where one ending in `/` is a directory in
    /// its own right, and marks what is pending from `files`.
    pub fn build<'a>(paths: impl Iterator<Item = &'a str>, files: &[FileChange]) -> Tree {
        let root = Node {
            name: String::new(),
            path: String::new(),
            parent: 0,
            children: Vec::new(),
            dir: true,
            kind: Kind::None,
            add: 0,
            del: 0,
            pending: 0,
            uncommitted: false,
        };
        let mut tree = Tree { nodes: vec![root], ..Tree::default() };
        for path in paths {
            match path.strip_suffix('/') {
                Some(dir) => tree.insert(dir, true),
                None => tree.insert(path, false),
            };
        }
        for file in files {
            tree.mark(file);
        }
        for i in 0..tree.nodes.len() {
            let mut children = std::mem::take(&mut tree.nodes[i].children);
            children.sort_unstable_by(|&a, &b| {
                let (a, b) = (&tree.nodes[a as usize], &tree.nodes[b as usize]);
                b.dir.cmp(&a.dir).then_with(|| natural_cmp(&a.name, &b.name))
            });
            tree.nodes[i].children = children;
        }
        tree
    }

    fn insert(&mut self, path: &str, dir: bool) -> u32 {
        if let Some(&i) = self.index.get(path) {
            // A directory that became a file is still where its old files hang.
            self.nodes[i as usize].dir |= dir;
            return i;
        }
        let (parent, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (self.insert(dir, true), name),
            None => (0, path),
        };
        let i = self.nodes.len() as u32;
        self.nodes.push(Node {
            name: name.to_string(),
            path: path.to_string(),
            parent,
            children: Vec::new(),
            dir,
            kind: Kind::None,
            add: 0,
            del: 0,
            pending: 0,
            uncommitted: false,
        });
        self.nodes[parent as usize].children.push(i);
        self.index.insert(path.to_string(), i);
        i
    }

    /// Records one pending file on its node and on every directory above it.
    /// A file listed twice, not committed and then committed, counts once,
    /// as it first appears.
    fn mark(&mut self, file: &FileChange) {
        let mut i = self.insert(&file.path, false) as usize;
        let first = self.nodes[i].kind == Kind::None;
        loop {
            let n = &mut self.nodes[i];
            n.uncommitted |= !file.committed;
            if first {
                if n.kind < file.kind {
                    n.kind = file.kind;
                }
                n.add += file.add;
                n.del += file.del;
                n.pending += 1;
            }
            if i == 0 {
                break;
            }
            i = n.parent as usize;
        }
    }

    /// Recomputes the visible rows. `open` says which directories are
    /// expanded; `changed_only` drops everything that is not pending.
    pub fn layout(&mut self, open: impl Fn(&str) -> bool, changed_only: bool) {
        self.rows.clear();
        let mut stack: Vec<(u32, u16)> = self.nodes[0].children.iter().rev().map(|&c| (c, 0)).collect();
        while let Some((i, depth)) = stack.pop() {
            let n = &self.nodes[i as usize];
            if changed_only && n.kind == Kind::None {
                continue;
            }
            self.rows.push((i, depth));
            if n.dir && open(&n.path) {
                stack.extend(n.children.iter().rev().map(|&c| (c, depth + 1)));
            }
        }
    }

    pub fn row(&self, i: usize) -> Option<&Node> {
        self.rows.get(i).map(|&(n, _)| &self.nodes[n as usize])
    }

    pub fn row_of(&self, path: &str) -> Option<usize> {
        let node = *self.index.get(path)?;
        self.rows.iter().position(|&(n, _)| n == node)
    }

    /// The row of the folder that row `i` is in, if it is in one.
    pub fn parent_row(&self, i: usize) -> Option<usize> {
        let parent = self.row(i)?.parent;
        (parent != 0).then(|| self.rows.iter().position(|&(n, _)| n == parent)).flatten()
    }

    /// The next or previous folder among the visible rows.
    pub fn folder_row(&self, from: usize, forward: bool) -> Option<usize> {
        let is_dir = |&(n, _): &(u32, u16)| self.nodes[n as usize].dir;
        if forward {
            self.rows.iter().skip(from + 1).position(is_dir).map(|i| from + 1 + i)
        } else {
            self.rows.iter().take(from).rposition(is_dir)
        }
    }

    /// The next or previous row at the same depth in the same folder.
    pub fn sibling_row(&self, from: usize, forward: bool) -> Option<usize> {
        let &(_, depth) = self.rows.get(from)?;
        // The folder's rows end at the first one shallower than this.
        if forward {
            let ahead = self.rows[from + 1..].iter().take_while(|r| r.1 >= depth);
            ahead.enumerate().find(|(_, r)| r.1 == depth).map(|(i, _)| from + 1 + i)
        } else {
            let behind = self.rows[..from].iter().rev().take_while(|r| r.1 >= depth);
            behind.enumerate().find(|(_, r)| r.1 == depth).map(|(i, _)| from - 1 - i)
        }
    }

    /// The nearest pending file after (or before) row `from` in tree order,
    /// wrapping around, whether or not its folders are open.
    pub fn next_pending(&self, from: usize, forward: bool) -> Option<&str> {
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<u32> = self.nodes[0].children.iter().rev().copied().collect();
        while let Some(i) = stack.pop() {
            order.push(i);
            stack.extend(self.nodes[i as usize].children.iter().rev());
        }
        let pending = |&&n: &&u32| !self.nodes[n as usize].dir && self.nodes[n as usize].kind != Kind::None;
        let at = self.rows.get(from).and_then(|&(n, _)| order.iter().position(|&x| x == n));
        let (before, after) = match at {
            Some(at) => (&order[..at], &order[at + 1..]),
            None => (&order[..0], &order[..]),
        };
        let hit = if forward {
            after.iter().chain(before).find(pending)
        } else {
            before.iter().rev().chain(after.iter().rev()).find(pending)
        };
        hit.map(|&n| self.nodes[n as usize].path.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, kind: Kind, add: u32, committed: bool) -> FileChange {
        FileChange { path: path.to_string(), from: None, kind, add, del: 0, committed, stamp: None }
    }

    fn names(tree: &Tree) -> Vec<&str> {
        tree.rows.iter().map(|&(n, _)| tree.nodes[n as usize].path.as_str()).collect()
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
    }

    #[test]
    fn a_file_listed_twice_counts_once_as_first_seen() {
        let files = [file("a.rs", Kind::Modified, 1, false), file("a.rs", Kind::Added, 9, true)];
        let tree = Tree::build(["a.rs"].into_iter(), &files);
        let a = &tree.nodes[1];
        assert_eq!((a.kind, a.add, a.uncommitted), (Kind::Modified, 1, true));
        assert_eq!(tree.nodes[0].pending, 1);
    }

    #[test]
    fn a_directory_replaced_by_a_file_still_shows_what_it_held() {
        let files = [file("dir", Kind::Untracked, 1, false), file("dir/a.txt", Kind::Deleted, 0, false)];
        let mut tree = Tree::build(["dir"].into_iter(), &files);
        tree.layout(|_| true, false);
        assert_eq!(names(&tree), ["dir", "dir/a.txt"]);
    }

    #[test]
    fn changed_only_keeps_closed_folders_closed() {
        let files = [file("src/a.rs", Kind::Modified, 1, false)];
        let mut tree = Tree::build(["src/a.rs", "docs/x.md"].into_iter(), &files);
        tree.layout(|_| false, true);
        assert_eq!(names(&tree), ["src"]);
    }

    #[test]
    fn jumps_find_siblings_folders_and_pending_files() {
        let files = [file("b/y.rs", Kind::Modified, 1, false)];
        let mut tree = Tree::build(["a/x.rs", "b/y.rs", "c.txt"].into_iter(), &files);
        tree.layout(|p| p == "a", false);
        assert_eq!(names(&tree), ["a", "a/x.rs", "b", "c.txt"]);
        assert_eq!(tree.sibling_row(0, true), Some(2));
        assert_eq!(tree.sibling_row(2, false), Some(0));
        assert_eq!(tree.folder_row(0, true), Some(2));
        assert_eq!(tree.parent_row(1), Some(0));
        assert_eq!(tree.parent_row(0), None);
        // The pending file sits in a closed folder, and is still found.
        assert_eq!(tree.next_pending(0, true), Some("b/y.rs"));
    }

    #[test]
    fn opening_everything_can_be_undone() {
        let mut folds = Folds::default();
        folds.set("src", false);
        folds.suggest("src");
        folds.suggest("docs");
        assert!(!folds.is_open("src") && folds.is_open("docs") && !folds.is_open("other"));
        folds.toggle_all();
        assert!(folds.is_open("src") && folds.is_open("other"));
        folds.toggle_all();
        assert!(!folds.is_open("src") && folds.is_open("docs") && !folds.is_open("other"));
    }
}
