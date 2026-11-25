//! Finds the project files that a terminal's recent output mentions, for the Rex action that opens one in the reader.
//!
//! A *mention* is a word of the output that reads as a path, with the line and
//! column printed after it if there are any. It counts only if it leads to a
//! file of the *worktree* the terminal was working in: the Git worktree that
//! holds the terminal's working folder, or that folder itself outside Git.
//!
//! The output is untrusted text. Nothing here runs a program, opens an address,
//! reads a file's contents or keeps the output. A path is only looked up, and
//! what is printed for Rex holds no control characters.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::git;
use crate::reader::{self, Spot};
use crate::rex;

/// The most output read. Rex's action sends a quarter of this.
const INPUT: u64 = 1 << 20;
/// The most recent lines of that output that are looked at.
const LINES: usize = 2000;
/// A longer line is looked at only this far, and a longer word is passed over.
const LINE_BYTES: usize = 4096;
const WORD_BYTES: usize = 1024;
/// How many different paths are looked up on disk, and how many places are offered.
const LOOKUPS: usize = 4000;
const OFFERED: usize = 200;
/// The most characters of the output shown beside a place.
const CONTEXT: usize = 160;
/// Rex refuses a longer label.
const LABEL: usize = 128;

const BAD_ARGS: &str = "Invalid output-files arguments.";
const NO_FOLDER: &str = "Milo could not find the folder this pane is working in.";
const NO_INPUT: &str = "Milo could not read this pane's output.";
const NOT_OFFERED: &str = "That is not a file of this pane's worktree.";

/// What may stand around a path without being part of it.
const WRAPPERS: [char; 14] = ['(', ')', '[', ']', '{', '}', '<', '>', '"', '\'', '`', ',', ';', '='];
/// What may end a sentence or a diagnostic after a path.
const ENDINGS: [char; 6] = ['.', ',', ':', ';', '!', '?'];

/// Internal, noninteractive entry point for the installed Rex action.
///
/// `list CWD` reads output on standard input and prints `found N outside M`,
/// then a line for each place, most recent first: what to open, a tab, and the
/// output it was mentioned in. `open CWD PLACE [BLOCK]...` checks one of those
/// places again and offers it to the readers that may run in the blocks named,
/// which are the floating terminals of the tab. It prints `shown BLOCK` when
/// one took it, or `view` and what a new reader is to be given.
pub fn command(args: &[OsString]) -> i32 {
    match run(args) {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn run(args: &[OsString]) -> Result<String, &'static str> {
    let [action, cwd, rest @ ..] = args else { return Err(BAD_ARGS) };
    let tree = Worktree::of(Path::new(cwd)).ok_or(NO_FOLDER)?;
    match (action.to_str(), rest) {
        (Some("list"), []) => {
            let mut bytes = Vec::new();
            std::io::stdin().lock().take(INPUT).read_to_end(&mut bytes).map_err(|_| NO_INPUT)?;
            Ok(scan(&String::from_utf8_lossy(&bytes), &tree).listing())
        }
        (Some("open"), [place, blocks @ ..]) => {
            let place = place.to_str().and_then(|text| tree.place(text)).ok_or(NOT_OFFERED)?;
            tree.open(&place, blocks, reader::show_in).ok_or(NO_FOLDER)
        }
        _ => Err(BAD_ARGS),
    }
}

/// The folder whose files may be opened, and the folder that paths in the output start from.
struct Worktree {
    root: PathBuf,
    cwd: PathBuf,
}

/// Where a path in the output leads.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Lookup {
    /// To a file of the worktree, named from its root with `/`.
    Inside(String),
    /// To a file that is not the worktree's to open.
    Outside,
    Missing,
}

/// A file of the worktree and where in it, as the output named it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Place {
    file: String,
    spot: Option<Spot>,
}

impl Place {
    /// How the place is offered and asked for: `src/disk.rs:184:17`.
    fn title(&self) -> String {
        match self.spot {
            Some(spot) => format!("{}:{spot}", self.file),
            None => self.file.clone(),
        }
    }
}

impl Worktree {
    /// Git decides the worktree. Where Git cannot be asked or there is none,
    /// only files under the working folder itself are opened.
    fn of(cwd: &Path) -> Option<Self> {
        if !cwd.is_absolute() {
            return None;
        }
        let cwd = cwd.canonicalize().ok().filter(|dir| dir.is_dir())?;
        let checkout = git::checkout(&cwd).ok().flatten();
        let root = checkout.and_then(|checkout| checkout.root.canonicalize().ok()).filter(|root| cwd.starts_with(root));
        Some(Self { root: root.unwrap_or_else(|| cwd.clone()), cwd })
    }

