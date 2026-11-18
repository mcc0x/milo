//! `strata history`: the files not yet committed or pushed on the left, how
//! they changed on the right.

use std::path::PathBuf;

use crate::git::{Detail, LineKind, Repo};
use crate::term::{self, Key, Screen, Style};
use crate::ui::{self, Rect, ACCENT, DIM};
use crate::View;

/// Below this size the two sides are shown one at a time.
const WIDE: (usize, usize) = (72, 10);

const HELP: [&str; 9] = [
    "What is waiting to be committed or pushed",
    "",
    "↑ ↓ or j k     pick a file, or scroll the change",
    "space, b       page the change down, up",
    "enter, esc     step into the change, and back out",
    "[ ]            previous, next file",
    "r              reload          q   quit",
    "",
    "Press any key to close",
];

enum Item {
    /// A heading; the index into `Detail::groups`.
    Group(usize),
    /// The index into `Detail::files`.
    File(usize),
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    List,
    Change,
}

pub struct History {
    repo: Repo,
    detail: Detail,
    items: Vec<Item>,
    sel: usize,
    list_top: usize,
    top: usize,
    pan: usize,
    focus: Side,
    help: bool,
    /// User-dragged divider column.
    split: Option<usize>,
    dragging: bool,
    /// Where the divider was last drawn; 0 when there is none.
    divider: usize,
    /// Screen areas of the list and the change, set by the last draw.
    areas: [Rect; 2],
}

impl History {
    pub fn new(root: PathBuf) -> History {
        let mut history = History {
            repo: Repo::open(root),
            detail: Detail::default(),
            items: Vec::new(),
            sel: 0,
            list_top: 0,
            top: 0,
            pan: 0,
            focus: Side::List,
            help: false,
            split: None,
            dragging: false,
            divider: 0,
            areas: [Rect::default(); 2],
        };
        history.reload();
        history
    }

    fn file_at(&self, item: usize) -> Option<usize> {
        match self.items.get(item) {
            Some(&Item::File(i)) => Some(i),
            _ => None,
        }
    }

    /// Loads afresh, keeping the cursor on the same file and the same place.
    fn reload(&mut self) {
        let keep = self.file_at(self.sel).map(|i| self.detail.files[i].path.clone());
        self.detail = self.repo.load(true);
        self.items.clear();
        for (g, group) in self.detail.groups.iter().enumerate() {
            self.items.push(Item::Group(g));
            self.items.extend(group.files.clone().map(Item::File));
        }
        let files = &self.detail.files;
        let first = self.items.iter().position(|it| matches!(it, Item::File(_)));
        let same = keep.and_then(|p| self.items.iter().position(|it| matches!(it, Item::File(i) if files[*i].path == p)));
        self.sel = same.or(first).unwrap_or(0);
        self.top = self.top.min(self.detail.lines.len().saturating_sub(1));
    }

    /// Selects a file and brings its section into view.
    fn select(&mut self, item: usize) {
        let Some(i) = self.file_at(item) else { return };
        self.sel = item;
        self.top = self.detail.files[i].line.unwrap_or(self.top);
    }

    /// Moves the cursor by `delta` files, stepping over headings.
    fn move_file(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        let (step, mut left) = (delta.signum(), delta.unsigned_abs().min(n as usize));
        let (mut i, mut best) = (self.sel as isize, self.sel);
        while left > 0 {
            i += step;
            if i < 0 || i >= n {
                break;
            }
            if self.file_at(i as usize).is_some() {
                best = i as usize;
                left -= 1;
            }
        }
        self.select(best);
    }

    /// The file whose section is at the top of the change view.
    fn current_file(&self) -> Option<usize> {
        let end = (self.top + 1).min(self.detail.lines.len());
        self.detail.lines[..end].iter().rev().find_map(|l| match l.kind {
            LineKind::File(i) => Some(i as usize),
            _ => None,
        })
    }

    fn scroll(&mut self, delta: isize) {
        let last = self.detail.lines.len().saturating_sub(1);
        self.top = self.top.saturating_add_signed(delta).min(last);
        let current = self.current_file();
        if let Some(item) = self.items.iter().position(|it| matches!(it, Item::File(i) if Some(*i) == current)) {
            self.sel = item;
        }
    }

