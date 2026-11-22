//! Finds a patch's lines in the file as it is now and takes them back out, in memory.

use super::patch::{Hunk, Mark, Section};

pub(super) const CHANGED: &str =
    "This file no longer holds the lines the patch left, so its edits cannot be told apart.";
pub(super) const REPEATED: &str = "The patch's lines are in this file more than once, so its edits cannot be placed.";
pub(super) const TOO_MUCH: &str = "Placing this patch in the file would take too long.";
pub(super) const NOTHING: &str = "The patch changes no line of this file.";

/// Line comparisons allowed for one file, so that a patch cannot hold the pane up.
const WORK: usize = 20_000_000;

/// A line of the file as it was before some of the patch's sections.
#[derive(Clone, Copy)]
struct Item<'a> {
    text: &'a str,
    /// Its index among the file's lines now, or none for a line the patch removed.
    now: Option<usize>,
}

/// Writes the whole file as one hunk of a diff, in which only what `sections`
/// did is added or removed. Everything else in the file reads as unchanged,
/// whoever wrote it and whether or not Git has it.
///
/// The sections are undone last first. Each must *fit*: its lines as it left
/// them must still be in the file, where it said or in exactly one other
/// place. Where the lines around an edit have changed since, the unchanged
/// lines at either end of a hunk are given up one by one, as long as one
/// unchanged line is left to hold the edit in place, and what is left must
/// then be found exactly once.
///
/// `last_first` gives the file's sections in the reverse of the patch's order.
pub(super) fn whole_file_diff<'s, 'a: 's>(
    now: &str,
    last_first: impl Iterator<Item = &'s Section<'a>>,
) -> Result<Vec<u8>, &'static str> {
    let lines: Vec<&str> = now.split_terminator('\n').collect();
    let mut before: Vec<Item<'_>> = lines.iter().enumerate().map(|(i, text)| Item { text, now: Some(i) }).collect();
    let mut work = WORK;
    for section in last_first {
        before = undo(&before, section, &mut work)?;
    }
    let mut out = Vec::with_capacity(now.len() + lines.len() + 64);
    out.extend_from_slice(b"diff --git a/file b/file\n@@ -1 +1 @@\n");
    let mut put = |sign: u8, text: &str| {
        out.push(sign);
        out.extend_from_slice(text.as_bytes());
        out.push(b'\n');
    };
    let (mut next, mut edits) = (0, 0usize);
    for item in &before {
        match item.now {
            Some(i) => {
                lines[next..i].iter().for_each(|added| put(b'+', added));
                edits += i - next;
                put(b' ', item.text);
                next = i + 1;
            }
            None => {
                put(b'-', item.text);
                edits += 1;
            }
        }
    }
    lines[next..].iter().for_each(|added| put(b'+', added));
    if edits + lines.len() - next == 0 { Err(NOTHING) } else { Ok(out) }
}

/// The lines as they were before `section`, given the lines after it.
fn undo<'a>(after: &[Item<'a>], section: &Section<'a>, work: &mut usize) -> Result<Vec<Item<'a>>, &'static str> {
    // Copying the file once for each section is work too, however little each section finds.
    *work = work.checked_sub(after.len()).ok_or(TOO_MUCH)?;
    let mut before = Vec::with_capacity(after.len());
    // Lines of `after` already taken; hunks come in the file's order.
    let mut done = 0;
    for hunk in &section.hunks {
        let Placed { at, lines } = place(after, done, hunk, work)?;
        before.extend_from_slice(&after[done..at]);
        done = at;
        for &(mark, text) in lines {
            match mark {
                Mark::Same => before.push(after[done]),
                Mark::Del => before.push(Item { text, now: None }),
                Mark::Add => {}
            }
            done += usize::from(mark != Mark::Del);
        }
    }
    before.extend_from_slice(&after[done..]);
    Ok(before)
}

/// The lines of a hunk that were found, and the index of the first of them.
struct Placed<'h, 'a> {
    at: usize,
    lines: &'h [(Mark, &'a str)],
}

