//! The whole project as a tree, coloured by what is pending.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::git::Kind;

pub struct Node {
    pub name: String,
    pub path: String,
    pub parent: u32,
    pub children: Vec<u32>,
    pub dir: bool,
    pub kind: Kind,
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
            match n.len().cmp(&m.len()).then_with(|| n.cmp(&m)) {
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

#[derive(Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    index: HashMap<String, u32>,
    /// Visible rows as (node, depth).
    pub rows: Vec<(u32, u16)>,
}

impl Tree {
    pub fn build<'a>(paths: impl Iterator<Item = &'a str>) -> Tree {
        let root = Node {
            name: String::new(),
            path: String::new(),
            parent: 0,
            children: Vec::new(),
            dir: true,
            kind: Kind::None,
        };
        let mut tree = Tree { nodes: vec![root], ..Tree::default() };
        // A path ending in `/` is a directory in its own right, possibly empty.
        for path in paths {
            match path.strip_suffix('/') {
                Some(dir) => tree.insert(dir, true),
                None => tree.insert(path, false),
            };
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
        });
        self.nodes[parent as usize].children.push(i);
        self.index.insert(path.to_string(), i);
        i
    }

    /// Colours the changed files and lets each directory take the strongest
    /// kind beneath it. A path given twice keeps the later kind.
    pub fn colour<'a>(&mut self, changed: impl Iterator<Item = (&'a str, Kind)>) {
        for (path, kind) in changed {
            let Some(&i) = self.index.get(path) else { continue };
            let mut i = i as usize;
            self.nodes[i].kind = kind;
            while i != 0 {
                i = self.nodes[i].parent as usize;
                if self.nodes[i].kind >= kind {
                    break;
                }
                self.nodes[i].kind = kind;
            }
        }
    }

    /// Recomputes the visible rows. `open` says which directories are
    /// expanded; `changed_only` drops everything that is not pending and
    /// opens the rest.
    pub fn layout(&mut self, open: impl Fn(&str) -> bool, changed_only: bool) {
        self.rows.clear();
        let mut stack: Vec<(u32, u16)> = self.nodes[0].children.iter().rev().map(|&c| (c, 0)).collect();
        while let Some((i, depth)) = stack.pop() {
            let n = &self.nodes[i as usize];
            if changed_only && n.kind == Kind::None {
                continue;
            }
            self.rows.push((i, depth));
            if n.dir && (changed_only || open(&n.path)) {
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
}
