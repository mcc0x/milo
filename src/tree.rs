//! The whole project as a tree, coloured by what a change touched.

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

#[derive(Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    index: HashMap<String, u32>,
    /// Holds paths that are not in the working tree (files a commit deleted).
    pub ghosts: bool,
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
        for path in paths {
            tree.insert(path, false);
        }
        for i in 0..tree.nodes.len() {
            let mut children = std::mem::take(&mut tree.nodes[i].children);
            children.sort_unstable_by(|&a, &b| {
                let (a, b) = (&tree.nodes[a as usize], &tree.nodes[b as usize]);
                b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name))
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

    pub fn contains(&self, path: &str) -> bool {
        self.index.contains_key(path)
    }

    /// Colours the changed files and lets each directory take the strongest
    /// kind beneath it.
    pub fn colour<'a>(&mut self, changed: impl Iterator<Item = (&'a str, Kind)>) {
        for n in &mut self.nodes {
            n.kind = Kind::None;
        }
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

    /// Recomputes the visible rows; `open` says which directories are expanded.
    pub fn layout(&mut self, open: impl Fn(&str) -> bool) {
        self.rows.clear();
        let mut stack: Vec<(u32, u16)> = self.nodes[0].children.iter().rev().map(|&c| (c, 0)).collect();
        while let Some((i, depth)) = stack.pop() {
            let n = &self.nodes[i as usize];
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
}
