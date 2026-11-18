//! The project as a strip, coloured by what is not yet committed or pushed.
//!
//! A tall pane shows the file tree. A short one shows only the pending files,
//! flowed across its width. A wide one adds what happened to each file.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::git::{FileChange, Kind, Repo};
use crate::icons;
use crate::term::{self, Key, Screen, Style};
use crate::tree::Tree;
use crate::ui::{self, DIM};

/// At or below this height the pane is a horizontal strip.
const STRIP_ROWS: usize = 7;
/// From this width each file also says what happened to it.
const WIDE_COLS: usize = 44;

pub struct Outline {
    repo: Repo,
    /// Pending files; one both committed and edited again appears twice,
    /// the uncommitted state first.
    files: Vec<FileChange>,
    tree: Tree,
    /// Directories the user opened, and ones they closed again.
    expanded: HashSet<String>,
    collapsed: HashSet<String>,
    /// Directories holding something pending; open unless closed by hand.
    touched: HashSet<String>,
    /// Paths, and the directories above them, with work not committed yet.
    uncommitted: HashSet<String>,
    changed_only: bool,
    /// Every directory is open, until toggled off again.
    all_open: bool,
    /// Whether this pane has the keyboard, as far as the terminal says.
    focused: bool,
    sel: usize,
    top: usize,
    /// First column shown when the strip has more than fits.
    strip_col: usize,
    horizontal: bool,
    /// The rows below the heading: (first row, height).
    body: (usize, usize),
}

impl Outline {
    pub fn new(root: PathBuf) -> Outline {
        let mut outline = Outline {
            repo: Repo::open(root),
            files: Vec::new(),
            tree: Tree::default(),
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            touched: HashSet::new(),
            uncommitted: HashSet::new(),
            changed_only: false,
            all_open: false,
            focused: true,
            sel: 0,
            top: 0,
            strip_col: 0,
            horizontal: false,
            body: (0, 0),
        };
        outline.reload();
        outline
    }

    fn reload(&mut self) {
        let selected = self.selected_path();
        self.files = self.repo.load();
        let all = self.repo.files();
        let pending = self.files.iter().map(|f| f.path.as_str());
        self.tree = Tree::build(all.iter().map(String::as_str).chain(pending));
        // Reversed, so a file both committed and edited again shows its newest state.
        self.tree.colour(self.files.iter().rev().map(|f| (f.path.as_str(), f.kind)));
        self.touched.clear();
        self.uncommitted.clear();
        for f in &self.files {
            if !f.committed {
                self.uncommitted.insert(f.path.clone());
            }
            let mut p = f.path.as_str();
            while let Some((dir, _)) = p.rsplit_once('/') {
                // Stop climbing once this folder is known in every set it belongs to.
                let seen = !self.touched.insert(dir.to_string());
                let marked = f.committed || !self.uncommitted.insert(dir.to_string());
                if seen && marked {
                    break;
                }
                p = dir;
            }
        }
        self.relayout(selected);
    }

    /// Called when nothing has happened for a while.
    pub fn tick(&mut self) {
        if self.repo.stale(&self.files) {
            self.reload();
        }
    }

    fn selected_path(&self) -> Option<String> {
        self.tree.row(self.sel).map(|n| n.path.clone())
    }

    fn is_open(&self, path: &str) -> bool {
        !self.collapsed.contains(path)
            && (self.all_open || self.expanded.contains(path) || self.touched.contains(path))
    }

    /// Recomputes the visible rows, keeping the cursor on `selected`.
    fn relayout(&mut self, selected: Option<String>) {
        let (expanded, collapsed, touched, all) = (&self.expanded, &self.collapsed, &self.touched, self.all_open);
        let open = |p: &str| !collapsed.contains(p) && (all || expanded.contains(p) || touched.contains(p));
        self.tree.layout(open, self.changed_only);
        let at = selected.and_then(|p| self.tree.row_of(&p)).unwrap_or(self.sel);
        self.sel = at.min(self.tree.rows.len().saturating_sub(1));
    }

