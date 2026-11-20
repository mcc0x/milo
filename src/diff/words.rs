//! Finds the words that differ between two paired lines, within fixed limits.
//!
//! A word is a run of letters, digits and underscores, a run of whitespace, or
//! any other single character. Double-width characters, such as those of Chinese
//! and Japanese text, stand alone because they are written without spaces.

use std::ops::Range;

use crate::width::{CellWidth, cell_width};

/// The most words compared on each side of a pair.
const MAX_WORDS: usize = 256;
/// The most words a pair may remove and add together.
const MAX_EDITS: usize = 32;
/// Bytes split into words, word comparisons and search steps allowed for one file,
/// so that its largest diff still opens promptly.
const BUDGET: usize = 4 << 20;
const UNREACHED: usize = usize::MAX;
const JOINER: char = '\u{200d}';

/// How a character joins its neighbours into a word.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    Letter,
    Space,
    /// A hidden character stays with the character before it.
    Mark,
    Single,
}

fn part(c: char) -> Part {
    if c.is_ascii_alphanumeric() {
        return Part::Letter;
    }
    match cell_width(c) {
        _ if c == '\t' => Part::Space,
        CellWidth::Skip | CellWidth::Attach => Part::Mark,
        _ if c.is_whitespace() => Part::Space,
        CellWidth::Narrow if c.is_alphanumeric() || c == '_' => Part::Letter,
        _ => Part::Single,
    }
}

/// Where the word starting at `from` ends.
fn word_end(text: &str, from: usize) -> usize {
    let mut chars = text[from..].char_indices();
    let Some((_, first)) = chars.next() else { return from };
    let kind = part(first);
    let mut joined = first == JOINER;
    for (at, c) in chars {
        let next = part(c);
        if next != Part::Mark && !joined && !(next == kind && matches!(kind, Part::Letter | Part::Space)) {
            return from + at;
        }
        joined = c == JOINER;
    }
    text.len()
}

/// A word boundary before the character that precedes `at`, found without reading the line from its start.
fn word_start(text: &str, at: usize) -> usize {
    let mut before = text[..at].char_indices().rev();
    let Some((mut start, mut after)) = before.next() else { return 0 };
    for (i, c) in before {
        let joins = match (part(c), part(after)) {
            (_, Part::Mark) => true,
            _ if c == JOINER => true,
            // Look through marks to the character they follow.
            (Part::Mark, _) => continue,
            (kind, next) => kind == next && matches!(kind, Part::Letter | Part::Space),
        };
        if !joins {
            return start;
        }
        (start, after) = (i, c);
    }
    0
}

/// One word of a line, and whether the other line lacks it.
struct Word {
    at: Range<usize>,
    /// Settles most comparisons without reading the word again.
    hash: u32,
    changed: bool,
}

/// Adds the word at `at` and returns its end, or `None` when the side already has as many words as it may.
fn push_word(text: &str, at: usize, words: &mut Vec<Word>, budget: &mut usize) -> Option<usize> {
    if words.len() == MAX_WORDS {
        return None;
    }
    let end = word_end(text, at);
    *budget = budget.saturating_sub(end - at);
    // FNV-1a.
    let hash =
        text.as_bytes()[at..end].iter().fold(0x811c_9dc5, |hash, &b| (hash ^ u32::from(b)).wrapping_mul(0x0100_0193));
    words.push(Word { at: at..end, hash, changed: false });
    Some(end)
}

/// The bytes of the changed words from `*at` on, or an empty range where such words would be.
fn run(words: &[Word], at: &mut usize, from: usize) -> Range<usize> {
    let start = words.get(*at).map_or_else(|| words.last().map_or(from, |word| word.at.end), |word| word.at.start);
    let mut end = start;
    while let Some(word) = words.get(*at).filter(|word| word.changed) {
        end = word.at.end;
        *at += 1;
    }
    start..end
}

/// The edit that reaches a position in the search for a shortest edit.
#[derive(Clone, Copy)]
enum Edit {
    Remove,
    Add,
}

