//! Draws a figure large: each digit three cells wide and three rows tall,
//! built from block characters, with half blocks for crossbars and corners.
//!
//! The percent sign stays an ordinary character at the foot of the digits.
//! Drawn as large as they are, it reads as one more digit.

use crate::screen::{Screen, Style};

use super::LARGE_ROWS;

const DIGITS: [[&str; LARGE_ROWS]; 10] = [
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
/// Columns from the start of one large glyph to the start of the next.
const PITCH: usize = 4;

fn large(ch: u8) -> [&'static str; LARGE_ROWS] {
    match ch {
        b'0'..=b'9' => DIGITS[usize::from(ch - b'0')],
        b'<' => [" ▄▀", "█  ", " ▀▄"],
        b'+' => [" ▄ ", "▀█▀", " ▀ "],
        _ => unreachable!("the large glyphs are digits or qualifiers"),
    }
}

/// Each large glyph takes one pitch, including the gap or trailing percent sign.
pub(super) fn width(figure: &str) -> usize {
    PITCH * (figure.len() - 1)
}

/// Draws a figure such as "27%" or "<1%" with its top left at (x, y).
pub(super) fn draw(screen: &mut Screen, mut x: usize, y: usize, figure: &str, colour: Style) {
    for ch in figure[..figure.len() - 1].bytes() {
        for (row, text) in large(ch).into_iter().enumerate() {
            screen.put(x, y + row, PITCH - 1, text, colour);
        }
        x += PITCH;
    }
    screen.put(x - 1, y + LARGE_ROWS - 1, 1, "%", colour.bold());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(figure: &str) -> Vec<String> {
        let mut screen = Screen::sized(width(figure), LARGE_ROWS);
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