    /// Follows `path` as a program in the working folder would, links
    /// included, and says whether it ends at a file of this worktree.
    fn lookup(&self, path: &str) -> Lookup {
        let Ok(real) = self.cwd.join(path).canonicalize() else { return Lookup::Missing };
        if !real.is_file() {
            return Lookup::Missing;
        }
        let Ok(relative) = real.strip_prefix(&self.root) else { return Lookup::Outside };
        // A folder with a `.git` of its own is another worktree or a submodule,
        // and what Git keeps under `.git` is not a file of the project.
        let nested = relative.ancestors().skip(1).filter(|dir| !dir.as_os_str().is_empty()).any(|dir| {
            dir.file_name().is_some_and(|name| name == ".git")
                || fs::symlink_metadata(self.root.join(dir).join(".git")).is_ok()
        });
        match relative.to_str().filter(|name| !nested && !name.chars().any(char::is_control)) {
            Some(name) => Lookup::Inside(name.to_owned()),
            None => Lookup::Outside,
        }
    }

    /// The place a title names, if it is still a file of this worktree.
    fn place(&self, title: &str) -> Option<Place> {
        let (path, spot) = split_spot(title);
        match self.lookup(path) {
            Lookup::Inside(file) => Some(Place { file, spot }),
            Lookup::Outside | Lookup::Missing => None,
        }
    }

    /// Offers `place` to the readers in `blocks`, of which only one opened for
    /// this worktree takes it, and says which did or what a new reader is to
    /// be given. Nothing is said of a worktree whose path a line of text cannot carry.
    fn open(
        &self,
        place: &Place,
        blocks: &[OsString],
        show_in: impl Fn(&str, &Path, &Path, Option<Spot>) -> bool,
    ) -> Option<String> {
        let root = self.root.to_str().filter(|root| !root.chars().any(char::is_control))?;
        let file = self.root.join(&place.file);
        let label = clipped(place.file.rsplit('/').next().unwrap_or_default(), LABEL);
        for block in blocks {
            if let Some(block) = rex::exact_id(block, "block:")
                && show_in(block, &self.root, &file, place.spot)
            {
                return Some(format!("shown\t{block}\t{label}\n"));
            }
        }
        let spot = place.spot.map_or_else(|| "-".to_owned(), |spot| spot.to_string());
        Some(format!("view\t{root}\t{root}/{}\t{spot}\t{label}\n", place.file))
    }
}

/// The places found in some output, most recent first, each with the output around its mention.
#[derive(Default, Debug)]
struct Report {
    found: Vec<(Place, String)>,
    /// How many different files were mentioned that are not this worktree's to open.
    outside: usize,
}

impl Report {
    fn listing(&self) -> String {
        let mut text = format!("found {} outside {}\n", self.found.len(), self.outside);
        for (place, context) in &self.found {
            text.push_str(&place.title());
            text.push('\t');
            text.push_str(context);
            text.push('\n');
        }
        text
    }
}

/// Looks through the last lines of `text` from the end, so that what was
/// printed last is offered first. A place mentioned again is offered once, as
/// it was last mentioned; another line of the same file is another place.
fn scan(text: &str, tree: &Worktree) -> Report {
    let lines: Vec<&str> = text.lines().collect();
    let lines = &lines[lines.len().saturating_sub(LINES)..];
    let mut looked_up: HashMap<&str, Lookup> = HashMap::new();
    let mut lookup = |path| {
        if looked_up.len() >= LOOKUPS && !looked_up.contains_key(path) {
            return Lookup::Missing;
        }
        looked_up.entry(path).or_insert_with(|| tree.lookup(path)).clone()
    };
    let (mut report, mut offered, mut outside) = (Report::default(), HashSet::new(), HashSet::new());
    for (index, line) in lines.iter().enumerate().rev() {
        let mut rest = clipped(line, LINE_BYTES);
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            let (word, after) = rest.split_at(rest.find(char::is_whitespace).unwrap_or(rest.len()));
            rest = after;
            for (path, spot) in mentions(word, after) {
                match lookup(path) {
                    Lookup::Inside(file) => {
                        let place = Place { file, spot };
                        if offered.insert(place.clone()) {
                            report.found.push((place, context(lines, index, word)));
                        }
                    }
                    Lookup::Outside => {
                        outside.insert(path);
                    }
                    Lookup::Missing => {}
                }
            }
            if report.found.len() >= OFFERED {
                report.outside = outside.len();
                return report;
            }
        }
    }
    report.outside = outside.len();
    report
}

/// The start of `text`, no longer than `limit` bytes.
fn clipped(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The paths a word of output may name, the likeliest first: the word without
/// what stands around it, then the parts between brackets, quotes and commas.
/// `after` is the rest of the line, where Python prints the line number.
fn mentions<'a>(word: &'a str, after: &str) -> Vec<(&'a str, Option<Spot>)> {
    let mut found: Vec<(&str, Option<Spot>)> = Vec::new();
    if word.len() > WORD_BYTES {
        return found;
    }
    let mut add = |part: &'a str, after: &str| {
        if let Some(mention) = mention(part, after)
            && !found.iter().any(|known| known.0 == mention.0)
        {
            found.push(mention);
        }
    };
    add(word, after);
    if word.contains(WRAPPERS) {
        for part in word.split(WRAPPERS) {
            add(part, "");
        }
    }
    found
}

