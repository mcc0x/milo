//! Draws a figure large: each digit three cells wide and three rows tall,
//! built from block characters, with half blocks for crossbars and corners.
//!
//! The percent sign stays an ordinary character at the foot of the digits.
//! Drawn as large as they are, it reads as one more digit.

use crate::screen::{Screen, Style};

const DIGITS: [[&str; 3]; 10] = [
    ["▟▀▙", "█ █", "▜▄▛"], // 0
    ["▄█ ", " █ ", "▄█▄"], // 1
    ["▀▀▙", "▟▀▀", "█▄▄"], // 2
    ["▀▀▙", " ▀█", "▄▄▛"], // 3
    ["█ █", "▀▀█", "  █"], // 4
    ["█▀▀", "▀▀▙", "▄▄▛"], // 5
    ["▟▀▀", "█▀▙", "▜▄▛"], // 6
    ["▀▀█", " ▟▀", " █ "], // 7
    ["▟▀▙", "▟▀▙", "▜▄▛"], // 8
    ["▟▀▙", "▀▀█", "▄▄▛"], // 9
];
/// Rows a large figure takes.
const ROWS: usize = 3;
/// Columns from the start of one large glyph to the start of the next.
const PITCH: usize = 4;

/// The glyph for one character of a figure, if it is drawn large.
fn large(ch: u8) -> Option<[&'static str; 3]> {
    match ch {
        b'0'..=b'9' => Some(DIGITS[usize::from(ch - b'0')]),
        b'<' => Some([" ▄▀", "█  ", " ▀▄"]),
        b'+' => Some([" ▄ ", "▀█▀", " ▀ "]),
        _ => None,
    }
}

/// The columns a figure takes: its large glyphs with a column between
/// them, and one more for a percent sign.
pub(super) fn width(figure: &str) -> usize {
    let glyphs = figure.bytes().filter(|&ch| large(ch).is_some()).count();
    let small = figure.len() - glyphs;
    (PITCH * glyphs).saturating_sub(1) + small
}

/// Draws a figure such as "27%" or "<1%" with its top left at (x, y).
pub(super) fn draw(screen: &mut Screen, mut x: usize, y: usize, figure: &str, colour: Style) {
    for ch in figure.bytes() {
        if let Some(glyph) = large(ch) {
            for (row, text) in glyph.into_iter().enumerate() {
                screen.put(x, y + row, PITCH - 1, text, colour);
            }
            x += PITCH;
        } else {
            // Close up to the last digit, at its foot.
            x = x.saturating_sub(1);
            x += screen.put(x, y + ROWS - 1, 1, "%", colour.bold());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(figure: &str) -> Vec<String> {
        let mut screen = Screen::sized(width(figure), ROWS);
        draw(&mut screen, 0, 0, figure, Style::PLAIN);
        screen.text()
    }

    #[test]
    fn a_figure_is_large_digits_with_a_small_percent_sign_at_their_foot() {
        assert_eq!(drawn("23%"), ["▀▀▙ ▀▀▙", "▟▀▀  ▀█", "█▄▄ ▄▄▛%"]);
        assert_eq!(drawn("0%"), ["▟▀▙", "█ █", "▜▄▛%"]);
    }

    #[test]
    fn every_figure_the_pane_can_show_fits_the_width_it_claims() {
        // Too wide a claim would leave a hole; too narrow would cut a glyph.
        for figure in ["0%", "7%", "42%", "100%", "<1%", "99+%"] {
            let rows = drawn(figure);
            let widest = rows.iter().map(|row| row.chars().count()).max().unwrap();
            assert_eq!(widest, width(figure), "{figure}: {rows:?}");
        }
    }
}