    fn set_open(&mut self, path: String, open: bool) {
        if open {
            self.collapsed.remove(&path);
            self.expanded.insert(path.clone());
        } else {
            self.expanded.remove(&path);
            self.collapsed.insert(path.clone());
        }
        self.relayout(Some(path));
    }

    /// Pending files, each once.
    fn pending(&self) -> Vec<&FileChange> {
        let mut seen = HashSet::new();
        self.files.iter().filter(|f| seen.insert(f.path.as_str())).collect()
    }

    pub fn draw(&mut self, s: &mut Screen) {
        s.clear();
        let (w, h) = (s.w, s.h);
        if w < 12 || h < 1 {
            return;
        }
        self.horizontal = h <= STRIP_ROWS;
        // A strip of two or three rows has none to spare for a heading.
        let head = (h >= 4) as usize;
        self.body = (head, h - head);
        if head == 1 {
            self.header(s);
        }
        if self.horizontal {
            self.draw_strip(s);
        } else {
            self.draw_tree(s);
        }
    }

    fn header(&self, s: &mut Screen) {
        let w = s.w;
        let summary = match self.pending().len() {
            0 => "nothing pending".to_string(),
            n => format!("{n} pending"),
        };
        let sw = if w >= 30 { summary.len() } else { 0 };
        s.put(w.saturating_sub(sw + 1), 0, sw, &summary, DIM);
        let room = w.saturating_sub(sw + 3);
        let x = 1 + s.put_fit(1, 0, room, &self.repo.name, Style::PLAIN.bold());
        s.put_fit(x + 2, 0, room.saturating_sub(x + 1), &self.repo.branch, Style::fg(term::MAGENTA));
    }

    fn draw_tree(&mut self, s: &mut Screen) {
        let (w, (y0, h)) = (s.w, self.body);
        ui::follow(self.sel, &mut self.top, h);
        self.top = self.top.min(self.tree.rows.len().saturating_sub(1));
        if self.tree.rows.is_empty() {
            let text = if self.changed_only { "Nothing pending." } else { "No files." };
            s.put_fit(1, y0, w - 1, text, DIM);
            return;
        }
        let rows = self.tree.rows.iter().enumerate().skip(self.top).take(h);
        for (y, (i, &(node, depth))) in (y0..).zip(rows) {
            let n = &self.tree.nodes[node as usize];
            let st = ui::kind_style(n.kind);
            let mut right = w - 1;
            if w >= WIDE_COLS && n.kind != Kind::None && !n.dir {
                if let Some(f) = self.files.iter().find(|f| f.path == n.path) {
                    right -= ui::size_right(s, right, y, f.add, f.del).max(9);
                    let word = ui::kind_word(n.kind);
                    right = right.saturating_sub(word.len() + 1);
                    s.put(right, y, word.len(), word, st);
                }
            }
            let mut x = 1 + (depth as usize * 2).min(w / 3);
            // Bold is work not committed yet; committed-but-unpushed stays regular.
            let st = if self.uncommitted.contains(&n.path) { st.bold() } else { st };
            let icon = if n.dir { icons::dir(self.is_open(&n.path)) } else { icons::file(&n.name) };
            x += ui::icon(s, x, y, right.saturating_sub(x + 1), icon);
            s.put_fit(x, y, right.saturating_sub(x + 1), &n.name, st);
            // Grey rather than a solid bar, so the row keeps its own colour;
            // and no bar at all while the keyboard is in another pane.
            if i == self.sel && self.focused {
                s.tint(0, y, w);
            }
        }
    }

