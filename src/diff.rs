//! Parses whole-file diffs and marks the bytes that differ in paired lines.

use std::borrow::Cow;
use std::ops::Range;

use crate::git::Kind;
use crate::width::{CellWidth, cell_width, char_width};

const TAB: usize = 4;

/// What happened to a line relative to the version compared against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Same,
    Add,
    Del,
}

/// One numbered line of a whole-file diff or of the file itself.
pub struct Line {
    pub kind: LineKind,
    /// The current line number, or its number in the base if it was removed.
    pub number: u32,
    pub text: String,
    /// Width of the source text, measured once so long lines need not be measured on every draw.
    pub width: usize,
    /// Display bytes to emphasize, including the base of any changed mark.
    pub changed: Range<usize>,
    /// Visible explanations for changed characters that are hard to see in the source text.
    pub annotation: String,
}

impl Line {
    /// Makes a line with tabs expanded and control characters left out.
    pub fn new(kind: LineKind, number: u32, text: &str) -> Self {
        Self::edited(kind, number, text, 0..0, String::new())
    }

    fn edited(kind: LineKind, number: u32, text: &str, changed: Range<usize>, annotation: String) -> Self {
        let (text, width, changed) = clean(text, changed);
        Self { kind, number, text, width, changed, annotation }
    }

    pub fn display_width(&self) -> usize {
        self.width + self.annotation.len() // Annotations are ASCII.
    }
}

/// Expands tabs and drops control characters, translating the edit to display bytes.
fn clean(line: &str, changed: Range<usize>) -> (String, usize, Range<usize>) {
    if line.bytes().all(|b| matches!(b, b' '..=b'~')) {
        return (line.to_owned(), line.len(), changed);
    }
    let mut s = String::with_capacity(line.len());
    let mut col = 0;
    let mut range = 0..0;
    for (at, c) in line.char_indices() {
        if at == changed.start {
            range.start = s.len();
        }
        if at == changed.end {
            range.end = s.len();
        }
        if c == '\t' {
            let pad = TAB - col % TAB;
            s.extend(std::iter::repeat_n(' ', pad));
            col += pad;
        } else if !c.is_control() {
            s.push(c);
            col += char_width(c);
        }
    }
    if changed.start == line.len() {
        range.start = s.len();
    }
    if changed.end == line.len() {
        range.end = s.len();
    }
    (s, col, range)
}

/// Bytes two strings share at the start and the end, without overlap.
fn common_ends(a: &str, b: &str) -> (usize, usize) {
    let prefix: usize = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    let (a, b) = (&a[prefix..], &b[prefix..]);
    let suffix = a.chars().rev().zip(b.chars().rev()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    (prefix, suffix)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    Lf,
    CrLf,
    None,
}

impl Ending {
    fn label(self) -> &'static str {
        match self {
            Self::Lf => " [LF]",
            Self::CrLf => " [CRLF]",
            Self::None => " [no newline]",
        }
    }
}

struct PatchLine<'a> {
    kind: LineKind,
    number: u32,
    text: &'a str,
    ending: Ending,
    changed: Range<usize>,
    annotation: String,
}

fn hidden(c: char) -> bool {
    c != '\t' && matches!(cell_width(c), CellWidth::Skip | CellWidth::Attach)
}

/// Moves a mark-only edit onto the cell it affects, on both sides of the edit.
fn include_base(text: &str, changed: &mut Range<usize>) {
    if let Some((at, _)) = text[..changed.start].char_indices().rev().find(|&(_, c)| !hidden(c)) {
        changed.start = at;
    } else if text[changed.clone()].chars().all(hidden)
        && let Some((at, c)) = text[changed.end..].char_indices().find(|&(_, c)| !hidden(c))
    {
        changed.end += at + c.len_utf8();
    }
}

fn mark_label(c: char) -> &'static str {
    match c {
        '\r' => "carriage return",
        '\u{00ad}' => "soft hyphen",
        '\u{0300}' => "combining grave",
        '\u{0301}' => "combining acute",
        '\u{0302}' => "combining circumflex",
        '\u{0303}' => "combining tilde",
        '\u{0304}' => "combining macron",
        '\u{0308}' => "combining diaeresis",
        '\u{030a}' => "combining ring",
        '\u{0327}' => "combining cedilla",
        '\u{034f}' => "combining grapheme joiner",
        '\u{200b}' => "zero-width space",
        '\u{200c}' => "zero-width non-joiner",
        '\u{200d}' => "zero-width joiner",
        '\u{2060}' => "word joiner",
        '\u{fe00}'..='\u{fe0f}' | '\u{e0100}'..='\u{e01ef}' => "variation selector",
        '\u{feff}' => "BOM",
        c if c.is_control() => "control",
        _ => "zero-width",
    }
}

