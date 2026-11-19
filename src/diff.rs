//! Parses whole-file diffs and marks the bytes that differ in paired lines.

use std::borrow::Cow;
use std::ops::Range;

use crate::git::Kind;
use crate::width::char_width;

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
    /// Display width, measured once so long lines need not be measured on every draw.
    pub width: usize,
    /// Bytes that differ from the paired line, empty if none.
    pub changed: Range<usize>,
}

impl Line {
    /// Makes a line with tabs expanded and control characters left out.
    pub fn new(kind: LineKind, number: u32, text: &str) -> Self {
        let (text, width) = clean(text);
        Self { kind, number, text, width, changed: 0..0 }
    }
}

/// Expands tabs and drops control characters before comparing or drawing text.
fn clean(line: &str) -> (String, usize) {
    if line.bytes().all(|b| matches!(b, b' '..=b'~')) {
        return (line.to_owned(), line.len());
    }
    let mut s = String::with_capacity(line.len());
    let mut col = 0;
    for c in line.chars() {
        if c == '\t' {
            let pad = TAB - col % TAB;
            s.extend(std::iter::repeat_n(' ', pad));
            col += pad;
        } else if !c.is_control() {
            s.push(c);
            col += char_width(c);
        }
    }
    (s, col)
}

/// Bytes two strings share at the start and the end, without overlap.
fn common_ends(a: &str, b: &str) -> (usize, usize) {
    let prefix: usize = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    let (a, b) = (&a[prefix..], &b[prefix..]);
    let suffix = a.chars().rev().zip(b.chars().rev()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    (prefix, suffix)
}

/// Looks for small edits where consecutive removals and additions pair up.
fn mark_edits(mut lines: &mut [Line]) {
    while let Some(start) = lines.iter().position(|l| l.kind == LineKind::Del) {
        lines = &mut lines[start..];
        let removed = lines.iter().take_while(|l| l.kind == LineKind::Del).count();
        let (old, rest) = lines.split_at_mut(removed);
        let added = rest.iter().take_while(|l| l.kind == LineKind::Add).count();
        let (new, remaining) = rest.split_at_mut(added);
        if removed == added {
            for (old, new) in old.iter_mut().zip(new) {
                let (prefix, suffix) = common_ends(&old.text, &new.text);
                // Lines that share less than half are different lines, not edits.
                if (prefix + suffix) * 2 >= old.text.len().max(new.text.len()) {
                    old.changed = prefix..old.text.len() - suffix;
                    new.changed = prefix..new.text.len() - suffix;
                }
            }
        }
        lines = remaining;
    }
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
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            // Git shows a change of file type as a deletion and an addition.
            (in_hunk, second) = (false, kind != Kind::None);
            kind = kind.max(Kind::Modified);
        } else if let Some(hunk) = line.strip_prefix("@@ -") {
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
            let (kind, number) = match line.as_bytes().first() {
                Some(b'+') => (LineKind::Add, &mut new),
                Some(b'-') => (LineKind::Del, &mut old),
                Some(b'\\') => continue, // "\ No newline at end of file"
                _ => {
                    old += 1;
                    (LineKind::Same, &mut new)
                }
            };
            lines.push(Line::new(kind, *number, line.get(1..).unwrap_or("")));
            *number += 1;
        }
    }
    mark_edits(&mut lines);
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
    fn unrelated_lines_are_not_paired() {
        let lines = parse(b"diff --git a/f b/f\n@@ -1 +1 @@\n-alpha\n+zzzzzzzzzz\n").lines;
        assert_eq!(lines[0].changed, 0..0);
    }

    #[test]
    fn text_is_made_safe_to_draw() {
        assert_eq!(clean("a\tb\x1b[31m").0, "a   b[31m");
    }

    #[test]
    fn tabs_align_after_wide_characters_and_combining_marks() {
        for (source, text) in [("界\tX", "界  X"), ("e\u{301}\tX", "e\u{301}   X")] {
            let line = Line::new(LineKind::Same, 1, source);
            assert_eq!((line.text.as_str(), line.width), (text, 5));
        }
    }
}