    /// Pending files only, filling columns top to bottom like `ls`.
    fn draw_strip(&mut self, s: &mut Screen) {
        let (w, (y0, h)) = (s.w, self.body);
        let files = self.pending();
        if files.is_empty() {
            s.put_fit(1, y0, w - 1, "Nothing to commit or push.", DIM);
            return;
        }
        let name_w = |f: &&FileChange| term::str_width(f.path.rsplit_once('/').map_or(&f.path, |(_, n)| n));
        let col_w = (files.iter().map(name_w).max().unwrap_or(0) + 12).clamp(16, 34).min(w);
        let shown = (w / col_w).max(1);
        let total = files.len().div_ceil(h);
        let first = self.strip_col.min(total.saturating_sub(shown));
        for col in 0..shown {
            for row in 0..h {
                let Some(f) = files.get((first + col) * h + row) else { break };
                ui::file_name(s, col * col_w + 1, y0 + row, col_w - 2, f);
            }
        }
        if first > 0 {
            s.put(0, y0, 1, "‹", DIM);
        }
        if first + shown < total {
            s.put(w - 1, y0 + h - 1, 1, "›", DIM);
        }
        self.strip_col = first;
    }

    fn move_by(&mut self, delta: isize) {
        if self.horizontal {
            self.strip_col = self.strip_col.saturating_add_signed(delta.signum());
        } else {
            let last = self.tree.rows.len().saturating_sub(1);
            self.sel = self.sel.saturating_add_signed(delta).min(last);
        }
    }

    /// The row under the cursor, as (path, is a directory, is open, parent node).
    fn pointed(&self) -> Option<(String, bool, bool, usize)> {
        let n = self.tree.row(self.sel)?;
        Some((n.path.clone(), n.dir, self.is_open(&n.path), n.parent as usize))
    }

    /// Returns true when the program should quit.
    pub fn on_key(&mut self, key: Key) -> bool {
        let page = self.body.1.max(2) as isize - 1;
        match key {
            Key::Char('q') | Key::Ctrl(b'c') | Key::Eof => return true,
            Key::Focus(focused) => self.focused = focused,
            Key::Char('r') => self.reload(),
            Key::Char('e') => {
                // Opening everything also forgets what was closed by hand.
                self.all_open = !self.all_open;
                self.collapsed.clear();
                self.relayout(self.selected_path());
            }
            Key::Char('c') => {
                self.changed_only = !self.changed_only;
                self.relayout(self.selected_path());
            }
            Key::Char('j') | Key::Down => self.move_by(1),
            Key::Char('k') | Key::Up => self.move_by(-1),
            Key::WheelDown => self.move_by(3),
            Key::WheelUp => self.move_by(-3),
            Key::PgDn | Key::Ctrl(b'f') | Key::Ctrl(b'd') => self.move_by(page),
            Key::PgUp | Key::Ctrl(b'b') | Key::Ctrl(b'u') => self.move_by(-page),
            Key::Char('g') | Key::Home => self.move_by(isize::MIN),
            Key::Char('G') | Key::End => self.move_by(isize::MAX),
            Key::Char('l') | Key::Right if self.horizontal => self.move_by(1),
            Key::Char('h') | Key::Left if self.horizontal => self.move_by(-1),
            Key::Click(y) if !self.horizontal && y >= self.body.0 => {
                let row = self.top + y - self.body.0;
                if row < self.tree.rows.len() {
                    self.sel = row;
                    if let Some((path, true, open, _)) = self.pointed() {
                        self.set_open(path, !open);
                    }
                }
            }
            Key::Enter | Key::Char(' ') | Key::Char('l') | Key::Right | Key::Char('h') | Key::Left => {
                let Some((path, dir, open, parent)) = self.pointed() else { return false };
                let toggle = matches!(key, Key::Enter | Key::Char(' '));
                let right = matches!(key, Key::Char('l') | Key::Right);
                if dir && (toggle || open != right) {
                    self.set_open(path, !open);
                } else if !toggle && !right && parent != 0 {
                    // Left on a file or a closed directory steps out to its parent.
                    self.sel = self.tree.row_of(&self.tree.nodes[parent].path).unwrap_or(self.sel);
                }
            }
            _ => {}
        }
        false
    }
}
