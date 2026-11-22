//! Measures text in terminal columns.

mod data;

pub enum CellWidth {
    /// A control or presentation modifier that is not sent to the terminal.
    Skip,
    /// A printable continuation of the preceding cell.
    Attach,
    Narrow,
    Wide,
}

pub fn cell_width(ch: char) -> CellWidth {
    use CellWidth::*;
    let c = u32::from(ch);
    match c {
        0..=0x1f | 0x7f..=0x9f | 0xad => Skip,
        0x20..=0x2ff => Narrow,
        // Omit Unicode default-ignorables (except Hangul fillers), along
        // with other controls and modifiers that can change how a row is drawn.
        0x34f
        | 0x61c
        | 0x17b4..=0x17b5
        | 0x180b..=0x180f
        | 0x200b..=0x200f
        | 0x2028..=0x202e
        | 0x2060..=0x206f
        | 0x20e3
        | 0xfe00..=0xfe0f
        | 0xfeff
        | 0xfff0..=0xfffb
        | 0x1bca0..=0x1bca3
        | 0x1d173..=0x1d17a
        | 0xe0000..=0xe0fff => Skip,
        // Vowel and trailing Jamo join the preceding Hangul cell. Include the
        // assigned ranges in Jamo Extended-B (HangulSyllableType.txt, V and T).
        0x1160..=0x11ff | 0xd7b0..=0xd7c6 | 0xd7cb..=0xd7fb => Attach,
        _ if data::is_nonspacing(c) => Attach,
        _ if data::is_wide(c) => Wide,
        _ => Narrow,
    }
}

/// Approximates a scalar's width in terminal columns.
pub fn char_width(ch: char) -> usize {
    match cell_width(ch) {
        CellWidth::Skip | CellWidth::Attach => 0,
        CellWidth::Narrow => 1,
        CellWidth::Wide => 2,
    }
}

/// The byte offset of the first base character starting at or after column
/// `col`, or the length of `text`. Never cuts before an attached continuation.
pub fn byte_at_col(text: &str, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    // Printable ASCII takes a column a byte, so a run of it that starts the text is not measured.
    let plain = text.bytes().take(col).take_while(|byte| matches!(byte, b' '..=b'~')).count();
    let mut start = plain;
    for (byte, ch) in text[plain..].char_indices() {
        let width = char_width(ch);
        if width > 0 && start >= col {
            return plain + byte;
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
    fn default_ignorable_formatting_does_not_split_or_shift_a_row() {
        let text = "a\u{17b4}\u{180b}\u{180f}\u{1bca0}\u{1d173}b";
        assert_eq!(str_width(text), 2);
        assert_eq!(byte_at_col(text, 1), text.len() - 1);
    }

    #[test]
    fn only_east_asian_wide_and_fullwidth_scalars_take_two_columns() {
        for ch in ['🟢', '☰', '𝌆', 'Ａ', '\u{1fa8a}', '\u{2fffd}', '\u{3fffd}'] {
            assert_eq!(char_width(ch), 2, "{ch:?}");
        }
        for ch in ['🛠', '⚒', '🇺', '·', '･', '\u{2e9a}', '\u{2fffe}', '\u{3fffe}'] {
            assert_eq!(char_width(ch), 1, "{ch:?}");
        }
        assert!(matches!(cell_width('\u{3099}'), CellWidth::Attach));
        for ch in ['\u{200d}', '\u{20e3}', '\u{fe0f}', '\u{e0100}'] {
            assert!(matches!(cell_width(ch), CellWidth::Skip), "{ch:?}");
        }
    }

    #[test]
    fn panning_steps_over_both_columns_of_wide_emoji_but_not_bare_text_symbols() {
        let text = "🟢🛠\u{fe0f}|";
        assert_eq!(byte_at_col(text, 1), "🟢".len());
        assert_eq!(byte_at_col(text, 2), "🟢".len());
        assert_eq!(byte_at_col(text, 3), "🟢🛠\u{fe0f}".len());
    }

    #[test]
    fn nonspacing_marks_and_conjoining_jamo_do_not_advance_columns() {
        for (text, cols) in
            [("กิ้", 1), ("กำ", 2), ("क़", 1), ("क्", 1), ("कि", 2), ("का", 2), ("한글", 4), ("한글", 4), ("ꥠힰퟋ", 2)]
        {
            assert_eq!(str_width(text), cols, "{text:?}");
        }
    }

    #[test]
    fn panning_keeps_nonspacing_marks_and_jamo_with_their_base() {
        let text = "กิक़한x";
        assert_eq!(byte_at_col(text, 1), "กิ".len());
        assert_eq!(byte_at_col(text, 2), "กิक़".len());
        assert_eq!(byte_at_col(text, 3), "กิक़한".len());
        assert_eq!(byte_at_col(text, 4), "กิक़한".len());
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

    #[test]
    fn a_run_of_plain_text_is_stepped_over_to_where_measuring_it_would_stop() {
        let measured = |text: &str, col: usize| {
            let mut start = 0;
            let at = text.char_indices().find(|&(_, ch)| {
                let found = char_width(ch) > 0 && start >= col;
                start += char_width(ch);
                found
            });
            if col == 0 { 0 } else { at.map_or(text.len(), |(byte, _)| byte) }
        };
        let texts = [
            "", "a", "plain text only", "tab\tbell\x07 in it", "ab界cd", "plain e\u{301}x", "~ {|} \u{7f}y",
            "abc\u{200b}\u{301}def", "日本語 then plain", "é plain after", "ends wide 界", "marked e\u{301}",
        ];
        for text in texts {
            for col in 0..=str_width(text) + 2 {
                assert_eq!(byte_at_col(text, col), measured(text, col), "{text:?} at {col}");
            }
        }
    }
}
