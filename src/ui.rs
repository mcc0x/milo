//! Drawing shared by both views.

use crate::git::{Detail, FileChange, Kind, LineKind};
use crate::icons;
use crate::term::{self, Screen, Style};

pub const DIM: Style = Style::fg(term::GRAY);
pub const ACCENT: Style = Style::fg(term::CYAN).bold();
pub const ADD: Style = Style::fg(term::GREEN);
pub const DEL: Style = Style::fg(term::RED);

#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, x: usize, y: usize) -> bool {
        (self.x..self.x + self.w).contains(&x) && (self.y..self.y + self.h).contains(&y)
    }
}

/// Colour says what happened to a file; the face backs it up, so the states
/// stay apart even where two colours look alike.
pub fn kind_style(kind: Kind) -> Style {
    match kind {
        Kind::None => Style::PLAIN,
        Kind::Modified => Style::fg(term::YELLOW),
        Kind::Added => ADD,
        Kind::Untracked => ADD.italic(),
        Kind::Deleted => DEL.strike(),
        Kind::Renamed => Style::fg(term::CYAN).italic(),
        Kind::Conflict => Style::fg(term::MAGENTA).bold(),
    }
}

pub fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::None => "",
        Kind::Modified => "edited",
        Kind::Added | Kind::Untracked => "new",
        Kind::Deleted => "removed",
        Kind::Renamed => "moved",
        Kind::Conflict => "conflict",
    }
}

pub fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Keeps `sel` inside a window of `h` rows starting at `top`.
pub fn follow(sel: usize, top: &mut usize, h: usize) {
    if sel < *top {
        *top = sel;
    } else if sel >= *top + h {
        *top = sel + 1 - h;
    }
}

pub fn title(s: &mut Screen, x: usize, y: usize, w: usize, text: &str, focused: bool) {
    s.fill(x, y, w, '─', DIM);
    let st = if focused { ACCENT } else { Style::PLAIN.bold() };
    s.put_fit(x + 1, y, w.saturating_sub(2), text, st);
}

/// Draws "+12 −3" ending just before column `end`. Returns its width.
pub fn size_right(s: &mut Screen, end: usize, y: usize, add: u32, del: u32) -> usize {
    let a = if add > 0 { format!("+{add}") } else { String::new() };
    let d = if del > 0 { format!("−{del}") } else { String::new() };
    let gap = (!a.is_empty() && !d.is_empty()) as usize;
    let w = a.len() + gap + term::str_width(&d);
    let x = end.saturating_sub(w);
    s.put(x, y, a.len(), &a, ADD);
    s.put(x + a.len() + gap, y, w, &d, DEL);
    w
}

/// Draws an icon and the gap after it. Returns the columns used.
pub fn icon(s: &mut Screen, x: usize, y: usize, max_w: usize, icon: &str, st: Style) -> usize {
    if icon.is_empty() || max_w < 2 {
        return 0;
    }
    s.put(x, y, 1, icon, st);
    2
}

/// "name parent/dir": the file name first, so it survives a narrow column.
pub fn file_name(s: &mut Screen, x: usize, y: usize, max_w: usize, f: &FileChange) -> usize {
    let st = kind_style(f.kind);
    let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
    let mut used = icon(s, x, y, max_w, icons::file(name), st);
    used += s.put_fit(x + used, y, max_w - used, name, st.bold());
    if !dir.is_empty() && used + 2 < max_w {
        used += 1 + s.put_fit(x + used + 1, y, max_w - used - 1, dir, DIM);
    }
    used
}

/// The pending changes as text: a bar per file, numbered lines, and the
/// words that changed inside an edited line picked out.
pub fn change(s: &mut Screen, detail: &Detail, area: Rect, top: usize, pan: usize) {
    let end = area.x + area.w;
    let digits = detail.max_no.max(1).ilog10() as usize + 1;
    // [space][line number][space][sign][space][text]
    let (no_x, text_x) = (area.x + 1, area.x + digits + 4);
    let text_w = end.saturating_sub(text_x);
    for (y, line) in (area.y..).zip(detail.lines.iter().skip(top).take(area.h)) {
        let prose = |s: &mut Screen, st| s.put_fit(area.x + 1, y, area.w.saturating_sub(2), &line.text, st);
        let (sign, st) = match line.kind {
            LineKind::Title => {
                prose(s, ACCENT);
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
                if let Some(f) = detail.files.get(i as usize) {
                    file_bar(s, f, area, y);
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
        s.put_skip(text_x, y, text_w, &line.text, st, pan);
        let (a, b) = (line.changed.0 as usize, line.changed.1 as usize);
        if a < b {
            let from = term::str_width(&line.text[..a]).saturating_sub(pan);
            let to = term::str_width(&line.text[..b]).saturating_sub(pan);
            s.emphasise(text_x + from, y, (to - from).min(text_w.saturating_sub(from)));
        }
    }
}

/// One bar per file: its icon and path, what happened to it, and how much.
fn file_bar(s: &mut Screen, f: &FileChange, area: Rect, y: usize) {
    let end = area.x + area.w;
    let size_w = size_right(s, end - 1, y, f.add, f.del);
    let word = kind_word(f.kind);
    let room = (area.w - 2).saturating_sub(size_w + word.len() + 3);
    let name = f.path.rsplit_once('/').map_or(f.path.as_str(), |(_, n)| n);
    let mut x = area.x + 1;
    x += icon(s, x, y, room, icons::file(name), kind_style(f.kind));
    x += s.put_fit(x, y, (area.x + 1 + room).saturating_sub(x), &f.path, Style::PLAIN.bold());
    s.put(x + 2, y, word.len(), word, kind_style(f.kind));
    s.tint(area.x, y, area.w);
}
