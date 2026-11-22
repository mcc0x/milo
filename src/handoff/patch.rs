//! Reads the files and hunks of a unified diff, as `git diff` or `diff -u` writes one.

use std::iter::Peekable;
use std::path::{Component, Path};
use std::str::SplitTerminator;

pub(super) const BINARY: &str = "The patch says this file is not text.";
pub(super) const MALFORMED: &str = "The patch for this file cannot be read.";
pub(super) const NOT_INSIDE: &str = "The patch names a path that is not a file inside the project.";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mark {
    Same,
    Add,
    Del,
}

/// One block of neighbouring lines, as the patch says they were and became.
#[derive(Debug)]
pub(super) struct Hunk<'a> {
    /// Lines the patch says come before this block once it is applied.
    pub(super) at: usize,
    pub(super) lines: Vec<(Mark, &'a str)>,
}

/// What one patch says it did to one file. A patch made edit by edit can
/// hold several sections for a file, in the order the edits were made.
#[derive(Debug)]
pub(super) struct Section<'a> {
    /// The file's name once the patch is applied, or before it for a removed file.
    pub(super) path: &'a str,
    pub(super) removes_file: bool,
    pub(super) hunks: Vec<Hunk<'a>>,
    /// Why nothing of this section can be shown.
    pub(super) problem: Option<&'static str>,
}

type Lines<'a> = Peekable<SplitTerminator<'a, char>>;

/// Finds every file the patch names. Text between the files is ignored, as `patch` ignores it.
pub(super) fn parse(text: &str) -> Vec<Section<'_>> {
    let mut sections = Vec::new();
    // Git names a file it gives no lines for, such as a picture, only here.
    let mut git_name = None;
    // A CR before the LF stays with its line: it may belong to the file.
    let mut lines = text.split_terminator('\n').peekable();
    while let Some(line) = lines.next() {
        if let Some(names) = line.strip_prefix("diff --git ") {
            git_name = git_path(names);
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            if let Some(path) = git_name.take() {
                let problem = Some(if safe(path) { BINARY } else { NOT_INSIDE });
                sections.push(Section { path, removes_file: false, hunks: Vec::new(), problem });
            }
        } else if let Some(old) = line.strip_prefix("--- ")
            && let Some(new) = lines.peek().and_then(|next| next.strip_prefix("+++ "))
        {
            lines.next();
            git_name = None;
            let mut section = named(old, new);
            while let Some(header) = lines.peek().and_then(|next| next.strip_prefix("@@ -")) {
                lines.next();
                match hunk(header, &mut lines) {
                    Some(hunk) => section.hunks.push(hunk),
                    None => {
                        section.problem.get_or_insert(MALFORMED);
                        break;
                    }
                }
            }
            // Lines past what a hunk's header counted would otherwise be left out without a word.
            let spills = lines.peek().is_some_and(|next| match next.as_bytes().first() {
                Some(b' ' | b'+') => true,
                Some(b'-') => !next.starts_with("--- ") && *next != "-- ",
                _ => false,
            });
            if section.hunks.is_empty() || spills {
                section.problem.get_or_insert(MALFORMED);
            }
            sections.push(section);
        }
    }
    sections
}

/// The one path in `a/PATH b/PATH`.
fn git_path(names: &str) -> Option<&str> {
    let middle = names.len() / 2;
    if names.len().is_multiple_of(2) || names.as_bytes()[middle] != b' ' {
        return None;
    }
    let old = names[..middle].strip_prefix("a/")?;
    (names[middle + 1..].strip_prefix("b/") == Some(old)).then_some(old)
}

fn named<'a>(old: &'a str, new: &'a str) -> Section<'a> {
    const NO_FILE: &str = "/dev/null";
    // `diff -u` writes a tab and a date after the name.
    let name = |side: &'a str| side.split('\t').next().unwrap_or(side);
    let (old, new) = (name(old), name(new));
    let prefixed = (old == NO_FILE || old.starts_with("a/")) && (new == NO_FILE || new.starts_with("b/"));
    let removes_file = new == NO_FILE;
    let path = match (removes_file, prefixed) {
        (true, true) => old.strip_prefix("a/").unwrap_or(old),
        (true, false) => old,
        (false, true) => &new[2..],
        (false, false) => new,
    };
    let problem = (!safe(path)).then_some(NOT_INSIDE);
    Section { path, removes_file, hunks: Vec::new(), problem }
}

/// Whether the name can only mean a file below the project folder, and none of Git's own.
/// A name Git quoted, because of an unusual character in it, is refused with the rest.
fn safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('"')
        && !path.ends_with('/')
        && !path.chars().any(char::is_control)
        && Path::new(path).components().all(|part| match part {
            Component::Normal(name) => !name.eq_ignore_ascii_case(".git"),
            _ => false,
        })
}