    fn click(&mut self, x: usize, y: usize) {
        if self.divider != 0 && x == self.divider {
            self.dragging = true;
        } else if self.areas[0].contains(x, y) {
            self.focus = Side::List;
            self.select(self.list_top + y - self.areas[0].y);
        } else if self.areas[1].contains(x, y) {
            self.focus = Side::Change;
        }
    }

    fn header(&self, s: &mut Screen) {
        let w = s.w;
        let help = "? help";
        s.put(w.saturating_sub(help.len() + 1), 0, help.len(), help, DIM);
        let room = w.saturating_sub(help.len() + 3);
        let mut x = 1;
        let mut part = |text: &str, st: Style| x += s.put_fit(x, 0, room.saturating_sub(x), text, st);
        part(&self.repo.name, Style::PLAIN.bold());
        part("  ", DIM);
        part(&self.repo.status.branch, Style::fg(term::MAGENTA));
        let (ahead, behind) = (self.repo.status.ahead, self.repo.status.behind);
        if ahead > 0 {
            part(&format!("  {} to push", ui::plural(ahead as usize, "commit")), DIM);
        }
        if behind > 0 {
            part(&format!("  {} to pull", ui::plural(behind as usize, "commit")), DIM);
        }
    }

    fn footer(&self, s: &mut Screen) {
        let hint = match self.focus {
            Side::List => "↑↓ pick a file   enter read it   space page down   q quit",
            Side::Change => "↑↓ scroll   [ ] next file   esc back   q quit",
        };
        s.put_fit(1, s.h - 1, s.w.saturating_sub(2), hint, DIM);
    }

    fn list_title(&self) -> String {
        format!(" Waiting · {} ", ui::plural(self.detail.files.len(), "file"))
    }

    fn change_title(&self) -> String {
        match self.current_file().and_then(|i| self.detail.files.get(i)) {
            Some(f) => format!(" {} ", f.path),
            None => " Changes ".into(),
        }
    }

    fn draw_list(&mut self, s: &mut Screen) {
        let area = self.areas[0];
        ui::follow(self.sel, &mut self.list_top, area.h);
        // Keep a heading in view above its first file.
        if self.list_top > 0 && self.list_top == self.sel && area.h > 1 {
            self.list_top -= 1;
        }
        if self.items.is_empty() {
            s.put_fit(area.x + 1, area.y, area.w.saturating_sub(2), "Nothing waiting.", DIM);
            return;
        }
        let end = area.x + area.w;
        for (y, (i, item)) in (area.y..).zip(self.items.iter().enumerate().skip(self.list_top).take(area.h)) {
            match *item {
                Item::Group(g) => {
                    let group = &self.detail.groups[g];
                    s.put_fit(area.x + 1, y, area.w.saturating_sub(2), group.title, Style::PLAIN.bold());
                }
                Item::File(f) => {
                    let f = &self.detail.files[f];
                    let size_w = ui::size_right(s, end - 1, y, f.add, f.del);
                    ui::file_name(s, area.x + 2, y, area.w.saturating_sub(size_w + 4), f);
                }
            }
            if i == self.sel {
                s.select(area.x, y, area.w, self.focus == Side::List);
            }
        }
    }

    fn draw_change(&self, s: &mut Screen) {
        let area = self.areas[1];
        if self.detail.lines.is_empty() {
            let text = "Everything here is committed and pushed.";
            let x = area.x + area.w.saturating_sub(text.len()) / 2;
            s.put_fit(x.max(area.x + 1), area.y + area.h / 3, area.w.saturating_sub(2), text, DIM);
            return;
        }
        ui::change(s, &self.detail, area, self.top, self.pan);
        let n = self.detail.lines.len();
        if n > area.h {
            let text = format!(" {}% ", (self.top + area.h).min(n) * 100 / n);
            s.put((area.x + area.w).saturating_sub(text.len() + 1), area.y - 1, text.len(), &text, DIM);
        }
    }