fn annotate(line: &mut PatchLine<'_>, tabs_or_spaces: bool, ending_changed: bool) {
    use std::fmt::Write as _;

    let edit = &line.text[line.changed.clone()];
    if tabs_or_spaces {
        let tabs = edit.bytes().filter(|&b| b == b'\t').count();
        let spaces = edit.len() - tabs;
        for (n, name) in [(tabs, "tab"), (spaces, "space")] {
            if n == 1 {
                let _ = write!(line.annotation, " [{name}]");
            } else if n > 1 {
                let _ = write!(line.annotation, " [{n} {name}s]");
            }
        }
    } else {
        let mut marks = edit.chars().filter(|&c| hidden(c)).peekable();
        while let Some(c) = marks.next() {
            let mut count = 1;
            while marks.next_if_eq(&c).is_some() {
                count += 1;
            }
            let _ = write!(line.annotation, " [{} U+{:04X}", mark_label(c), u32::from(c));
            if count > 1 {
                let _ = write!(line.annotation, " x{count}");
            }
            line.annotation.push(']');
        }
    }
    if ending_changed {
        line.annotation.push_str(line.ending.label());
    }
}

fn mark_edit(old: &mut PatchLine<'_>, new: &mut PatchLine<'_>) {
    let (prefix, suffix) = common_ends(old.text, new.text);
    let before = &old.text[prefix..old.text.len() - suffix];
    let after = &new.text[prefix..new.text.len() - suffix];
    let tabs_or_spaces = [before, after].iter().all(|s| s.bytes().all(|b| matches!(b, b'\t' | b' ')))
        && (before.contains('\t') || after.contains('\t'));
    let ending_changed = old.ending != new.ending;
    if (prefix + suffix) * 2 < old.text.len().max(new.text.len()) && !tabs_or_spaces {
        // Tiny edits can have few shared bytes when an accented character is
        // rewritten as a base plus a combining mark.
        let tiny_mark_edit = (before.chars().any(hidden) || after.chars().any(hidden))
            && [before, after].iter().all(|s| s.chars().filter(|&c| !hidden(c)).nth(1).is_none());
        if !tiny_mark_edit {
            return;
        }
    }
    old.changed = prefix..old.text.len() - suffix;
    new.changed = prefix..new.text.len() - suffix;
    annotate(old, tabs_or_spaces, ending_changed);
    annotate(new, tabs_or_spaces, ending_changed);
    if before.chars().next().is_some_and(hidden) || after.chars().next().is_some_and(hidden) {
        include_base(old.text, &mut old.changed);
        include_base(new.text, &mut new.changed);
    }
}

/// A run of removals followed by the same number of additions can pair up.
fn finish_edits(edits: &mut Vec<PatchLine<'_>>, lines: &mut Vec<Line>) {
    let added_at = edits.iter().position(|l| l.kind == LineKind::Add).unwrap_or(edits.len());
    let (old, new) = edits.split_at_mut(added_at);
    if old.len() == new.len() {
        for (old, new) in old.iter_mut().zip(new) {
            mark_edit(old, new);
        }
    }
    lines.extend(edits.drain(..).map(|l| Line::edited(l.kind, l.number, l.text, l.changed, l.annotation)));
}

/// What happened to the file and the lines that describe it.
pub struct FileDiff {
    pub lines: Vec<Line>,
    pub kind: Kind,
    pub binary: bool,
}