/// Where diagonal `j` of the next row starts, given the row before it, and the edit that leads there.
/// See `Words::shortest` for the rows.
fn step(previous: &[usize], j: usize, n: usize, m: usize) -> Option<(usize, Edit)> {
    let d = previous.len();
    // Removing an old word moves one along from the diagonal below.
    let removed = j.checked_sub(1).and_then(|below| previous.get(below)).filter(|&&x| x < n).map(|x| x + 1);
    // Adding a new word keeps the place on the diagonal above, while that diagonal has new words left.
    let added =
        previous.get(j).copied().filter(|&x| x != UNREACHED && (x + d - 1).checked_sub(2 * j).is_some_and(|y| y < m));
    match (removed, added) {
        (Some(removed), Some(added)) if added > removed => Some((added, Edit::Add)),
        (Some(removed), _) => Some((removed, Edit::Remove)),
        (None, Some(added)) => Some((added, Edit::Add)),
        (None, None) => None,
    }
}

/// Scratch space and the work allowance for comparing the paired lines of one file.
pub(super) struct Words {
    old: Vec<Word>,
    new: Vec<Word>,
    /// Where the compared words start in both lines.
    from: usize,
    /// For each number of edits tried, the furthest old word reached on each diagonal.
    reached: Vec<usize>,
    /// What is left of `BUDGET` for this file.
    budget: usize,
}

impl Words {
    pub(super) fn new() -> Self {
        Self { old: Vec::new(), new: Vec::new(), from: 0, reached: Vec::new(), budget: BUDGET }
    }

    /// Finds the words that differ between two lines that share `prefix` bytes at
    /// the start and `suffix` at the end. Returns the bytes of `old` that are unchanged.
    /// `None` means the lines cannot keep `needed` bytes or cannot be compared within the limits.
    pub(super) fn compare(
        &mut self,
        old: &str,
        new: &str,
        prefix: usize,
        suffix: usize,
        needed: usize,
    ) -> Option<usize> {
        if self.budget == 0 {
            return None;
        }
        // The lines agree before `prefix`, so they share this boundary too.
        self.from = word_start(old, prefix);
        self.old.clear();
        self.new.clear();
        let (mut old_end, mut new_end) = (self.from, self.from);
        // Take whole words through each side's edit, then until the same text follows both.
        loop {
            let (old_left, new_left) = (old.len() - old_end, new.len() - new_end);
            if old_left > suffix || old_left > new_left {
                old_end = push_word(old, old_end, &mut self.old, &mut self.budget)?;
            } else if new_left > suffix || new_left > old_left {
                new_end = push_word(new, new_end, &mut self.new, &mut self.budget)?;
            } else {
                break;
            }
        }
        if needed > 0 && self.most_kept(old.len()) < needed {
            return None;
        }
        self.edits(old, new)?;
        let changed: usize = self.old.iter().filter(|word| word.changed).map(|word| word.at.len()).sum();
        Some(old.len() - changed)
    }

    /// The most bytes of the old line that any edit could keep: all but the words the new line lacks.
    /// Rewritten lines fail this test far sooner than a search for their shortest edit would.
    fn most_kept(&self, old_len: usize) -> usize {
        // A word can be taken for present by mistake, which only makes the limit generous.
        let mut present = [0_u64; 16];
        let bit = |word: &Word| ((word.hash >> 6) as usize % 16, 1_u64 << (word.hash % 64));
        for (at, mask) in self.new.iter().map(bit) {
            present[at] |= mask;
        }
        let lacking = self.old.iter().filter(|word| {
            let (at, mask) = bit(word);
            present[at] & mask == 0
        });
        old_len - lacking.map(|word| word.at.len()).sum::<usize>()
    }