    fn help_box(&self, s: &mut Screen) {
        let inner = HELP.iter().map(|l| term::str_width(l)).max().unwrap_or(0);
        let (bw, bh) = ((inner + 4).min(s.w), (HELP.len() + 2).min(s.h));
        let (x, y) = ((s.w - bw) / 2, (s.h - bh) / 2);
        for row in 0..bh {
            let (l, mid, r) = match row {
                0 => ('┌', '─', '┐'),
                row if row == bh - 1 => ('└', '─', '┘'),
                _ => ('│', ' ', '│'),
            };
            s.fill(x, y + row, bw, mid, DIM);
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
}

impl View for History {
    fn draw(&mut self, s: &mut Screen) {
        s.clear();
        self.areas = [Rect::default(); 2];
        self.divider = 0;
        let (w, h) = (s.w, s.h);
        if w < 30 || h < 5 {
            s.put_fit(0, 0, w, "Too small to show.", Style::PLAIN);
            return;
        }
        self.header(s);
        let body = Rect { x: 0, y: 2, w, h: h - 3 };
        if w >= WIDE.0 && h >= WIDE.1 {
            let left = self.split.unwrap_or(w * 3 / 10).clamp(26, w - 40);
            self.divider = left;
            self.areas = [Rect { w: left, ..body }, Rect { x: left + 1, w: w - left - 1, ..body }];
            ui::title(s, 0, 1, left, &self.list_title(), self.focus == Side::List);
            ui::title(s, left + 1, 1, w - left - 1, &self.change_title(), self.focus == Side::Change);
            s.fill(left, 1, 1, '┬', DIM);
            for y in 2..h - 1 {
                s.fill(left, y, 1, '│', DIM);
            }
            self.draw_list(s);
            self.draw_change(s);
        } else if self.focus == Side::List {
            self.areas[0] = body;
            ui::title(s, 0, 1, w, &self.list_title(), true);
            self.draw_list(s);
        } else {
            self.areas[1] = body;
            ui::title(s, 0, 1, w, &self.change_title(), true);
            self.draw_change(s);
        }
        self.footer(s);
        if self.help {
            self.help_box(s);
        }
    }

    fn on_key(&mut self, key: Key) -> bool {
        let page = self.areas[1].h.max(self.areas[0].h).max(2) as isize - 1;
        let reading = self.focus == Side::Change;
        match key {
            Key::Char('q') | Key::Ctrl(b'c') | Key::Eof => return true,
            Key::Focus(_) => {}
            Key::Click(x, y) => self.click(x, y),
            Key::Drag(x, _) if self.dragging => self.split = Some(x),
            Key::Release => self.dragging = false,
            Key::WheelDown(x, y) if self.areas[0].contains(x, y) => self.move_file(1),
            Key::WheelUp(x, y) if self.areas[0].contains(x, y) => self.move_file(-1),
            Key::WheelDown(..) => self.scroll(3),
            Key::WheelUp(..) => self.scroll(-3),
            _ if self.help => self.help = false,
            Key::Char('?') => self.help = true,
            Key::Char('r') => self.reload(),
            Key::Char(']') => self.move_file(1),
            Key::Char('[') => self.move_file(-1),
            Key::Char(' ') | Key::PgDn | Key::Ctrl(b'f') => self.scroll(page),
            Key::Char('b') | Key::PgUp | Key::Ctrl(b'b') => self.scroll(-page),
            Key::Ctrl(b'd') => self.scroll(page / 2),
            Key::Ctrl(b'u') => self.scroll(-page / 2),
            Key::Char('j') | Key::Down if reading => self.scroll(1),
            Key::Char('k') | Key::Up if reading => self.scroll(-1),
            Key::Char('g') | Key::Home if reading => self.scroll(isize::MIN),
            Key::Char('G') | Key::End if reading => self.scroll(isize::MAX),
            Key::Char('j') | Key::Down => self.move_file(1),
            Key::Char('k') | Key::Up => self.move_file(-1),
            Key::Char('g') | Key::Home => self.move_file(isize::MIN),
            Key::Char('G') | Key::End => self.move_file(isize::MAX),
            Key::Char('l') | Key::Right if reading => self.pan += 8,
            // Left pans back first, then leaves the change.
            Key::Char('h') | Key::Left if reading && self.pan > 0 => self.pan = self.pan.saturating_sub(8),
            Key::Enter | Key::Char('l') | Key::Right => {
                self.select(self.sel);
                self.focus = Side::Change;
            }
            Key::Esc | Key::Char('h') | Key::Left => self.focus = Side::List,
            Key::Tab | Key::BackTab => self.focus = if reading { Side::List } else { Side::Change },
            _ => {}
        }
        false
    }

    fn tick(&mut self) {
        if self.repo.stale(&self.detail) {
            self.reload();
        }
    }
}
