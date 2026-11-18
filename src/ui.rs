//! Drawing.

use crate::app::{App, Pane, Rect, PANES};
use crate::git::{Commit, FileChange, Kind, LineKind, Row};
use crate::term::{self, Screen, Style};

const DIM: Style = Style::fg(term::GRAY);
const ACCENT: Style = Style::fg(term::CYAN).bold();
const ADD: Style = Style::fg(term::GREEN);
const DEL: Style = Style::fg(term::RED);

/// Below this size the panes are shown one at a time.
const WIDE: (usize, usize) = (72, 14);

const HELP: [&str; 13] = [
    "What changed, and when",
    "",
    "Move      ↑ ↓ or j k   ·   space / page keys   ·   g top   G bottom",
    "Open      enter goes deeper: timeline → files → change",
    "Back      esc",
    "Jump      [ ] previous / next file",
    "Search    / then type   ·   n N next / previous match",
    "Files     t whole project   ·   f only this file's history",
    "View      d git details   ·   a all branches",
    "Other     y copy   ·   r reload   ·   q quit",
    "Mouse     click, scroll, drag the dividers",
    "",
    "Press any key to close",
];

fn kind_style(kind: Kind) -> Style {
    match kind {
        Kind::None => Style::PLAIN,
        Kind::Modified => Style::fg(term::YELLOW),
        Kind::Added | Kind::Untracked => ADD,
        Kind::Deleted => DEL,
        Kind::Renamed => Style::fg(term::CYAN),
        Kind::Conflict => Style::fg(term::MAGENTA).bold(),
    }
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::None => "",
        Kind::Modified => "edited",
        Kind::Added | Kind::Untracked => "new",
        Kind::Deleted => "removed",
        Kind::Renamed => "moved",
        Kind::Conflict => "conflict",
    }
}