/// Finds the hunk's lines in `after`, at or past `from`.
fn place<'h, 'a>(
    after: &[Item<'a>],
    from: usize,
    hunk: &'h Hunk<'a>,
    work: &mut usize,
) -> Result<Placed<'h, 'a>, &'static str> {
    let same = |&&(mark, _): &&(Mark, &str)| mark == Mark::Same;
    let lead = hunk.lines.iter().take_while(same).count();
    let trail = hunk.lines[lead..].iter().rev().take_while(same).count();
    let unchanged = hunk.lines.iter().filter(same).count();
    let removed = hunk.lines.iter().filter(|(mark, _)| *mark == Mark::Del).count();
    for cut in 0..=lead.max(trail) {
        *work = work.checked_sub(1).ok_or(TOO_MUCH)?;
        let given_up = cut.min(lead) + cut.min(trail);
        if cut > 0 && given_up == unchanged {
            // With no unchanged line left, a common line could be found somewhere the patch never was.
            break;
        }
        let lines = &hunk.lines[cut.min(lead)..hunk.lines.len() - cut.min(trail)];
        let left = lines.len() - removed;
        if left == 0 {
            // Removed lines alone have a certain place only in a file with nothing else in it.
            return if after.is_empty() { Ok(Placed { at: 0, lines }) } else { Err(CHANGED) };
        }
        if cut == 0 && hunk.at >= from && holds(after, hunk.at, lines, work)? {
            return Ok(Placed { at: hunk.at, lines });
        }
        let mut found = None;
        for at in from..(after.len() + 1).saturating_sub(left) {
            if holds(after, at, lines, work)? && found.replace(at).is_some() {
                return Err(REPEATED);
            }
        }
        if let Some(at) = found {
            return Ok(Placed { at, lines });
        }
    }
    Err(CHANGED)
}