/// Reads the lines of the hunk whose header, after `@@ -`, is `12,5 +14,6 @@`.
fn hunk<'a>(header: &str, lines: &mut Lines<'a>) -> Option<Hunk<'a>> {
    let (old, rest) = header.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let range = |text: &str| -> Option<(usize, usize)> {
        let (start, count) = text.split_once(',').unwrap_or((text, "1"));
        Some((start.parse().ok()?, count.parse().ok()?))
    };
    let ((_, mut old_left), (new_start, mut new_left)) = (range(old)?, range(new)?);
    // With no lines of its own on that side, a hunk gives the line it follows.
    let at = if new_left == 0 { new_start } else { new_start.checked_sub(1)? };
    // The header's counts are a claim: lines are kept as they are found, not room for the claim.
    let mut found = Vec::new();
    while old_left > 0 || new_left > 0 {
        let line = lines.next()?;
        let (mark, text) = match line.as_bytes().first() {
            // Some programs drop the space that marks an empty unchanged line.
            None => (Mark::Same, ""),
            Some(b' ') => (Mark::Same, &line[1..]),
            Some(b'+') => (Mark::Add, &line[1..]),
            Some(b'-') => (Mark::Del, &line[1..]),
            Some(b'\\') => continue,
            Some(_) => return None,
        };
        if mark != Mark::Add {
            old_left = old_left.checked_sub(1)?;
        }
        if mark != Mark::Del {
            new_left = new_left.checked_sub(1)?;
        }
        found.push((mark, text));
    }
    // "\ No newline at end of file" after the last line.
    lines.next_if(|line| line.starts_with('\\'));
    Some(Hunk { at, lines: found })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_and_plain_diffs_name_the_same_file_and_lines() {
        let git = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -3,3 +3,3 @@ fn x()\n one\n-two\n+2\n three\n";
        let plain = "--- src/a.rs\t2026-01-01 10:00:00\n+++ src/a.rs\t2026-01-01 10:00:01\n@@ -3,3 +3,3 @@\n one\n-two\n+2\n three\n";
        for text in [git, plain] {
            let sections = parse(text);
            let [section] = sections.as_slice() else { panic!("{sections:?}") };
            assert_eq!((section.path, section.removes_file, section.problem), ("src/a.rs", false, None));
            let [hunk] = section.hunks.as_slice() else { panic!("{section:?}") };
            assert_eq!(hunk.at, 2);
            let expected = [(Mark::Same, "one"), (Mark::Del, "two"), (Mark::Add, "2"), (Mark::Same, "three")];
            assert_eq!(hunk.lines, expected);
        }
    }

    #[test]
    fn a_removed_line_that_looks_like_a_file_header_stays_in_its_hunk() {
        let text = "--- a/a.md\n+++ b/a.md\n@@ -1,2 +1,1 @@\n--- old rule\n kept\n--- a/b.md\n+++ b/b.md\n@@ -0,0 +1 @@\n+new\n";
        let sections = parse(text);
        assert_eq!(sections.iter().map(|s| s.path).collect::<Vec<_>>(), ["a.md", "b.md"]);
        assert_eq!(sections[0].hunks[0].lines, [(Mark::Del, "-- old rule"), (Mark::Same, "kept")]);
        assert_eq!(
            (sections[1].hunks[0].at, sections[1].hunks[0].lines.as_slice()),
            (0, [(Mark::Add, "new")].as_slice())
        );
    }

    #[test]
    fn new_and_removed_files_take_their_name_from_the_side_that_has_one() {
        let text = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\n\\ No newline at end of file\n--- a/old.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
        let sections = parse(text);
        assert_eq!((sections[0].path, sections[0].removes_file), ("new.txt", false));
        assert_eq!((sections[1].path, sections[1].removes_file, sections[1].hunks[0].at), ("old.txt", true, 0));
        assert!(sections.iter().all(|section| section.problem.is_none()));
    }

    #[test]
    fn names_that_could_leave_the_project_are_refused_without_being_opened() {
        for name in [
            "../outside", "/etc/passwd", "a/../../b", ".git/config", "sub/.GIT/hooks/x", "\"quoted\\n\"", "bell\u{7}",
            "dir/",
        ] {
            let text = format!("--- {name}\n+++ {name}\n@@ -1 +1 @@\n-a\n+b\n");
            let sections = parse(&text);
            assert_eq!(sections[0].problem, Some(NOT_INSIDE), "{name}");
        }
        let both_missing = parse("--- /dev/null\n+++ /dev/null\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(both_missing[0].problem, Some(NOT_INSIDE));
    }

    #[test]
    fn a_file_without_text_or_with_a_cut_hunk_is_named_and_not_shown() {
        let binary = parse(
            "diff --git a/logo.png b/logo.png\nindex 1..2 100644\nBinary files a/logo.png and b/logo.png differ\n",
        );
        assert_eq!((binary[0].path, binary[0].problem), ("logo.png", Some(BINARY)));
        for cut in ["@@ -1,3 +1,3 @@\n one\n-two\n", "@@ -1 +1 @@\n*a\n", "@@ nonsense @@\n", ""] {
            let text = format!("--- a/a.txt\n+++ b/a.txt\n{cut}");
            assert_eq!(parse(&text)[0].problem, Some(MALFORMED), "{cut:?}");
        }
    }

    #[test]
    fn lines_a_hunk_header_did_not_count_make_the_file_unreadable_not_shorter() {
        let spilled = parse("--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-b\n+B\n c\n-d\n+D\n");
        assert_eq!(spilled[0].problem, Some(MALFORMED));
        // A header that claims more lines than any patch could hold is only a cut hunk.
        let huge = parse("--- a/f\n+++ b/f\n@@ -1,18446744073709551615 +18446744073709551615 @@\n-a\n+b\n");
        assert_eq!(huge[0].problem, Some(MALFORMED));
        // The next file, and the signature line of a mailed patch, end a hunk without a word.
        let ended = parse("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n-- \n2.50\n--- a/g\n+++ b/g\n@@ -1 +1 @@\n-a\n+b\n");
        assert!(ended.iter().all(|section| section.problem.is_none()) && ended.len() == 2);
    }

    #[test]
    fn text_that_is_not_a_diff_names_no_file() {
        assert!(parse("I changed two files.\n--- not a header\n").is_empty());
    }
}
