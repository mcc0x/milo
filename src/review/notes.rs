//! The notes written during one review, and the text they are copied as.
//!
//! A note keeps the lines it is about as the review showed them when it was
//! written. A note is *waiting* until its text has been handed to the
//! terminal's clipboard since it was last changed.

use std::fmt::Write as _;

use crate::diff::LineKind;
use crate::reader::{Picked, PickedLine};

/// The most characters one note holds.
pub const NOTE_LIMIT: usize = 4000;
/// The most notes one review holds.
pub const NOTES_LIMIT: usize = 99;
/// The most bytes copied at once. Terminals drop a clipboard request much larger than this.
pub const COPY_LIMIT: usize = 64 * 1024;

pub struct Note {
    /// The file's name as the review showed it.
    pub path: String,
    /// Where the first line was among the lines shown, and how many lines the note is about.
    pub at: usize,
    pub len: usize,
    /// The first of those lines, as shown then.
    pub lines: Vec<PickedLine>,
    kept: Option<(u32, u32)>,
    removed: Option<(u32, u32)>,
    pub text: String,
    pub copied: bool,
}

fn numbers(out: &mut String, one: &str, many: &str, (first, last): (u32, u32)) {
    let _ = if first == last { write!(out, "{one} {first}") } else { write!(out, "{many} {first}-{last}") };
}

/// Which lines a note is about. A removed line has no number in the file
/// now, so its number in the patch's base is given and said to be old.
pub fn place(kept: Option<(u32, u32)>, removed: Option<(u32, u32)>) -> String {
    let mut place = String::new();
    if let Some(kept) = kept {
        numbers(&mut place, "line", "lines", kept);
    }
    if let Some(removed) = removed {
        if !place.is_empty() {
            place.push_str(", ");
        }
        numbers(&mut place, "removed line", "removed lines", removed);
        place.push_str(" (old numbering)");
    }
    place
}

impl Note {
    pub fn new(path: String, picked: Picked, text: String) -> Self {
        let Picked { at, len, lines, kept, removed } = picked;
        Self { path, at, len, lines, kept, removed, text, copied: false }
    }

    pub fn place(&self) -> String {
        place(self.kept, self.removed)
    }

    /// The first line of the note's own words.
    pub fn summary(&self) -> &str {
        self.text.lines().next().unwrap_or_default()
    }

    fn write(&self, out: &mut String) {
        let _ = writeln!(out, "{}, {}:", self.path, self.place());
        let width = self.lines.iter().map(|line| line.number).max().unwrap_or(1).ilog10() as usize + 1;
        for line in &self.lines {
            let sign = match line.kind {
                LineKind::Same => ' ',
                LineKind::Add => '+',
                LineKind::Del => '-',
            };
            let _ = writeln!(out, "> {:>width$} {sign} {}", line.number, line.text);
        }
        match self.len - self.lines.len() {
            0 => {}
            1 => out.push_str("> … 1 more line\n"),
            more => {
                let _ = writeln!(out, "> … {more} more lines");
            }
        }
        out.push_str(&self.text);
        out.push('\n');
    }
}

/// One note as the text to copy.
pub fn one(note: &Note) -> String {
    let mut out = "Review note\n\n".to_string();
    note.write(&mut out);
    out
}

/// Several notes as one message, numbered in the order given.
pub fn batch<'a>(notes: impl ExactSizeIterator<Item = &'a Note>) -> String {
    let mut out = format!("Review notes ({})\n", notes.len());
    for (i, note) in notes.enumerate() {
        let _ = write!(out, "\n{}. ", i + 1);
        note.write(&mut out);
    }
    out
}

/// The notes waiting to be copied, as the one text they are copied as.
pub fn waiting(notes: &[Note]) -> Option<String> {
    let waiting: Vec<&Note> = notes.iter().filter(|note| !note.copied).collect();
    match waiting.as_slice() {
        [] => None,
        [note] => Some(one(note)),
        _ => Some(batch(waiting.into_iter())),
    }
}