/// The path and place one word or part of a word names, if it reads as a file's name at all.
fn mention<'a>(word: &'a str, after: &str) -> Option<(&'a str, Option<Spot>)> {
    let word = word.trim_end_matches(ENDINGS);
    // TypeScript and MSVC print `file(LINE,COLUMN)`.
    let in_brackets = word.strip_suffix(')').and_then(|open| open.rsplit_once('('));
    let in_brackets = in_brackets.and_then(|(path, spot)| Some((path, Spot::parse(&spot.replacen(',', ":", 1))?)));
    let (word, spot) = in_brackets.map_or((word, None), |(path, spot)| (path, Some(spot)));
    let quoted = word.ends_with(['"', '\'']);
    let word = bare(word);
    let (path, spot) = match spot {
        Some(spot) => (word, Some(spot)),
        None => {
            let (path, spot) = split_spot(word);
            // Python prints `File "FILE", line LINE`.
            let said = || after.trim_start().strip_prefix("line ").and_then(|rest| Spot::parse(digits(rest)));
            (path, spot.or_else(|| quoted.then(said).flatten()))
        }
    };
    let named = path.contains(['/', '.']) || spot.is_some();
    let plain = !path.contains("://") && !path.starts_with(['~', '-']) && !path.chars().any(char::is_control);
    (named && plain && path.chars().any(char::is_alphabetic)).then_some((path, spot))
}

/// A word without the brackets, quotes and sentence endings around it.
fn bare(mut word: &str) -> &str {
    loop {
        let inner = word.trim_matches(WRAPPERS).trim_end_matches(ENDINGS);
        if inner.len() == word.len() {
            return word;
        }
        word = inner;
    }
}

/// The digits `text` starts with.
fn digits(text: &str) -> &str {
    &text[..text.bytes().take_while(u8::is_ascii_digit).count()]
}

/// Parts a path from the place printed after it: `FILE:LINE`, `FILE:LINE:COLUMN`,
/// either followed by more of a diagnostic, or `FILE#LLINE`.
fn split_spot(word: &str) -> (&str, Option<Spot>) {
    if let Some((path, line)) = word.rsplit_once("#L")
        && let Some(spot) = Spot::parse(digits(line)).filter(|_| !path.is_empty())
    {
        return (path, Some(spot));
    }
    for (at, _) in word.match_indices(':').filter(|&(at, _)| at > 0) {
        let rest = &word[at + 1..];
        let line = digits(rest);
        let after = &rest[line.len()..];
        if line.is_empty() || !(after.is_empty() || after.starts_with([':', '-', ','])) {
            continue;
        }
        let column = after.strip_prefix(':').map(digits).filter(|column| !column.is_empty());
        let spot = match column {
            Some(column) => Spot::parse(&rest[..line.len() + 1 + column.len()]),
            None => Spot::parse(line),
        };
        if spot.is_some() {
            return (&word[..at], spot);
        }
    }
    (word, None)
}

/// The output that says why a file was mentioned: its own line where that says
/// something besides the path, else the line above an arrow or a frame, as
/// compilers print them. A line that is only the path has nothing to add.
fn context(lines: &[&str], index: usize, word: &str) -> String {
    let says = |text: &str| text.chars().filter(|c| c.is_alphanumeric()).count() >= 3;
    let line = lines[index];
    let around = line.replacen(word, " ", 1);
    let chosen = if says(&around) {
        line
    } else if around.trim().is_empty() {
        ""
    } else {
        lines[..index].iter().rev().take(3).find(|above| says(above)).copied().unwrap_or_default()
    };
    tidy(chosen)
}

/// One short line of plain text: no terminal controls, no runs of spaces.
fn tidy(text: &str) -> String {
    let mut plain = String::with_capacity(text.len().min(CONTEXT * 4));
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.next_if_eq(&'[').is_some() {
            // A colour or cursor sequence ends at its first letter.
            while chars.next().is_some_and(|c| !c.is_ascii_alphabetic()) {}
        } else if c.is_control() || c.is_whitespace() {
            if !plain.is_empty() && !plain.ends_with(' ') {
                plain.push(' ');
            }
        } else {
            plain.push(c);
        }
    }
    let plain = plain.trim_end();
    match plain.char_indices().nth(CONTEXT) {
        Some((end, _)) => format!("{}…", plain[..end].trim_end()),
        None => plain.to_owned(),
    }
}

#[cfg(test)]
mod tests;