/// Parses the file's status and each line, leaving removals in place.
pub fn parse(raw: &[u8]) -> FileDiff {
    let mut lines = Vec::new();
    let mut kind = Kind::None;
    let text = String::from_utf8_lossy(raw);
    let nontext = raw.contains(&0) || matches!(text, Cow::Owned(_));
    let mut binary = nontext;
    let (mut old, mut new, mut in_hunk, mut second) = (1, 1, false, false);
    let mut edits = Vec::new();
    // Git's LF separates patch lines; a preceding CR may belong to the file.
    let mut patch = text.split_terminator('\n').peekable();
    while let Some(line) = patch.next() {
        if line.starts_with("diff --git ") {
            finish_edits(&mut edits, &mut lines);
            // Git shows a change of file type as a deletion and an addition.
            (in_hunk, second) = (false, kind != Kind::None);
            kind = kind.max(Kind::Modified);
        } else if let Some(hunk) = line.strip_prefix("@@ -") {
            finish_edits(&mut edits, &mut lines);
            // "@@ -12,5 +14,6 @@"
            let start = |s: &str| s.split([',', ' ']).next().and_then(|n| n.parse().ok()).unwrap_or(1);
            old = start(hunk);
            new = hunk.split_once(" +").map_or(1, |(_, n)| start(n));
            in_hunk = true;
        } else if !in_hunk {
            if line.starts_with("new file mode ") {
                kind = if second { Kind::Modified } else { Kind::Added };
            } else if line.starts_with("deleted file mode ") {
                kind = Kind::Deleted;
            } else if line.starts_with("rename from ") {
                kind = Kind::Renamed;
            } else if line.starts_with("Binary files ") {
                binary = true;
            }
        } else if !nontext {
            let no_newline = patch.peek().is_some_and(|line| line.starts_with('\\'));
            let text = line.get(1..).unwrap_or("");
            let (text, ending) = if no_newline {
                patch.next();
                (text, Ending::None)
            } else if let Some(text) = text.strip_suffix('\r') {
                (text, Ending::CrLf)
            } else {
                (text, Ending::Lf)
            };
            let (kind, number) = match line.as_bytes().first() {
                Some(b'+') => (LineKind::Add, &mut new),
                Some(b'-') => (LineKind::Del, &mut old),
                Some(b'\\') => continue, // "\ No newline at end of file"
                _ => {
                    finish_edits(&mut edits, &mut lines);
                    lines.push(Line::new(LineKind::Same, new, text));
                    old += 1;
                    new += 1;
                    continue;
                }
            };
            if edits.is_empty() && kind == LineKind::Add {
                lines.push(Line::new(kind, *number, text));
                *number += 1;
                continue;
            }
            edits.push(PatchLine { kind, number: *number, text, ending, changed: 0..0, annotation: String::new() });
            *number += 1;
        }
    }
    finish_edits(&mut edits, &mut lines);
    FileDiff { lines, kind, binary }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(lines: &[Line]) -> Vec<(LineKind, u32, &str)> {
        lines.iter().map(|l| (l.kind, l.number, l.text.as_str())).collect()
    }

    #[test]
    fn a_diff_becomes_numbered_lines() {
        let FileDiff { lines, kind, .. } = parse(
            b"diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,4 @@\n one\n-two\n+2\n+2.5\n three\n",
        );
        assert_eq!(kind, Kind::Modified);
        assert_eq!(
            shape(&lines),
            [
                (LineKind::Same, 1, "one"),
                (LineKind::Del, 2, "two"),
                (LineKind::Add, 2, "2"),
                (LineKind::Add, 3, "2.5"),
                (LineKind::Same, 4, "three"),
            ]
        );
    }

    #[test]
    fn a_diff_says_what_became_of_the_file() {
        let new = b"diff --git a/f b/f\nnew file mode 100644\n--- /dev/null\n+++ b/f\n@@ -0,0 +1 @@\n+x\n";
        assert_eq!(parse(new).kind, Kind::Added);
        let gone = b"diff --git a/f b/f\ndeleted file mode 100644\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        assert_eq!(parse(gone).kind, Kind::Deleted);
        let moved = b"diff --git a/f b/g\nsimilarity index 90%\nrename from f\nrename to g\n@@ -1 +1 @@\n-x\n+y\n";
        assert_eq!(parse(moved).kind, Kind::Renamed);
        assert_eq!(parse(b"").kind, Kind::None);
        let mode = b"diff --git a/f b/f\nold mode 100644\nnew mode 100755\n";
        assert_eq!(parse(mode).kind, Kind::Modified);
        let swapped = b"diff --git a/f b/f\ndeleted file mode 100644\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n\
                        diff --git a/f b/f\nnew file mode 120000\n--- /dev/null\n+++ b/f\n@@ -0,0 +1 @@\n+t\n";
        assert_eq!(parse(swapped).kind, Kind::Modified);
    }

    #[test]
    fn a_file_type_change_does_not_mix_diff_headers_into_the_text() {
        let raw = b"diff --git a/f b/f\ndeleted file mode 100644\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-text\n\
                    diff --git a/f b/f\nnew file mode 120000\n--- /dev/null\n+++ b/f\n@@ -0,0 +1 @@\n+target\n";
        assert_eq!(shape(&parse(raw).lines), [(LineKind::Del, 1, "text"), (LineKind::Add, 1, "target")]);
    }

    #[test]
    fn an_edited_word_is_picked_out() {
        let lines = parse(b"diff --git a/f b/f\n@@ -1 +1 @@\n-let total = count + 1;\n+let total = count + 2;\n").lines;
        assert_eq!(&lines[0].text[lines[0].changed.clone()], "1");
        assert_eq!(&lines[1].text[lines[1].changed.clone()], "2");
    }

    #[test]
    fn an_edit_range_includes_whole_multibyte_characters() {
        let lines = parse("diff --git a/f b/f\n@@ -1 +1 @@\n-色 is a colour\n+红 is a colour\n".as_bytes()).lines;
        assert_eq!(&lines[0].text[lines[0].changed.clone()], "色");
        assert_eq!(&lines[1].text[lines[1].changed.clone()], "红");
    }

    #[test]
    fn tabs_are_explained_without_changing_the_columns_of_the_source() {
        let lines = parse("@@ -1 +1 @@\n-界\tX\tlater\n+界  X\tlater\n".as_bytes()).lines;
        assert_eq!(lines[0].text, lines[1].text);
        assert_eq!(lines[0].annotation, " [tab]");
        assert_eq!(lines[1].annotation, " [2 spaces]");
        for line in &lines {
            assert_eq!(&line.text[line.changed.clone()], "  ");
        }

        let indent_only = parse(b"@@ -1 +1 @@\n-\t\n+    \n").lines;
        assert_eq!(indent_only[0].annotation, " [tab]");
        assert_eq!(indent_only[1].annotation, " [4 spaces]");
    }

    #[test]
    fn line_endings_are_explained_only_when_they_change() {
        let lines = parse(b"@@ -1,2 +1,2 @@\n context\r\n-copy\r\n+copy\n").lines;
        assert_eq!(
            shape(&lines),
            [(LineKind::Same, 1, "context"), (LineKind::Del, 2, "copy"), (LineKind::Add, 2, "copy")]
        );
        assert!(lines[0].annotation.is_empty());
        assert_eq!(lines[1].annotation, " [CRLF]");
        assert_eq!(lines[2].annotation, " [LF]");

        let lines = parse(b"@@ -1 +1 @@\n-copy\r\n\\ No newline at end of file\n+copy\r\n").lines;
        assert_eq!(lines[0].text, lines[1].text);
        assert_eq!(lines[0].annotation, " [carriage return U+000D] [no newline]");
        assert_eq!(lines[1].annotation, " [CRLF]");
    }

    #[test]
    fn changed_zero_width_marks_name_the_character_and_include_the_base_on_both_sides() {
        for (before, after, old_edit, new_edit, label) in [
            ("ab", "a\u{200b}b", "a", "a\u{200b}", " [zero-width space U+200B]"),
            ("a", "\u{200b}a", "a", "\u{200b}a", " [zero-width space U+200B]"),
            ("界x", "界\u{301}x", "界", "界\u{301}", " [combining acute U+0301]"),
            ("é", "e\u{301}", "é", "e\u{301}", " [combining acute U+0301]"),
            ("e", "e\u{301}\u{301}", "e", "e\u{301}\u{301}", " [combining acute U+0301 x2]"),
        ] {
            let lines = parse(format!("@@ -1 +1 @@\n-{before}\n+{after}\n").as_bytes()).lines;
            assert_eq!(&lines[0].text[lines[0].changed.clone()], old_edit);
            assert_eq!(&lines[1].text[lines[1].changed.clone()], new_edit);
            assert!(lines[0].annotation.is_empty());
            assert_eq!(lines[1].annotation, label);
        }
    }

    #[test]
    fn ordinary_edits_do_not_label_unchanged_tabs_marks_or_line_endings() {
        let lines = parse("@@ -1 +1 @@\n-\tcafe\u{301} = 1\r\n+\tcafe\u{301} = 2\r\n".as_bytes()).lines;
        for (line, changed) in lines.iter().zip(["1", "2"]) {
            assert!(line.annotation.is_empty());
            assert_eq!(&line.text[line.changed.clone()], changed);
        }
    }

    #[test]
    fn unrelated_lines_are_not_paired() {
        let lines = parse(b"diff --git a/f b/f\n@@ -1 +1 @@\n-alpha\n+zzzzzzzzzz\n").lines;
        assert_eq!(lines[0].changed, 0..0);
    }

    #[test]
    fn text_is_made_safe_to_draw() {
        assert_eq!(Line::new(LineKind::Same, 1, "a\tb\x1b[31m").text, "a   b[31m");
    }

    #[test]
    fn tabs_align_after_wide_characters_and_combining_marks() {
        for (source, text) in [("界\tX", "界  X"), ("e\u{301}\tX", "e\u{301}   X")] {
            let line = Line::new(LineKind::Same, 1, source);
            assert_eq!((line.text.as_str(), line.width), (text, 5));
        }
    }
}