/// Whether the lines the hunk left are the ones at `at`.
fn holds(after: &[Item<'_>], at: usize, lines: &[(Mark, &str)], work: &mut usize) -> Result<bool, &'static str> {
    let mut there = after.get(at..).unwrap_or_default().iter();
    for (_, text) in lines.iter().filter(|(mark, _)| *mark != Mark::Del) {
        *work = work.checked_sub(1).ok_or(TOO_MUCH)?;
        if there.next().is_none_or(|item| item.text != *text) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::super::patch;
    use super::*;

    /// The diff's lines after its two headers, for a file that holds one name a line.
    fn shown(now: &str, patch: &str) -> Result<Vec<String>, &'static str> {
        let sections = patch::parse(patch);
        let diff = whole_file_diff(now, sections.iter().rev())?;
        Ok(String::from_utf8(diff).unwrap().lines().skip(2).map(str::to_owned).collect())
    }

    fn file(lines: &[&str]) -> String {
        lines.iter().map(|line| format!("{line}\n")).collect()
    }

    const TEN: [&str; 10] = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
    /// Replaces `e` in `TEN` by `E`.
    const EDIT: &str = "--- a/f\n+++ b/f\n@@ -2,7 +2,7 @@\n b\n c\n d\n-e\n+E\n f\n g\n h\n";

    fn edited() -> Vec<&'static str> {
        TEN.iter().map(|line| if *line == "e" { "E" } else { line }).collect()
    }

    #[test]
    fn only_the_patchs_lines_are_marked_in_the_file_as_it_is() {
        let lines = shown(&file(&edited()), EDIT).unwrap();
        assert_eq!(lines, [" a", " b", " c", " d", "-e", "+E", " f", " g", " h", " i", " j"]);
    }

    #[test]
    fn other_peoples_lines_in_the_same_file_read_as_unchanged() {
        // Someone else added two lines above, changed `b` next to the edit's
        // context, and rewrote the last line after the edit was made.
        let mut now = edited();
        now.splice(0..2, ["zero", "one", "a", "B, by someone else"]);
        *now.last_mut().unwrap() = "J, by someone else";
        let lines = shown(&file(&now), EDIT).unwrap();
        let expected = [
            " zero", " one", " a", " B, by someone else", " c", " d", "-e", "+E", " f", " g", " h", " i",
            " J, by someone else",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn an_edit_that_was_rewritten_since_is_not_shown_at_all() {
        let now: Vec<_> = TEN.iter().map(|line| if *line == "e" { "E, rewritten" } else { line }).collect();
        assert_eq!(shown(&file(&now), EDIT), Err(CHANGED));
        // Nor is one whose file was put back as it was.
        assert_eq!(shown(&file(&TEN), EDIT), Err(CHANGED));
    }

    #[test]
    fn an_edit_whose_lines_are_there_twice_away_from_its_place_is_not_guessed() {
        let mut now = vec!["moved"];
        now.extend(edited());
        now.extend(edited());
        assert_eq!(shown(&file(&now), EDIT), Err(REPEATED));
        // Where the patch said they are, they are taken there.
        let mut twice = edited();
        twice.extend(edited());
        assert_eq!(shown(&file(&twice), EDIT).unwrap()[4..6], ["-e", "+E"]);
    }

    #[test]
    fn an_edit_with_no_unchanged_neighbour_left_is_not_placed_by_luck() {
        let patch = "--- a/f\n+++ b/f\n@@ -1,3 +1,4 @@\n fn a() {\n     one();\n+}\n }\n";
        let now = file(&["fn a() { // changed", "    one(); // changed", "}", "} // changed", "}"]);
        assert_eq!(shown(&now, patch), Err(CHANGED));
        // The added line is elsewhere in the file exactly once, in a function the patch never touched.
        let patch = "--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n fn a() {\n-    old();\n+    return;\n }\n";
        let now = file(&["fn a() { // edited", "    other();", "} // edited", "fn b() {", "    return;", "}"]);
        assert_eq!(shown(&now, patch), Err(CHANGED));
        // With no unchanged lines of its own, a hunk is still found where it is alone.
        let bare = "--- a/f\n+++ b/f\n@@ -2 +2 @@\n-    old();\n+    return;\n";
        assert_eq!(
            shown(&file(&["moved", "fn a() {", "    return;", "}"]), bare).unwrap()[2..4],
            ["-    old();", "+    return;"]
        );
    }

    #[test]
    fn edits_made_one_after_another_to_the_same_lines_show_first_to_last() {
        let first = "--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n a\n-b\n+B1\n c\n";
        let second = "--- a/f\n+++ b/f\n@@ -1,3 +1,4 @@\n a\n-B1\n+B2\n+B3\n c\n";
        let lines = shown(&file(&["a", "B2", "B3", "c"]), &format!("{first}{second}")).unwrap();
        assert_eq!(lines, [" a", "-b", "+B2", "+B3", " c"]);
        // Out of order they do not fit, and nothing is shown.
        assert_eq!(shown(&file(&["a", "B2", "B3", "c"]), &format!("{second}{first}")), Err(CHANGED));
    }

    #[test]
    fn a_new_file_keeps_lines_others_added_to_it_unmarked() {
        let patch = "--- /dev/null\n+++ b/f\n@@ -0,0 +1,2 @@\n+one\n+two\n";
        assert_eq!(shown("one\ntwo\n", patch).unwrap(), ["+one", "+two"]);
        assert_eq!(
            shown("one\ntwo\nthree, by someone else\n", patch).unwrap(),
            ["+one", "+two", " three, by someone else"]
        );
    }

    #[test]
    fn removed_lines_are_shown_where_their_neighbours_are_and_nowhere_else() {
        let patch = "--- a/f\n+++ b/f\n@@ -1,3 +1,2 @@\n a\n-b\n c\n";
        assert_eq!(shown("a\nc\n", patch).unwrap(), [" a", "-b", " c"]);
        assert_eq!(shown("x\ny\n", patch), Err(CHANGED));
        let all = "--- a/f\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n";
        assert_eq!(shown("", all).unwrap(), ["-a", "-b"]);
        assert_eq!(shown("back again\n", all), Err(CHANGED));
    }

    #[test]
    fn a_patch_of_unchanged_lines_only_shows_nothing() {
        assert_eq!(shown("a\nb\n", "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n b\n"), Err(NOTHING));
    }

    #[test]
    fn a_patch_that_would_take_too_long_to_place_is_given_up() {
        // Every line of the file starts a near match of a long hunk that is not there.
        let now = "x\n".repeat(60_000);
        let hunk: String = (0..600).map(|_| "+x\n").chain(["+y\n"]).collect();
        let patch = format!("--- a/f\n+++ b/f\n@@ -0,0 +70000,601 @@\n{hunk}");
        assert_eq!(shown(&now, &patch), Err(TOO_MUCH));
    }

    #[test]
    fn many_small_sections_count_as_work_though_each_is_found_at_once() {
        let now = "a\n".repeat(50_000);
        let patch = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+a\n".repeat(500);
        assert_eq!(shown(&now, &patch), Err(TOO_MUCH));
    }
}
