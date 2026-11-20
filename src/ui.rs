//! Shares headings, status styles, icons and scroll thumbs across screens.

use crate::git::{Kind, PendingFile};
use crate::icons::{self, Icon};
use crate::preferences::IconMode;
use crate::screen::{self, Screen, Style};
use crate::when::Age;
use crate::width::str_width;

/// Faint text follows the theme's foreground, unlike a fixed grey.
pub const FAINT: Style = Style::PLAIN.faint();
pub const HEADING: Style = Style::PLAIN.bold();
pub const ADD: Style = Style::fg(screen::GREEN);
pub const DEL: Style = Style::fg(screen::RED);

/// Titles are inset by one column and yield to whatever follows on the same line.
pub fn section_heading(s: &mut Screen, y: usize, width: usize, text: &str) -> usize {
    s.put_fit(1, y, width, text, HEADING)
}

/// The space a project name and branch would take before shortening either.
pub fn project_heading_width(name: &str, branch: &str) -> usize {
    str_width(name) + str_width(branch) + 5
}

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

/// Names have a second cue besides colour, such as italics or a strike.
/// Status labels use colour alone so a word like "removed" stays legible.
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
pub fn size_width(add: impl Into<u64>, del: impl Into<u64>) -> usize {
    let (add, del) = (add.into(), del.into());
    let digits = |n: u64| if n > 0 { n.ilog10() as usize + 2 } else { 0 };
    digits(add) + digits(del) + (add > 0 && del > 0) as usize
}

/// Draws "+12 −3" ending just before column `end`. Returns its width.
pub fn size_right(s: &mut Screen, end: usize, y: usize, add: impl Into<u64>, del: impl Into<u64>) -> usize {
    let (add, del) = (add.into(), del.into());
    let mut used = 0;
    if del > 0 {
        used += signed_right(s, end, y, "−", del, DEL);
    }
    if add > 0 {
        let gap = usize::from(del > 0);
        used += gap + signed_right(s, end.saturating_sub(used + gap), y, "+", add, ADD);
    }
    used
}

fn signed_right(s: &mut Screen, end: usize, y: usize, sign: &str, count: u64, st: Style) -> usize {
    let end = end.min(s.w);
    let digits = count.ilog10() + 1;
    let used = (digits as usize + 1).min(end);
    if used == 0 || y >= s.h {
        return 0;
    }
    s.put(end - used, y, 1, sign, st);
    if used > 1 {
        // Like clipped text, keep the first digits, not the last ones.
        let clipped = u32::try_from(digits as usize + 1 - used).expect("a u64 has at most twenty digits");
        s.put_number(end, y, count / 10u64.pow(clipped), st);
    }
    used
}

/// Draws a compact age that has room to fit.
pub fn age_right(s: &mut Screen, end: usize, y: usize, age: Age) -> usize {
    let used = s.put_right(end, y, age.suffix, FAINT);
    if let Some(count) = age.count {
        s.put_number(end - used, y, count, FAINT);
    }
    age.width()
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

/// Plain letters encode both the Git change and whether it needs committing or pushing.
pub fn git_mark(s: &mut Screen, x: usize, y: usize, room: usize, kind: Kind, uncommitted: bool) -> usize {
    let change = match kind {
        Kind::None => return 0,
        Kind::Modified => "M",
        Kind::Added | Kind::Untracked => "A",
        Kind::Deleted => "D",
        Kind::Renamed => "R",
        Kind::Conflict => "U",
    };
    if room < 5 {
        return 0;
    }
    s.put(x, y, 1, change, Style::PLAIN);
    s.put(x + 2, y, 1, if uncommitted { "C" } else { "P" }, Style::PLAIN);
    4
}

/// "name parent/dir": the file name first, so it survives a narrow column.
pub fn file_name(s: &mut Screen, x: usize, y: usize, max_w: usize, f: &PendingFile, icons: IconMode, text: bool) {
    let st = kind_style(f.kind);
    let st = if f.unpushed { st } else { st.bold() };
    let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
    let mut used = if text { git_mark(s, x, y, max_w, f.kind, !f.unpushed) } else { 0 };
    if used == 0 || max_w - used >= 4 {
        used += icon(s, x + used, y, max_w - used, icons::file(icons, name));
    }
    used += s.put_middle(x + used, y, max_w - used, name, st);
    if !dir.is_empty() && used + 2 < max_w {
        s.put_fit(x + used + 1, y, max_w - used - 1, dir, FAINT);
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
        s.put(x, y, 1, "▐", FAINT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipped_change_counts_keep_the_sign_and_first_digits() {
        for w in 0..25 {
            for end in 0..25 {
                for (add, del) in [(12, 345), (u64::MAX, 0), (0, u64::MAX), (1, 1), (0, 0)] {
                    let mut screen = Screen::sized(w, 1);
                    let actual = size_right(&mut screen, end, 0, add, del);
                    let mut expected = Screen::sized(w, 1);
                    let mut used = 0;
                    if del > 0 {
                        used += expected.put_right(end, 0, &format!("−{del}"), DEL);
                    }
                    if add > 0 {
                        let gap = usize::from(del > 0);
                        used += gap + expected.put_right(end.saturating_sub(used + gap), 0, &format!("+{add}"), ADD);
                    }
                    assert_eq!(actual, used);
                    assert_eq!(screen.text(), expected.text());
                }
            }
        }
    }
}
