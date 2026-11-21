//! Parses whole-file diffs and marks the words that differ in paired lines.

mod words;

use std::borrow::Cow;
use std::ops::Range;

use crate::git::Kind;
use crate::width::{CellWidth, cell_width, char_width};
use words::Words;

/// Columns between tab stops wherever a file's text is shown.
pub(crate) const TAB: usize = 4;

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
    /// Display bytes to emphasize, in order, including the base of any changed mark.
    pub changed: Box<[Range<usize>]>,
    /// Visible explanations for changed characters that are hard to see in the source text.
    pub annotation: String,
}

impl Line {
    /// Makes a line with tabs expanded and control characters left out.
    pub fn new(kind: LineKind, number: u32, text: &str) -> Self {
        Self::edited(kind, number, text, Vec::new(), String::new())
    }

    fn edited(kind: LineKind, number: u32, text: &str, mut changed: Vec<Range<usize>>, annotation: String) -> Self {
        let (text, width) = clean(text, &mut changed);
        Self { kind, number, text, width, changed: changed.into_boxed_slice(), annotation }
    }

    pub fn display_width(&self) -> usize {
        self.width + self.annotation.len() // Annotations are ASCII.
    }
}

/// Expands tabs and drops control characters, translating the edits to display bytes.
fn clean(line: &str, changed: &mut Vec<Range<usize>>) -> (String, usize) {
    if line.bytes().all(|b| matches!(b, b' '..=b'~')) {
        return (line.to_owned(), line.len());
    }
    let mut s = String::with_capacity(line.len());
    let mut col = 0;
    // The ranges are in order, so their ends are met in order too.
    let mut ends = changed.iter_mut().flat_map(|range| [&mut range.start, &mut range.end]).peekable();
    for (at, c) in line.char_indices() {
        while let Some(end) = ends.next_if(|end| **end <= at) {
            *end = s.len();
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
    for end in ends {
        *end = s.len();
    }
    changed.retain(|range| !range.is_empty());
    (s, col)
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
    changed: Vec<Range<usize>>,
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

/// Says which of a changed tab and spaces a line has, since both draw as blank columns.
fn label_indent(annotation: &mut String, edit: &str) {
    use std::fmt::Write as _;

    let tabs = edit.bytes().filter(|&b| b == b'\t').count();
    let spaces = edit.len() - tabs;
    for (n, name) in [(tabs, "tab"), (spaces, "space")] {
        if n == 1 {
            let _ = write!(annotation, " [{name}]");
        } else if n > 1 {
            let _ = write!(annotation, " [{n} {name}s]");
        }
    }
}

/// Names the hidden characters in the changed part of a line.
fn label_marks(annotation: &mut String, edit: &str) {
    use std::fmt::Write as _;

    let mut marks = edit.chars().filter(|&c| hidden(c)).peekable();
    while let Some(c) = marks.next() {
        let mut count = 1;
        while marks.next_if_eq(&c).is_some() {
            count += 1;
        }
        let _ = write!(annotation, " [{} U+{:04X}", mark_label(c), u32::from(c));
        if count > 1 {
            let _ = write!(annotation, " x{count}");
        }
        annotation.push(']');
    }
}

/// Adds a range to emphasise after those before it, joining ranges that touch.
fn emphasise(changed: &mut Vec<Range<usize>>, range: Range<usize>) {
    match changed.last_mut() {
        _ if range.is_empty() => {}
        Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
        _ => changed.push(range),
    }
}

/// Whether an edit only swaps tabs and spaces, which draw as the same blank columns.
fn swaps_indent(before: &str, after: &str) -> bool {
    [before, after].iter().all(|s| s.bytes().all(|b| matches!(b, b'\t' | b' ')))
        && (before.contains('\t') || after.contains('\t'))
}

/// Emphasises each run of changed words and labels what cannot be seen in it: a swap of
/// tabs and spaces, or hidden characters that changed.
fn mark_words<'a>(old: &mut PatchLine<'a>, new: &mut PatchLine<'a>, words: &Words) {
    for (old_run, new_run) in words.runs() {
        let (head, tail) = common_ends(&old.text[old_run.clone()], &new.text[new_run.clone()]);
        let edits = [old_run.start + head..old_run.end - tail, new_run.start + head..new_run.end - tail];
        let [before, after] = [&old.text[edits[0].clone()], &new.text[edits[1].clone()]];
        let indent = swaps_indent(before, after);
        // A change made only of marks is shown on the character it affects, not on the whole word.
        let marks_only = [before, after].iter().all(|s| s.chars().all(hidden));
        for ((line, run), mut edit) in [&mut *old, &mut *new].into_iter().zip([old_run, new_run]).zip(edits) {
            let label = if indent { label_indent } else { label_marks };
            label(&mut line.annotation, &line.text[edit.clone()]);
            if marks_only {
                include_base(line.text, &mut edit);
            }
            emphasise(&mut line.changed, if marks_only || indent { edit } else { run });
        }
    }
}

/// Marks what changed between a removed line and the added line paired with it.
/// Lines that share too little, or are too long to compare by word, keep only their line colour.
fn mark_edit<'a>(old: &mut PatchLine<'a>, new: &mut PatchLine<'a>, words: &mut Words) {
    let (prefix, suffix) = common_ends(old.text, new.text);
    let edits = [prefix..old.text.len() - suffix, prefix..new.text.len() - suffix];
    let [before, after] = [&old.text[edits[0].clone()], &new.text[edits[1].clone()]];
    let longest = old.text.len().max(new.text.len());
    if swaps_indent(before, after) {
        for (line, edit) in [&mut *old, &mut *new].into_iter().zip(edits) {
            label_indent(&mut line.annotation, &line.text[edit.clone()]);
            emphasise(&mut line.changed, edit);
        }
    } else {
        // Tiny edits can have few shared bytes when an accented character is
        // rewritten as a base plus a combining mark.
        let tiny_mark_edit = || {
            (before.chars().any(hidden) || after.chars().any(hidden))
                && [before, after].iter().all(|s| s.chars().filter(|&c| !hidden(c)).nth(1).is_none())
        };
        // Shared ends count even inside a word. Without them, whole words must keep half of the longer line.
        let needed = if (prefix + suffix) * 2 >= longest || tiny_mark_edit() { 0 } else { longest.div_ceil(2) };
        match words.compare(old.text, new.text, prefix, suffix, needed) {
            Some(kept) if kept >= needed => mark_words(old, new, words),
            // Past the limits there are no words to emphasise, but lines with enough
            // shared ends still name the hidden characters between them.
            None if needed == 0 => {
                label_marks(&mut old.annotation, before);
                label_marks(&mut new.annotation, after);
            }
            _ => return,
        }
    }
    if old.ending != new.ending {
        old.annotation.push_str(old.ending.label());
        new.annotation.push_str(new.ending.label());
    }
}

/// A run of removals followed by the same number of additions can pair up.
fn finish_edits(edits: &mut Vec<PatchLine<'_>>, lines: &mut Vec<Line>, words: &mut Words) {
    let added_at = edits.iter().position(|l| l.kind == LineKind::Add).unwrap_or(edits.len());
    let (old, new) = edits.split_at_mut(added_at);
    if old.len() == new.len() {
        for (old, new) in old.iter_mut().zip(new) {
            mark_edit(old, new, words);
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
    let mut words = Words::new();
    // Git's LF separates patch lines; a preceding CR may belong to the file.
    let mut patch = text.split_terminator('\n').peekable();
    while let Some(line) = patch.next() {
        if line.starts_with("diff --git ") {
            finish_edits(&mut edits, &mut lines, &mut words);
            // Git shows a change of file type as a deletion and an addition.
            (in_hunk, second) = (false, kind != Kind::None);
            kind = kind.max(Kind::Modified);
        } else if let Some(hunk) = line.strip_prefix("@@ -") {
            finish_edits(&mut edits, &mut lines, &mut words);
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
                    finish_edits(&mut edits, &mut lines, &mut words);
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
            edits.push(PatchLine {
                kind,
                number: *number,
                text,
                ending,
                changed: Vec::new(),
                annotation: String::new(),
            });
            *number += 1;
        }
    }
    finish_edits(&mut edits, &mut lines, &mut words);
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

    /// The pieces of a line that are emphasised.
    fn changed(line: &Line) -> Vec<&str> {
        line.changed.iter().map(|range| &line.text[range.clone()]).collect()
    }

    fn pair(old: &str, new: &str) -> Vec<Line> {
        parse(format!("@@ -1 +1 @@\n-{old}\n+{new}\n").as_bytes()).lines
    }

    #[test]
    fn an_edited_word_is_picked_out() {
        let lines = parse(b"diff --git a/f b/f\n@@ -1 +1 @@\n-let total = count + 1;\n+let total = count + 2;\n").lines;
        assert_eq!(changed(&lines[0]), ["1"]);
        assert_eq!(changed(&lines[1]), ["2"]);
    }

    #[test]
    fn each_edited_word_is_picked_out_and_the_words_between_are_not() {
        let lines = pair("    let total = count(items) + 1; // sum", "    let sum = count(rows) + 2; // sum");
        assert_eq!(changed(&lines[0]), ["total", "items", "1"]);
        assert_eq!(changed(&lines[1]), ["sum", "rows", "2"]);

        // A word is emphasised whole, and added or removed words have nothing opposite them.
        let lines = pair("configure_logging_v1(level)", "configure_logging_v2(level, sink)");
        assert_eq!(changed(&lines[0]), ["configure_logging_v1"]);
        assert_eq!(changed(&lines[1]), ["configure_logging_v2", ", sink"]);
    }

    #[test]
    fn an_edit_range_includes_whole_multibyte_characters() {
        let lines = parse("diff --git a/f b/f\n@@ -1 +1 @@\n-色 is a colour\n+红 is a colour\n".as_bytes()).lines;
        assert_eq!(changed(&lines[0]), ["色"]);
        assert_eq!(changed(&lines[1]), ["红"]);
    }

    #[test]
    fn edited_words_are_found_in_accented_wide_and_joined_text() {
        let lines = pair("größe = länge * 2 # naïve Schätzung", "größe = breite * 2 # naïve Prüfung");
        assert_eq!(changed(&lines[0]), ["länge", "Schätzung"]);
        assert_eq!(changed(&lines[1]), ["breite", "Prüfung"]);

        let lines = pair("名前は太郎で、年齢は二十歳です", "名前は花子で、年齢は三十歳です");
        assert_eq!(changed(&lines[0]), ["太郎", "二"]);
        assert_eq!(changed(&lines[1]), ["花子", "三"]);

        let lines =
            pair("ship it with 👩\u{200d}💻 and cafe\u{301} today", "ship it with 👩\u{200d}🚀 and cafe\u{301} soon");
        assert_eq!(changed(&lines[0]), ["👩\u{200d}💻", "today"]);
        assert_eq!(changed(&lines[1]), ["👩\u{200d}🚀", "soon"]);
        assert!(lines.iter().all(|line| line.annotation.is_empty()));
    }

    #[test]
    fn a_changed_mark_beside_an_edited_word_still_shows_on_its_own_character() {
        let lines = pair("let hidden = 1; // note", "let hidden\u{200b} = 2; // note");
        assert_eq!(changed(&lines[0]), ["n", "1"]);
        assert_eq!(changed(&lines[1]), ["n\u{200b}", "2"]);
        assert!(lines[0].annotation.is_empty());
        assert_eq!(lines[1].annotation, " [zero-width space U+200B]");
    }

    #[test]
    fn emphasis_follows_words_through_expanded_tabs_and_dropped_controls() {
        let lines = pair("\tlet a\t= 1;\x07\tend", "\tlet b\t= 2;\x07\tend");
        assert_eq!(lines[0].text, "    let a   = 1;    end");
        assert_eq!(changed(&lines[0]), ["a", "1"]);
        assert_eq!(changed(&lines[1]), ["b", "2"]);
    }

    #[test]
    fn long_or_heavily_edited_pairs_keep_only_their_line_colour() {
        // One edit in a long line is still found.
        let long = "word ".repeat(40_000);
        let lines = pair(&format!("{long}old {long}"), &format!("{long}new {long}"));
        assert_eq!(changed(&lines[0]), ["old"]);
        assert_eq!(changed(&lines[1]), ["new"]);

        // Edits at both ends of it leave too many words between them to compare.
        let lines = pair(&format!("old {long}old"), &format!("new {long}new"));
        assert!(lines.iter().all(|line| line.changed.is_empty()));

        // So do lines that share only their shape.
        let numbers = |from: u32| (from..from + 40).map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
        for (old, new) in [(numbers(100), numbers(200)), ("return None;".into(), "let x = 1;".into())] {
            let lines = pair(&old, &new);
            assert!(lines.iter().all(|line| line.changed.is_empty()), "{old}");
            assert_eq!((lines[0].text.as_str(), lines[1].text.as_str()), (old.as_str(), new.as_str()));
        }
    }

    #[test]
    fn a_pair_past_the_limits_still_explains_hidden_characters_and_endings() {
        // Seventeen replaced words are more than a pair may have, but the long shared start keeps the lines paired.
        let start = "x".repeat(200);
        let words = |word: &str| (0..17).map(|n| format!("{word}{n}")).collect::<Vec<_>>().join(" ");
        let (old, new) = (format!("{start} {}", words("a")), format!("{start} {}", words("b\u{200b}")));
        let lines = parse(format!("@@ -1 +1 @@\n-{old}\r\n+{new}\n").as_bytes()).lines;
        assert!(lines.iter().all(|line| line.changed.is_empty()));
        assert_eq!(lines[0].annotation, " [CRLF]");
        assert_eq!(lines[1].annotation, " [zero-width space U+200B x17] [LF]");
    }

    #[test]
    fn a_tab_swapped_for_spaces_beside_another_edit_is_explained_too() {
        let lines = pair("\ttotal = count + 1", "    total = count + 2");
        assert_eq!(lines[0].text, "    total = count + 1");
        assert_eq!((changed(&lines[0]), lines[0].annotation.as_str()), (vec!["    ", "1"], " [tab]"));
        assert_eq!((changed(&lines[1]), lines[1].annotation.as_str()), (vec!["    ", "2"], " [4 spaces]"));
    }

    #[test]
    fn emphasis_never_changes_the_text_that_is_shown() {
        let texts = [
            "",
            "plain = 1;",
            "plain = 2; // more",
            "\ttabs\tand  spaces ",
            "    tabs\tand spaces",
            "cafe\u{301} 変更 👩\u{200d}💻\u{fe0f} ok\r",
            "café 変化 👩\u{200d}🚀 ok",
            "\u{feff}\u{200b}marks\u{200d} \x1b[31mred",
            "marks \x1b[32mgreen\u{ad}",
        ];
        for old in texts {
            for new in texts {
                for ending in ["\n", "\r\n", "\n\\ No newline at end of file\n"] {
                    let lines = parse(format!("@@ -1 +1 @@\n-{old}\n+{new}{ending}").as_bytes()).lines;
                    for (line, source) in lines.iter().zip([old, new]) {
                        // Printable text is Git's, byte for byte. Tabs and controls are replaced
                        // exactly as they are on a line that has no edits.
                        if !source.chars().any(char::is_control) {
                            assert_eq!(line.text, source, "{old:?} {new:?}");
                        }
                        assert_eq!(line.text, Line::new(line.kind, 1, source).text, "{old:?} {new:?}");
                        let mut at = 0;
                        for range in &line.changed {
                            assert!(at <= range.start && range.start < range.end, "{old:?} {new:?}");
                            assert!(line.text.is_char_boundary(range.start) && line.text.is_char_boundary(range.end));
                            at = range.end;
                        }
                        assert!(at <= line.text.len());
                    }
                }
            }
        }
    }

    #[test]
    fn tabs_are_explained_without_changing_the_columns_of_the_source() {
        let lines = parse("@@ -1 +1 @@\n-界\tX\tlater\n+界  X\tlater\n".as_bytes()).lines;
        assert_eq!(lines[0].text, lines[1].text);
        assert_eq!(lines[0].annotation, " [tab]");
        assert_eq!(lines[1].annotation, " [2 spaces]");
        for line in &lines {
            assert_eq!(changed(line), ["  "]);
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
            let lines = pair(before, after);
            assert_eq!(changed(&lines[0]), [old_edit]);
            assert_eq!(changed(&lines[1]), [new_edit]);
            assert!(lines[0].annotation.is_empty());
            assert_eq!(lines[1].annotation, label);
        }
    }

    #[test]
    fn a_control_only_edit_has_a_label_but_no_empty_emphasis_range() {
        let lines = pair("\x07", "\x08");
        for (line, label) in lines.iter().zip([" [control U+0007]", " [control U+0008]"]) {
            assert!(line.text.is_empty() && line.changed.is_empty());
            assert_eq!(line.annotation, label);
        }
    }

    #[test]
    fn ordinary_edits_do_not_label_unchanged_tabs_marks_or_line_endings() {
        let lines = parse("@@ -1 +1 @@\n-\tcafe\u{301} = 1\r\n+\tcafe\u{301} = 2\r\n".as_bytes()).lines;
        for (line, edit) in lines.iter().zip(["1", "2"]) {
            assert!(line.annotation.is_empty());
            assert_eq!(changed(line), [edit]);
        }
    }

    #[test]
    fn unrelated_lines_are_not_paired() {
        let lines = parse(b"diff --git a/f b/f\n@@ -1 +1 @@\n-alpha\n+zzzzzzzzzz\n").lines;
        assert!(lines.iter().all(|line| line.changed.is_empty()));
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