    /// Each run of changed words in the old line with the run that replaces it in
    /// the new one, in order. Either range can be empty.
    pub(super) fn runs(&self) -> impl Iterator<Item = (Range<usize>, Range<usize>)> + '_ {
        let (mut i, mut j) = (0, 0);
        std::iter::from_fn(move || {
            // Unchanged words pair up in order, so both sides reach each changed run together.
            while self.old.get(i).is_some_and(|word| !word.changed) && self.new.get(j).is_some_and(|word| !word.changed)
            {
                i += 1;
                j += 1;
            }
            let old = run(&self.old, &mut i, self.from);
            let new = run(&self.new, &mut j, self.from);
            (!old.is_empty() || !new.is_empty()).then_some((old, new))
        })
    }

    fn same(&mut self, old: &str, new: &str, i: usize, j: usize) -> bool {
        let (a, b) = (&self.old[i], &self.new[j]);
        // Unequal hashes settle it; equal ones cost a look at the bytes.
        let alike = a.hash == b.hash && a.at.len() == b.at.len();
        self.budget = self.budget.saturating_sub(1 + if alike { a.at.len() } else { 0 });
        alike && old.as_bytes()[a.at.clone()] == new.as_bytes()[b.at.clone()]
    }

    /// Marks the words of a shortest edit between the two lists as changed.
    fn edits(&mut self, old: &str, new: &str) -> Option<()> {
        let (mut lo, mut old_hi, mut new_hi) = (0, self.old.len(), self.new.len());
        while lo < old_hi && lo < new_hi && self.same(old, new, lo, lo) {
            lo += 1;
        }
        while lo < old_hi && lo < new_hi && self.same(old, new, old_hi - 1, new_hi - 1) {
            old_hi -= 1;
            new_hi -= 1;
        }
        if lo == old_hi || lo == new_hi {
            // One run of words was only added or removed, however long it is.
            self.old[lo..old_hi].iter_mut().chain(&mut self.new[lo..new_hi]).for_each(|word| word.changed = true);
            return Some(());
        }
        let (n, m) = (old_hi - lo, new_hi - lo);
        let edits = self.shortest(old, new, lo, n, m)?;
        self.mark_path(lo, n, m, edits)
    }

    /// Counts the fewest removals and additions that turn the `n` old words from `lo` into the `m` new ones.
    ///
    /// This is Myers' greedy search. With `x` old and `y` new words consumed, row `d`
    /// of `reached` holds the furthest `x` that `d` edits reach on each diagonal
    /// `x - y = 2j - d`, for `j` from 0 to `d`. Equal words extend a diagonal for free.
    fn shortest(&mut self, old: &str, new: &str, lo: usize, n: usize, m: usize) -> Option<usize> {
        self.reached.clear();
        // The first words differ, or they would have been set aside with the other shared words.
        self.reached.push(0);
        for d in 1..=MAX_EDITS {
            if self.budget == 0 {
                return None;
            }
            self.budget = self.budget.saturating_sub(d + 1);
            let row = d * (d - 1) / 2;
            for j in 0..=d {
                let Some((mut x, _)) = step(&self.reached[row..row + d], j, n, m) else {
                    self.reached.push(UNREACHED);
                    continue;
                };
                let mut y = x + d - 2 * j;
                while x < n && y < m && self.same(old, new, lo + x, lo + y) {
                    x += 1;
                    y += 1;
                }
                self.reached.push(x);
                if x == n && y == m {
                    return Some(d);
                }
            }
        }
        None
    }

    /// Walks the search back from the end of both lists, marking each removed or added word.
    fn mark_path(&mut self, lo: usize, n: usize, m: usize, edits: usize) -> Option<()> {
        let (mut x, mut y) = (n, m);
        for d in (1..=edits).rev() {
            let row = d * (d - 1) / 2;
            let j = (x + d).checked_sub(y)? / 2;
            let (start, edit) = step(self.reached.get(row..row + d)?, j, n, m)?;
            match edit {
                Edit::Remove => {
                    (x, y) = (start.checked_sub(1)?, (start + d).checked_sub(2 * j)?);
                    self.old.get_mut(lo + x)?.changed = true;
                }
                Edit::Add => {
                    (x, y) = (start, (start + d).checked_sub(2 * j + 1)?);
                    self.new.get_mut(lo + y)?.changed = true;
                }
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::common_ends;
    use super::*;

    /// The changed runs of each line as text, or `None` when the pair is out of bounds.
    fn changed<'a>(words: &mut Words, old: &'a str, new: &'a str) -> Option<Vec<(&'a str, &'a str)>> {
        let (prefix, suffix) = common_ends(old, new);
        words.compare(old, new, prefix, suffix, 0)?;
        Some(words.runs().map(|(a, b)| (&old[a], &new[b])).collect())
    }

    fn pairs<'a>(old: &'a str, new: &'a str) -> Vec<(&'a str, &'a str)> {
        changed(&mut Words::new(), old, new).unwrap()
    }

    #[test]
    fn separate_edits_in_a_line_are_separate_runs() {
        assert_eq!(pairs("let total = count + 1;", "let sum = count + 2;"), [("total", "sum"), ("1", "2")]);
        assert_eq!(pairs("copy(a, b)", "copy(a, b, c)"), [("", ", c")]);
        assert_eq!(pairs("if ready && armed {", "if armed {"), [("ready && ", "")]);
        assert_eq!(pairs("count1 = 0", "count2 = 0"), [("count1", "count2")]);
        assert_eq!(pairs("same", "same"), []);
        assert_eq!(pairs("", "added words"), [("", "added words")]);
    }

    #[test]
    fn words_follow_unicode_letters_marks_and_wide_characters() {
        // Accented and non-Latin letters form words, with or without combining marks.
        assert_eq!(pairs("señor Müller привет", "señora Müller мир"), [("señor", "señora"), ("привет", "мир")]);
        assert_eq!(pairs("cafe\u{301} noir", "cafe\u{301} au lait"), [("noir", "au lait")]);
        assert_eq!(pairs("x = nai\u{308}ve", "x = native"), [("nai\u{308}ve", "native")]);
        // Text written without spaces changes one character at a time.
        assert_eq!(pairs("変更された行です", "変更されない行です"), [("た", "ない")]);
        assert_eq!(pairs("值为一，名为二", "值为三，名为四"), [("一", "三"), ("二", "四")]);
        // A joined emoji is one word, and so is a character with its variation selector.
        assert_eq!(pairs("by 👩\u{200d}💻 ok", "by 👩\u{200d}🚀 ok"), [("👩\u{200d}💻", "👩\u{200d}🚀")]);
        assert_eq!(pairs("a ❤\u{fe0f} b", "a ❤ b"), [("❤\u{fe0f}", "❤")]);
        // A hidden character belongs to the word it follows.
        assert_eq!(pairs("one two", "one\u{200b} two"), [("one", "one\u{200b}")]);
        assert_eq!(pairs("a", "\u{200b}a"), [("", "\u{200b}")]);
    }

    #[test]
    fn runs_are_whole_characters_in_order_on_both_sides() {
        let lines = [
            "",
            "plain words here",
            "tabs\tand  spaces\t",
            "e\u{301}\u{301}tude 変更 👩\u{200d}💻\u{fe0f} ok",
            "\u{200b}\u{feff}lead",
            "mixedАБВ界x_1",
            "\u{200d}\u{200d}",
            "a\u{200d}",
            " \u{301} ",
        ];
        for old in lines {
            for new in lines {
                let mut words = Words::new();
                let (prefix, suffix) = common_ends(old, new);
                let kept = words.compare(old, new, prefix, suffix, 0).unwrap();
                let (mut old_at, mut new_at, mut removed) = (0, 0, 0);
                for (a, b) in words.runs() {
                    assert!(old_at <= a.start && new_at <= b.start, "{old:?} {new:?}");
                    // The text between changed runs is the same on both sides.
                    assert_eq!(old[old_at..a.start], new[new_at..b.start], "{old:?} {new:?}");
                    assert!(!a.is_empty() || !b.is_empty());
                    removed += a.len();
                    (old_at, new_at) = (a.end, b.end);
                }
                assert_eq!(old[old_at..], new[new_at..], "{old:?} {new:?}");
                assert_eq!(kept, old.len() - removed);
                assert!(kept <= words.most_kept(old.len()), "{old:?} {new:?}");
            }
        }
    }

    /// The longest run of words two lists share in order, by the textbook table.
    fn longest_common(a: &[u8], b: &[u8]) -> usize {
        let mut table = vec![vec![0; b.len() + 1]; a.len() + 1];
        for i in 0..a.len() {
            for j in 0..b.len() {
                table[i + 1][j + 1] = if a[i] == b[j] { table[i][j] + 1 } else { table[i][j + 1].max(table[i + 1][j]) };
            }
        }
        table[a.len()][b.len()]
    }

    #[test]
    fn the_search_keeps_as_many_words_as_any_edit_could() {
        // Punctuation marks are one word each, so every byte of these lines is a word.
        let mut seed = 0x2545_f491_u32;
        let mut line = |len: u32| -> String {
            (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    char::from(b"+-*/"[(seed >> 24) as usize % 4])
                })
                .collect()
        };
        for round in 0..400 {
            let (old, new) = (line(round % 13), line(round % 11));
            let mut words = Words::new();
            let (prefix, suffix) = common_ends(&old, &new);
            let kept = words.compare(&old, &new, prefix, suffix, 0).unwrap();
            assert_eq!(kept, longest_common(old.as_bytes(), new.as_bytes()), "{old:?} {new:?}");
            assert!(kept <= words.most_kept(old.len()), "{old:?} {new:?}");
            let unchanged = |words: &[Word], text: &str| -> String {
                let shared = words.first().map_or(text, |word| &text[..word.at.start]);
                let middle = words.iter().filter(|word| !word.changed).map(|word| &text[word.at.clone()]);
                let tail = words.last().map_or("", |word| &text[word.at.end..]);
                std::iter::once(shared).chain(middle).chain([tail]).collect()
            };
            assert_eq!(unchanged(&words.old, &old), unchanged(&words.new, &new), "{old:?} {new:?}");
        }
    }

    #[test]
    fn a_pair_with_too_many_words_or_edits_is_not_compared() {
        let mut words = Words::new();
        // One run of added words needs no search, so it may be as long as a side can be.
        let many = "x ".repeat(MAX_WORDS / 2 - 1);
        assert_eq!(changed(&mut words, "start end", &format!("start {many}end")), Some(vec![("", many.as_str())]));
        let more = "x ".repeat(MAX_WORDS / 2 + 1);
        assert_eq!(changed(&mut words, "start end", &format!("start {more}end")), None);

        // Sixteen replaced words are thirty-two edits.
        let numbers = |from: usize, count: usize| (from..from + count).map(|n| n.to_string()).collect::<Vec<_>>();
        let old = numbers(100, MAX_EDITS / 2).join(" ");
        let new = numbers(200, MAX_EDITS / 2).join(" ");
        assert_eq!(changed(&mut words, &old, &new).map(|runs| runs.len()), Some(MAX_EDITS / 2));
        let old = numbers(100, MAX_EDITS / 2 + 1).join(" ");
        let new = numbers(200, MAX_EDITS / 2 + 1).join(" ");
        assert_eq!(changed(&mut words, &old, &new), None);
    }

    #[test]
    fn lines_that_cannot_share_enough_are_refused_without_a_search() {
        let mut words = Words::new();
        let (old, new) = ("keep the last good frame until git answers", "total count index buffer cursor width span");
        let (prefix, suffix) = common_ends(old, new);
        // Only the spaces are shared, far less than half of the line. Splitting it into words is all that costs.
        assert_eq!(words.compare(old, new, prefix, suffix, old.len() / 2), None);
        let split = BUDGET - old.len() - new.len();
        assert_eq!(words.budget, split);
        // Asking for no more than those spaces lets the search run.
        assert_eq!(words.compare(old, new, prefix, suffix, 6), Some(6));
        assert!(words.budget < split - old.len() - new.len());
    }

    #[test]
    fn a_spent_allowance_stops_comparing_this_and_later_pairs() {
        let mut words = Words::new();
        words.budget = 200;
        assert_eq!(changed(&mut words, "let a = 1;", "let b = 2;"), Some(vec![("a", "b"), ("1", "2")]));
        assert!((1..200).contains(&words.budget));
        words.budget = 3;
        assert_eq!(changed(&mut words, "let a = 1;", "let b = 2;"), None);
        assert_eq!(changed(&mut words, "one", "two"), None);
    }
}