fn kind_letter(kind: Kind) -> &'static str {
    match kind {
        Kind::None => "",
        Kind::Modified => "M",
        Kind::Added => "A",
        Kind::Untracked => "U",
        Kind::Deleted => "D",
        Kind::Renamed => "R",
        Kind::Conflict => "!",
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Keeps `sel` inside a window of `h` rows starting at `top`.
fn follow(sel: usize, top: &mut usize, h: usize) {
    if sel < *top {
        *top = sel;
    } else if sel >= *top + h {
        *top = sel + 1 - h;
    }
}

fn title(s: &mut Screen, x: usize, y: usize, w: usize, text: &str, focused: bool) {
    s.fill(x, y, w, '─', DIM);
    let st = if focused { ACCENT } else { Style::PLAIN.bold() };
    s.put_fit(x + 1, y, w.saturating_sub(2), text, st);
}

/// Draws "+12 −3" ending just before column `end`. Returns its width.
fn size_right(s: &mut Screen, end: usize, y: usize, add: u32, del: u32) -> usize {
    let a = if add > 0 { format!("+{add}") } else { String::new() };
    let d = if del > 0 { format!("−{del}") } else { String::new() };
    let gap = (!a.is_empty() && !d.is_empty()) as usize;
    let w = a.len() + gap + term::str_width(&d);
    let x = end.saturating_sub(w);
    s.put(x, y, a.len(), &a, ADD);
    s.put(x + a.len() + gap, y, w, &d, DEL);
    w
}

impl App {
    pub fn draw(&mut self, s: &mut Screen) {
        s.clear();
        self.need_stats = false;
        self.areas = [Rect::default(); 3];
        self.dividers = (0, 0);
        let (w, h) = (s.w, s.h);
        if w < 30 || h < 6 {
            s.put(0, 0, w, "Too small to show.", Style::PLAIN);
            return;
        }
        self.header(s);
        if w >= WIDE.0 && h >= WIDE.1 {
            self.wide(s);
        } else {
            self.narrow(s);
        }
        self.footer(s);
        if self.help {
            self.help_box(s);
        }
    }

    /// Timeline over files on the left, the change on the right.
    fn wide(&mut self, s: &mut Screen) {
        let (w, h) = (s.w, s.h);
        let left_w = self.split_x.unwrap_or(w * 2 / 5).clamp(28, w - 36);
        let files_y = self.split_y.unwrap_or(2 + (h - 3) * 11 / 20).clamp(5, h - 5);
        let (rx, rw) = (left_w + 1, w - left_w - 1);
        self.dividers = (left_w, files_y);
        self.areas = [
            Rect { x: 0, y: 2, w: left_w, h: files_y - 2 },
            Rect { x: 0, y: files_y + 1, w: left_w, h: h - 2 - files_y },
            Rect { x: rx, y: 2, w: rw, h: h - 3 },
        ];
        title(s, 0, 1, left_w, &self.title_of(Pane::Timeline), self.focus == Pane::Timeline);
        title(s, 0, files_y, left_w, &self.title_of(Pane::Files), self.focus == Pane::Files);
        title(s, rx, 1, rw, &self.title_of(Pane::Change), self.focus == Pane::Change);
        self.change_position(s, w, 1);
        for y in 1..h - 1 {
            let ch = match y {
                1 => '┬',
                y if y == files_y => '┤',
                _ => '│',
            };
            s.fill(left_w, y, 1, ch, DIM);
        }
        self.draw_timeline(s);
        self.draw_files(s);
        self.draw_change(s);
    }

    /// One pane at a time, with the path through them as the heading.
    fn narrow(&mut self, s: &mut Screen) {
        let (w, h) = (s.w, s.h);
        s.fill(0, 1, w, '─', DIM);
        let mut x = 1 + s.put(1, 1, 1, " ", DIM);
        for (i, pane) in PANES.into_iter().enumerate() {
            if i > 0 {
                x += s.put(x, 1, w.saturating_sub(x), " › ", DIM);
            }
            let st = if pane == self.focus { ACCENT } else { DIM };
            let name = ["Timeline", "Files", "Change"][i];
            x += s.put(x, 1, w.saturating_sub(x), name, st);
        }
        s.put(x, 1, 1, " ", DIM);
        let i = PANES.iter().position(|&p| p == self.focus).unwrap_or(0);
        self.areas[i] = Rect { x: 0, y: 2, w, h: h - 3 };
        match self.focus {
            Pane::Timeline => self.draw_timeline(s),
            Pane::Files => self.draw_files(s),
            Pane::Change => {
                self.change_position(s, w, 1);
                self.draw_change(s);
            }
        }
    }

    fn title_of(&self, pane: Pane) -> String {
        match pane {
            Pane::Timeline if self.all => " Timeline · all branches ".into(),
            Pane::Timeline => " Timeline ".into(),
            Pane::Files if self.tree_mode => " Project ".into(),
            Pane::Files => format!(" Changed · {} ", plural(self.detail.files.len(), "file")),
            Pane::Change => match self.current_file().and_then(|i| self.detail.files.get(i)) {
                Some(f) => format!(" {} ", f.path),
                None => " Change ".into(),
            },
        }
    }

    /// How far through the change the view is, at the right of its heading.
    fn change_position(&self, s: &mut Screen, end: usize, y: usize) {
        let n = self.detail.lines.len();
        if n > self.areas[2].h {
            let text = format!(" {}% ", (self.change_top + self.areas[2].h).min(n) * 100 / n);
            s.put(end.saturating_sub(text.len() + 1), y, text.len(), &text, DIM);
        }
    }

    fn header(&self, s: &mut Screen) {
        let w = s.w;
        let help = "? help";
        s.put(w.saturating_sub(help.len() + 1), 0, help.len(), help, DIM);
        let room = w.saturating_sub(help.len() + 3);
        let mut x = 1;
        let mut part = |text: &str, st: Style| x += s.put_fit(x, 0, room.saturating_sub(x), text, st);
        part(&self.name, Style::PLAIN.bold());
        part("  ", DIM);
        part(&self.status.branch, Style::fg(term::MAGENTA));
        if self.status.ahead > 0 {
            part(&format!("  {} to push", self.status.ahead), DIM);
        }
        if self.status.behind > 0 {
            part(&format!("  {} to pull", self.status.behind), DIM);
        }
        if let Some(filter) = &self.filter {
            part(&format!("  only {filter}"), Style::fg(term::YELLOW));
            part("  (esc shows everything)", DIM);
        }
    }

    fn footer(&self, s: &mut Screen) {
        let (w, y) = (s.w, s.h - 1);
        if let Some(search) = &self.search {
            let x = 1 + s.put(1, y, w, "Search: ", ACCENT);
            let x = x + s.put_fit(x, y, w.saturating_sub(x + 2), &search.text, Style::PLAIN);
            s.put(x, y, 1, "▏", ACCENT);
            if !self.msg.is_empty() {
                s.put_fit(x + 3, y, w.saturating_sub(x + 4), &self.msg, DIM);
            }
            return;
        }
        if !self.msg.is_empty() {
            s.put_fit(1, y, w - 2, &self.msg, Style::fg(term::YELLOW));
            return;
        }
        let hint = match self.focus {
            Pane::Timeline if self.filter.is_some() => "↑↓ move   enter open   esc show everything   / search",
            Pane::Timeline => "↑↓ move   enter open   / search   d git details",
            Pane::Files if self.tree_mode => "↑↓ move   enter open   f this file's history   t changed only   esc back",
            Pane::Files => "↑↓ move   enter read   f this file's history   t whole project   esc back",
            Pane::Change => "↑↓ scroll   [ ] next file   / search   esc back",
        };
        s.put_fit(1, y, w - 2, hint, DIM);
    }

    fn help_box(&self, s: &mut Screen) {
        let inner = HELP.iter().map(|l| term::str_width(l)).max().unwrap_or(0);
        let (bw, bh) = ((inner + 4).min(s.w), (HELP.len() + 2).min(s.h));
        let (x, y) = ((s.w - bw) / 2, (s.h - bh) / 2);
        for row in 0..bh {
            s.fill(x, y + row, bw, ' ', Style::PLAIN);
            s.fill(x, y + row, 1, '│', DIM);
            s.fill(x + bw - 1, y + row, 1, '│', DIM);
        }
        for (row, (l, r)) in [(0, ('┌', '┐')), (bh - 1, ('└', '┘'))] {
            s.fill(x, y + row, bw, '─', DIM);
            s.fill(x, y + row, 1, l, DIM);
            s.fill(x + bw - 1, y + row, 1, r, DIM);
        }
        for (i, line) in HELP.iter().enumerate().take(bh.saturating_sub(2)) {
            let st = match i {
                0 => ACCENT,
                i if i == HELP.len() - 1 => DIM,
                _ => Style::PLAIN,
            };
            s.put_fit(x + 2, y + 1 + i, bw.saturating_sub(4), line, st);
        }
    }

    fn draw_timeline(&mut self, s: &mut Screen) {
        let area = self.areas[0];
        follow(self.sel, &mut self.top, area.h);
        // Keep a day heading in view above its first commit.
        if self.top > 0 && self.top == self.sel && matches!(self.rows[self.top - 1], Row::Day(_)) && area.h > 1 {
            self.top -= 1;
        }
        if self.rows.is_empty() {
            s.put(area.x + 1, area.y, area.w - 1, "Nothing here yet.", DIM);
            return;
        }
        let end = area.x + area.w;
        let rows = self.rows.iter().enumerate().skip(self.top).take(area.h);
        for (y, (i, row)) in (area.y..).zip(rows) {
            let x = area.x + 1;
            match row {
                Row::Graph(graph) => {
                    s.put(x, y, end - x, graph, Style::fg(term::BLUE));
                }
                Row::Day(label) => {
                    s.put(x, y, end - x, label, DIM);
                }
                Row::Uncommitted => {
                    let count = plural(self.status.files.len(), "file");
                    let cw = if self.filter.is_none() { count.len() } else { 0 };
                    s.put(end - 1 - cw, y, cw, &count, DIM);
                    let st = Style::fg(term::YELLOW).bold();
                    s.put_fit(x, y, (end - x).saturating_sub(cw + 2), "● Not committed yet", st);
                }
                Row::Commit(c) => {
                    self.need_stats |= c.stat.is_none();
                    if self.detailed {
                        commit_detailed(s, c, x, y, end);
                    } else {
                        commit_simple(s, c, x, y, end);
                    }
                }
            }
            if i == self.sel {
                s.select(area.x, y, area.w, self.focus == Pane::Timeline);
            }
        }
    }

    fn draw_files(&mut self, s: &mut Screen) {
        let area = self.areas[1];
        if self.tree_mode {
            return self.draw_tree(s, area);
        }
        follow(self.file_sel, &mut self.file_top, area.h);
        if self.detail.files.is_empty() {
            let text = if self.detail_dirty { "" } else { "No files changed." };
            s.put(area.x + 1, area.y, area.w - 1, text, DIM);
            return;
        }
        let end = area.x + area.w;
        let files = self.detail.files.iter().enumerate().skip(self.file_top).take(area.h);
        for (y, (i, f)) in (area.y..).zip(files) {
            // Right to left: size, then what happened; the name gets the rest.
            let mut right = end - 1;
            right -= size_right(s, right, y, f.add, f.del).max(if area.w >= 44 { 9 } else { 0 });
            let word = match (self.detailed, f.staged) {
                (false, _) => kind_word(f.kind).to_string(),
                (true, false) => kind_letter(f.kind).to_string(),
                (true, true) => format!("{} staged", kind_letter(f.kind)),
            };
            right = right.saturating_sub(word.len() + 1);
            s.put(right, y, word.len(), &word, kind_style(f.kind));

            let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
            let room = right.saturating_sub(area.x + 2);
            let x = area.x + 1 + s.put_fit(area.x + 1, y, room, name, kind_style(f.kind).bold());
            s.put_fit(x + 1, y, (area.x + 1 + room).saturating_sub(x + 1), dir, DIM);
            if i == self.file_sel {
                s.select(area.x, y, area.w, self.focus == Pane::Files);
            }
        }
    }

    fn draw_tree(&mut self, s: &mut Screen, area: Rect) {
        follow(self.tree_sel, &mut self.tree_top, area.h);
        self.tree_top = self.tree_top.min(self.tree.rows.len().saturating_sub(1));
        if self.tree.rows.is_empty() {
            s.put(area.x + 1, area.y, area.w - 1, "No files.", DIM);
            return;
        }
        let rows = self.tree.rows.iter().enumerate().skip(self.tree_top).take(area.h);
        for (y, (i, &(node, depth))) in (area.y..).zip(rows) {
            let n = &self.tree.nodes[node as usize];
            let mut st = kind_style(n.kind);
            if self.filter.as_deref() == Some(n.path.as_str()) {
                st = st.bold();
            }
            let mut x = area.x + (depth as usize * 2).min(area.w / 2);
            let name_w = (area.x + area.w).saturating_sub(x + 5);
            if n.dir {
                x += s.put(x, y, 2, if self.is_open(&n.path) { "▾ " } else { "▸ " }, DIM);
                x += s.put_fit(x, y, name_w, &n.name, st);
                s.put(x, y, 1, "/", st);
            } else {
                s.put_fit(x + 2, y, name_w, &n.name, st);
            }
            let badge = if n.dir && n.kind != Kind::None { "●" } else { kind_letter(n.kind) };
            s.put(area.x + area.w - 2, y, 1, badge, st);
            if i == self.tree_sel {
                s.select(area.x, y, area.w, self.focus == Pane::Files);
            }
        }
    }

    fn draw_change(&self, s: &mut Screen) {
        let area = self.areas[2];
        if self.detail.lines.is_empty() {
            let text = if self.detail_dirty { "Loading…" } else { "Nothing to show." };
            s.put(area.x + 2, area.y, area.w.saturating_sub(2), text, DIM);
            return;
        }
        let end = area.x + area.w;
        let digits = self.detail.max_no.max(1).ilog10() as usize + 1;
        // [space][line number][space][sign][space][text]
        let (no_x, text_x) = (area.x + 1, area.x + digits + 4);
        let text_w = end.saturating_sub(text_x);
        let lines = self.detail.lines.iter().skip(self.change_top).take(area.h);
        for (y, line) in (area.y..).zip(lines) {
            let prose = |s: &mut Screen, st| s.put_fit(area.x + 2, y, area.w.saturating_sub(3), &line.text, st);
            let (sign, st) = match line.kind {
                LineKind::Title => {
                    prose(s, Style::PLAIN.bold());
                    continue;
                }
                LineKind::Meta => {
                    prose(s, DIM);
                    continue;
                }
                LineKind::Info => {
                    prose(s, Style::PLAIN);
                    continue;
                }
                LineKind::Note => {
                    s.put_fit(text_x, y, text_w, &line.text, DIM);
                    continue;
                }
                LineKind::File(i) => {
                    if let Some(f) = self.detail.files.get(i as usize) {
                        self.file_heading(s, f, area, y);
                    }
                    continue;
                }
                LineKind::Hunk => {
                    let text = match line.text.as_str() {
                        "" => format!("⋯ line {}", line.no),
                        inside => format!("⋯ line {} · {inside}", line.no),
                    };
                    s.put_fit(text_x, y, text_w, &text, DIM);
                    continue;
                }
                LineKind::Context => (" ", Style::PLAIN),
                LineKind::Add => ("+", ADD),
                LineKind::Del => ("-", DEL),
            };
            let no = line.no.to_string();
            let no_st = if line.kind == LineKind::Context { DIM } else { st };
            s.put(no_x + digits - no.len().min(digits), y, digits, &no, no_st);
            s.put(text_x - 2, y, 1, sign, st);
            s.put_skip(text_x, y, text_w, &line.text, st, self.change_x);
            let (a, b) = (line.changed.0 as usize, line.changed.1 as usize);
            if a < b {
                let from = term::str_width(&line.text[..a]).saturating_sub(self.change_x);
                let to = term::str_width(&line.text[..b]).saturating_sub(self.change_x);
                s.emphasise(text_x + from, y, (to - from).min(text_w.saturating_sub(from)));
            }
        }
    }

    /// One bar per file: its path, what happened to it, and how much.
    fn file_heading(&self, s: &mut Screen, f: &FileChange, area: Rect, y: usize) {
        let end = area.x + area.w;
        let size_w = size_right(s, end - 1, y, f.add, f.del);
        let word = if self.detailed { kind_letter(f.kind) } else { kind_word(f.kind) };
        let room = (area.w - 2).saturating_sub(size_w + word.len() + 3);
        let x = area.x + 1 + s.put_fit(area.x + 1, y, room, &f.path, Style::PLAIN.bold());
        s.put(x + 2, y, word.len(), word, kind_style(f.kind));
        s.tint(area.x, y, area.w);
    }
}

/// "○ Subject  main                     +12 −3"
fn commit_simple(s: &mut Screen, c: &Commit, x: usize, y: usize, end: usize) {
    let size_w = c.stat.map_or(0, |st| size_right(s, end - 1, y, st.add, st.del));
    let room = (end - x).saturating_sub(size_w + 2);
    let mut x = x + s.put(x, y, room, "○ ", DIM);
    let refs_w = if c.refs.is_empty() { 0 } else { term::str_width(&c.refs) + 2 };
    // Branch names give way before the subject does.
    let refs_w = if refs_w * 2 > room { 0 } else { refs_w };
    x += s.put_fit(x, y, room.saturating_sub(2 + refs_w), &c.subject, Style::PLAIN);
    if refs_w > 0 {
        s.put(x + 2, y, refs_w, &c.refs, ADD);
    }
}

/// "* | a1b2c3d (HEAD -> main) Subject          Author · 2 hours ago"
fn commit_detailed(s: &mut Screen, c: &Commit, x: usize, y: usize, end: usize) {
    let mut x = x;
    let mut part = |s: &mut Screen, text: &str, st: Style| {
        x += s.put(x, y, end.saturating_sub(x + 1), text, st);
        x += 1;
        x
    };
    if !c.graph.is_empty() {
        part(s, &c.graph, Style::fg(term::BLUE));
    }
    part(s, c.hash.get(..7).unwrap_or(&c.hash), Style::fg(term::YELLOW));
    if !c.refs.is_empty() {
        part(s, &format!("({})", c.refs), ADD.bold());
    }
    let x = part(s, "", Style::PLAIN) - 1;
    let who = format!("{} · {}", c.author, c.when);
    let who_w = term::str_width(&who);
    let room = end.saturating_sub(x + 1);
    if room > who_w + 28 {
        s.put_fit(x, y, room - who_w - 2, &c.subject, Style::PLAIN);
        s.put(end - 1 - who_w, y, who_w, &who, DIM);
    } else {
        s.put_fit(x, y, room, &c.subject, Style::PLAIN);
    }
}
