//! Measures text in terminal columns.

/// Terminal columns a character occupies: none for control, combining and
/// invisible formatting characters, two for East Asian wide characters and
/// emoji, one for everything else. Text of unknown origin is drawn through
/// this, so what it calls zero-width is never sent to the terminal at all.
pub fn char_width(c: char) -> usize {
    let c = u32::from(c);
    match c {
        0..=0x1f | 0x7f..=0x9f => 0,
        0x20..=0x2ff => 1,
        // Combining marks.
        0x300..=0x36f
        | 0x483..=0x489
        | 0x591..=0x5bd
        | 0x610..=0x61a
        | 0x64b..=0x65f
        | 0x1ab0..=0x1aff
        | 0x1dc0..=0x1dff
        | 0x20d0..=0x20ff
        | 0xfe20..=0xfe2f => 0,
        // Zero-width and direction-changing characters, variation
        // selectors and tags: invisible, and able to reorder what is shown.
        0x61c
        | 0x200b..=0x200f
        | 0x2028..=0x202e
        | 0x2060..=0x206f
        | 0xfe00..=0xfe0f
        | 0xfeff
        | 0xfff9..=0xfffb
        | 0xe0000..=0xe0fff => 0,
        // Emoji outside the main blocks that terminals draw two columns wide.
        0x231a..=0x231b
        | 0x23e9..=0x23ec
        | 0x23f0
        | 0x23f3
        | 0x25fd..=0x25fe
        | 0x2614..=0x2615
        | 0x2648..=0x2653
        | 0x267f
        | 0x2693
        | 0x26a1
        | 0x26aa..=0x26ab
        | 0x26bd..=0x26be
        | 0x26c4..=0x26c5
        | 0x26ce
        | 0x26d4
        | 0x26ea
        | 0x26f2..=0x26f3
        | 0x26f5
        | 0x26fa
        | 0x26fd
        | 0x2705
        | 0x270a..=0x270b
        | 0x2728
        | 0x274c
        | 0x274e
        | 0x2753..=0x2755
        | 0x2757
        | 0x2795..=0x2797
        | 0x27b0
        | 0x27bf
        | 0x2b1b..=0x2b1c
        | 0x2b50
        | 0x2b55 => 2,
        0x1100..=0x115f
        | 0x2e80..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f680..=0x1f6ff
        | 0x1f900..=0x1faff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

/// The byte offset of the first character starting at or after column `col`,
/// or the length of `text` if no character starts there.
pub fn byte_at_col(text: &str, col: usize) -> usize {
    let mut start = 0;
    for (byte, ch) in text.char_indices() {
        if start >= col {
            return byte;
        }
        start += char_width(ch);
    }
    text.len()
}

pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_cover_what_breaks_a_row() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("日本"), 4);
        assert_eq!(str_width("🚀✨"), 4);
        // A bidirectional override and a zero-width space take no room.
        assert_eq!(str_width("a\u{202e}b\u{200b}c\u{2066}"), 3);
        assert_eq!(str_width("\x1b[31m"), 4);
    }

    #[test]
    fn column_offsets_always_land_on_character_boundaries() {
        assert_eq!(byte_at_col("abcd", 2), 2);
        assert_eq!(byte_at_col("a日b", 1), 1);
        assert_eq!(byte_at_col("a日b", 2), "a日".len());
        assert_eq!(byte_at_col("a\u{0301}b", 1), 1);
        assert_eq!(byte_at_col("a\u{0301}b", 2), "a\u{0301}b".len());
        assert_eq!(byte_at_col("ab", 20), 2);
    }
}
