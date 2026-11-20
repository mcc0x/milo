//! Draws a figure large: each digit three cells wide and three rows tall,
//! built from block characters, with half blocks for crossbars and corners.
//!
//! A percent sign or a token count's unit stays an ordinary character at the
//! foot of the digits. Drawn as large as they are, it reads as one more digit.

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
/// Columns from the start of one large glyph to the start of the next. A
/// decimal point is one column wide, so that it does not part the digits.
const fn pitch(ch: u8) -> usize {
    if ch == b'.' { 2 } else { 4 }
}

fn large(ch: u8) -> [&'static str; LARGE_ROWS] {
    match ch {
        b'0'..=b'9' => DIGITS[usize::from(ch - b'0')],
        b'<' => [" ▄▀", "█  ", " ▀▄"],
        b'+' => [" ▄ ", "▀█▀", " ▀ "],
        b'.' => [" ", " ", "▄"],
        _ => unreachable!("the large glyphs are digits, a point or qualifiers"),
    }
}

/// How many characters stay small at the end: a percent sign or a unit, but never a digit.
fn small(figure: &str) -> usize {
    usize::from(!figure.ends_with(|ch: char| ch.is_ascii_digit()))
}

/// Each large glyph takes its pitch, including the gap or the small character after the last.
pub(super) fn width(figure: &str) -> usize {
    let small = small(figure);
    figure[..figure.len() - small].bytes().map(pitch).sum::<usize>() - (1 - small)
}

/// Draws a figure such as "27%", "<1%" or "48.2k" with its top left at (x, y).
pub(super) fn draw(screen: &mut Screen, mut x: usize, y: usize, figure: &str, colour: Style) {
    let (glyphs, unit) = figure.split_at(figure.len() - small(figure));
    for ch in glyphs.bytes() {
        for (row, text) in large(ch).into_iter().enumerate() {
            screen.put(x, y + row, pitch(ch) - 1, text, colour);
        }
        x += pitch(ch);
    }
    if !unit.is_empty() {
        screen.put(x - 1, y + LARGE_ROWS - 1, 1, unit, colour.bold());
    }
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
    fn a_token_count_keeps_its_point_narrow_and_its_unit_small() {
        assert_eq!(drawn("48.2k"), ["█ █ ▟▀▙   ▀▀▙", "▀▀█ ▟▀▙   ▟▀▀", "  █ ▜▄▛ ▄ █▄▄k"]);
        assert_eq!(drawn("950"), ["▟▀▙ █▀▀ ▟▀▙", "▀▀█ ▀▀▙ █ █", "▄▄▛ ▄▄▛ ▜▄▛"]);
    }

    #[test]
    fn every_figure_the_pane_can_show_fits_the_width_it_claims() {
        // Too wide a claim would leave a hole; too narrow would cut a glyph.
        for figure in ["0%", "7%", "42%", "100%", "<1%", "99+%", "7", "950", "1.5k", "48.2k", "120M"] {
            let rows = drawn(figure);
            let widest = rows.iter().map(|row| row.chars().count()).max().unwrap();
            assert_eq!(widest, width(figure), "{figure}: {rows:?}");
        }
    }
}
