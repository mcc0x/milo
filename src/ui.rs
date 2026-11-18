//! Small drawing helpers.

use crate::git::{FileChange, Kind};
use crate::icons::{self, Icon};
use crate::screen::{self, Screen, Style};

/// Text that steps back. Faint, not grey: it stays a shade of the theme's
/// own text colour, where a theme's grey can match its background.
pub const DIM: Style = Style::PLAIN.faint();
pub const ADD: Style = Style::fg(screen::GREEN);
pub const DEL: Style = Style::fg(screen::RED);

/// The colour that says what happened to a file.
pub fn kind_colour(kind: Kind) -> Style {
    match kind {
        Kind::None => Style::PLAIN,
        Kind::Modified => Style::fg(screen::YELLOW),
        Kind::Added | Kind::Untracked => ADD,
        Kind::Deleted => DEL,
        Kind::Renamed => Style::fg(screen::CYAN),
        Kind::Conflict => Style::fg(screen::MAGENTA),
    }
}

/// How a file's own name is drawn: its colour, and a face to back it up,
/// so the states stay apart even where two colours look alike. The face is
/// for the name only; a word such as "removed" struck through would read as
/// taken back.
pub fn kind_style(kind: Kind) -> Style {
    let colour = kind_colour(kind);
    match kind {
        Kind::Untracked | Kind::Renamed => colour.italic(),
        Kind::Deleted => colour.strike(),
        Kind::Conflict => colour.bold(),
        _ => colour,
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

/// Columns "+12 −3" takes for these counts.
pub fn size_width(add: u32, del: u32) -> usize {
    let digits = |n: u32| if n > 0 { n.ilog10() as usize + 2 } else { 0 };
    digits(add) + digits(del) + (add > 0 && del > 0) as usize
}

/// Draws "+12 −3" ending just before column `end`. Returns its width.
pub fn size_right(s: &mut Screen, end: usize, y: usize, add: u32, del: u32) -> usize {
    let mut used = 0;
    if del > 0 {
        used += s.put_right(end, y, &format!("−{del}"), DEL);
    }
    if add > 0 {
        let gap = usize::from(del > 0);
        used += gap + s.put_right(end.saturating_sub(used + gap), y, &format!("+{add}"), ADD);
    }
    used
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

/// Draws a scroll thumb in column `x` beside `h` rows starting at `y0`, for
/// a list of `len` rows scrolled to `top`. Nothing if it all fits.
pub fn thumb(s: &mut Screen, x: usize, y0: usize, h: usize, len: usize, top: usize) {
    if len <= h || h == 0 {
        return;
    }
    let size = (h * h / len).max(1);
    let at = top.min(len - h) * (h - size) / (len - h);
    for y in y0 + at..y0 + at + size {
        s.put(x, y, 1, "▐", DIM);
    }
}
