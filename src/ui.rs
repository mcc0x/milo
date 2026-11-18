//! Small drawing helpers.

use crate::git::{FileChange, Kind};
use crate::icons::{self, Icon};
use crate::term::{self, Screen, Style};

pub const DIM: Style = Style::fg(term::GRAY);
const ADD: Style = Style::fg(term::GREEN);
const DEL: Style = Style::fg(term::RED);

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

/// Draws an icon in its own colour, and the gap after it. Returns the
/// columns used.
pub fn icon(s: &mut Screen, x: usize, y: usize, max_w: usize, icon: Icon) -> usize {
    if icon.glyph.is_empty() || max_w < 2 {
        return 0;
    }
    s.put(x, y, 1, icon.glyph, Style::fg(icon.colour));
    2
}

/// "name parent/dir": the file name first, so it survives a narrow column.
pub fn file_name(s: &mut Screen, x: usize, y: usize, max_w: usize, f: &FileChange) {
    let st = kind_style(f.kind);
    let st = if f.committed { st } else { st.bold() };
    let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
    let mut used = icon(s, x, y, max_w, icons::file(name));
    used += s.put_middle(x + used, y, max_w - used, name, st);
    if !dir.is_empty() && used + 2 < max_w {
        s.put_fit(x + used + 1, y, max_w - used - 1, dir, DIM);
    }
}
