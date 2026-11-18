//! Measures text in terminal columns.

/// Nonspacing marks that can safely follow a base glyph. Format characters,
/// variation selectors and the emoji keycap mark are not printable accents.
pub fn is_combining(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x300..=0x34e
            | 0x350..=0x36f
            | 0x483..=0x489
            | 0x591..=0x5bd
            | 0x5bf
            | 0x5c1..=0x5c2
            | 0x5c4..=0x5c5
            | 0x5c7
            | 0x610..=0x61a
            | 0x64b..=0x65f
            | 0x670
            | 0x1ab0..=0x1ace
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20e2
            | 0x20e4..=0x20f0
            | 0x3099..=0x309a
            | 0xfe20..=0xfe2f
    )
}

/// Approximates a character's terminal width. Zero-width controls are
/// dropped; printable marks take no extra columns after a base glyph.
pub fn char_width(ch: char) -> usize {
    let c = u32::from(ch);
    match c {
        0..=0x1f | 0x7f..=0x9f | 0xad => 0,
        0x20..=0x2ff => 1,
        _ if is_combining(ch) => 0,
        // Zero-width format characters, direction overrides and glyph modifiers.
        0x34f
        | 0x61c
        | 0x180e
        | 0x200b..=0x200f
        | 0x2028..=0x202e
        | 0x2060..=0x206f
        | 0x20e3
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

/// The byte offset of the first base character starting at or after column
/// `col`, or the length of `text`. Never cuts between a base and its marks.
pub fn byte_at_col(text: &str, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    let mut start = 0;
    for (byte, ch) in text.char_indices() {
        let width = char_width(ch);
        if width > 0 && start >= col {
            return byte;
        }
        start += width;
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
        assert_eq!(str_width("漢字-e\u{0301}"), 6);
        assert_eq!(str_width("か\u{3099}"), 2);
    }

    #[test]
    fn column_offsets_always_land_on_character_boundaries() {
        assert_eq!(byte_at_col("abcd", 2), 2);
        assert_eq!(byte_at_col("a日b", 1), 1);
        assert_eq!(byte_at_col("a日b", 2), "a日".len());
        assert_eq!(byte_at_col("a\u{0301}b", 1), "a\u{0301}".len());
        assert_eq!(byte_at_col("日\u{0301}b", 1), "日\u{0301}".len());
        assert_eq!(byte_at_col("a\u{200b}\u{0301}b", 1), "a\u{200b}\u{0301}".len());
        assert_eq!(byte_at_col("a\u{0301}b", 2), "a\u{0301}b".len());
        assert_eq!(byte_at_col("ab", 20), 2);
    }
}
