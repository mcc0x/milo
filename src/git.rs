//! Read-only git plumbing and the parsers for its output.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::when;

/// Patches larger than this are cut off rather than loaded whole.
const PATCH_LIMIT: usize = 4 << 20;
/// Changed lines shown per file before the rest is summarised.
const FILE_LINE_CAP: usize = 3000;
/// New files larger than this are listed without their content.
const NEW_FILE_LIMIT: u64 = 256 << 10;
/// What an empty repository is compared against before its first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

const LOG_FORMAT: &str = "--format=%x00%H%x00%an%x00%ar%x00%ct%x00%D%x00%s";
const HEAD_FORMAT: &str = "--format=%H%n%an%n%ar%n%D%n%B%x00";
const TAB: usize = 4;

fn command(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    // --no-optional-locks: a viewer must never contend with a real git command.
    c.arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=never"])
        .args(args)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    c
}

fn run(root: &Path, args: &[&str]) -> Vec<u8> {
    command(root, args).output().map(|o| o.stdout).unwrap_or_default()
}

/// Runs git, keeping at most `PATCH_LIMIT` bytes. Returns whether it was cut.
fn run_limited(root: &Path, args: &[&str]) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    let Ok(mut child) = command(root, args).stdout(Stdio::piped()).spawn() else {
        return (buf, false);
    };
    if let Some(out) = child.stdout.take() {
        let _ = out.take(PATCH_LIMIT as u64 + 1).read_to_end(&mut buf);
    }
    let cut = buf.len() > PATCH_LIMIT;
    if cut {
        buf.truncate(PATCH_LIMIT);
        let _ = child.kill();
    }
    let _ = child.wait();
    (buf, cut)
}

fn with_path<'a>(mut args: Vec<&'a str>, path: Option<&'a str>) -> Vec<&'a str> {
    if let Some(p) = path {
        args.extend(["--", p]);
    }
    args
}

pub fn toplevel(dir: &Path) -> Option<String> {
    let out = run(dir, &["rev-parse", "--show-toplevel"]);
    let s = String::from_utf8(out).ok()?;
    let s = s.trim_end_matches('\n');
    (!s.is_empty()).then(|| s.to_string())
}

/// How a path differs. Ordered by precedence when a directory summarises its
/// children.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Kind {
    None,
    Deleted,
    Renamed,
    Untracked,
    Added,
    Modified,
    Conflict,
}

pub struct StatusFile {
    pub path: String,
    pub untracked: bool,
    /// Has changes in the index.
    pub staged: bool,
}

#[derive(Default)]
pub struct Status {
    pub branch: String,
    pub oid: String,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<StatusFile>,
}

pub fn status_raw(root: &Path) -> Vec<u8> {
    run(root, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"])
}

pub fn parse_status(raw: &[u8]) -> Status {
    let mut st = Status::default();
    let text = String::from_utf8_lossy(raw);
    let mut entries = text.split('\0');
    while let Some(e) = entries.next() {
        let Some((tag, rest)) = e.split_once(' ') else { continue };
        // Field counts per entry type are fixed by the porcelain v2 format;
        // the path is always last and may itself contain spaces.
        let (fields, staged) = match tag {
            "#" => {
                if let Some(v) = rest.strip_prefix("branch.oid ") {
                    st.oid = v.to_string();
                } else if let Some(v) = rest.strip_prefix("branch.head ") {
                    st.branch = v.to_string();
                } else if let Some(v) = rest.strip_prefix("branch.ab ") {
                    let mut it = v.split(' ').map(|n| n.get(1..).and_then(|n| n.parse().ok()).unwrap_or(0));
                    st.ahead = it.next().unwrap_or(0);
                    st.behind = it.next().unwrap_or(0);
                }
                continue;
            }
            "?" => {
                st.files.push(StatusFile { path: rest.to_string(), untracked: true, staged: false });
                continue;
            }
            "1" => (8, !rest.starts_with('.')),
            "2" => {
                // A rename entry is followed by its original path.
                entries.next();
                (9, !rest.starts_with('.'))
            }
            "u" => (10, false),
            _ => continue,
        };
        if let Some(path) = rest.splitn(fields, ' ').last() {
            st.files.push(StatusFile { path: path.to_string(), untracked: false, staged });
        }
    }
    st
}

/// Every tracked and untracked-but-not-ignored file, relative to the root.
pub fn files(root: &Path) -> Vec<String> {
    let raw = run(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"]);
    let mut files: Vec<String> =
        String::from_utf8_lossy(&raw).split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect();
    files.dedup();
    files
}

#[derive(Clone, Copy, PartialEq, Default)]
pub struct Stat {
    pub files: u32,
    pub add: u32,
    pub del: u32,
}

pub struct Commit {
    pub graph: String,
    pub hash: String,
    pub author: String,
    /// Relative age, as git words it ("2 hours ago").
    pub when: String,
    pub refs: String,
    pub subject: String,
    /// Size of the change; filled in lazily for the rows on screen.
    pub stat: Option<Stat>,
}

pub enum Row {
    Uncommitted,
    Commit(Commit),
    /// A calendar heading in the simple timeline.
    Day(String),
    /// A connector line of the graph that carries no commit.
    Graph(String),
}

impl Row {
    pub fn selectable(&self) -> bool {
        matches!(self, Row::Uncommitted | Row::Commit(_))
    }
}

/// Branch and tag names without git's decoration syntax.
fn plain_refs(refs: &str) -> String {
    let names: Vec<&str> = refs
        .split(", ")
        .map(|r| r.strip_prefix("HEAD -> ").or_else(|| r.strip_prefix("tag: ")).unwrap_or(r))
        .filter(|r| !r.is_empty() && *r != "HEAD" && !r.ends_with("/HEAD"))
        .collect();
    names.join(", ")
}

/// Loads up to `limit` commits. With `graph` the rows carry git's branch
/// graph; without it they are grouped under day headings. Returns the rows
/// and whether history ended.
pub fn log(root: &Path, limit: usize, all: bool, graph: bool, path: Option<&str>) -> (Vec<Row>, bool) {
    let n = limit.to_string();
    let mut args = vec!["log", "--no-show-signature", LOG_FORMAT, "-n", &n];
    if graph {
        args.push("--graph");
    }
    if all {
        args.push("--all");
    }
    let raw = run(root, &with_path(args, path));
    let now = when::now();
    let mut rows = Vec::new();
    let (mut commits, mut day) = (0, String::new());
    for line in String::from_utf8_lossy(&raw).lines() {
        let mut f = line.split('\0');
        let lead = f.next().unwrap_or("").trim_end().to_string();
        let Some(hash) = f.next() else {
            rows.push(Row::Graph(lead));
            continue;
        };
        commits += 1;
        let mut field = || f.next().unwrap_or("");
        let (author, when, time, refs, subject) = (field(), field(), field(), field(), field());
        if !graph {
            let label = when::day_label(time.parse().unwrap_or(0), now);
            if label != day {
                rows.push(Row::Day(label.clone()));
                day = label;
            }
        }
        rows.push(Row::Commit(Commit {
            graph: lead,
            hash: hash.to_string(),
            author: author.to_string(),
            when: when.to_string(),
            refs: if graph { refs.to_string() } else { plain_refs(refs) },
            subject: subject.to_string(),
            stat: None,
        }));
    }
    (rows, commits < limit)
}

/// Change sizes for the given commits, each against its first parent.
pub fn stats(root: &Path, hashes: &[&str], path: Option<&str>) -> Vec<(String, Stat)> {
    let mut args =
        vec!["log", "--no-walk=unsorted", "--shortstat", "--diff-merges=first-parent", "-M", "--format=%x00%H"];
    args.extend(hashes);
    let raw = run(root, &with_path(args, path));
    String::from_utf8_lossy(&raw)
        .split('\0')
        .skip(1)
        .map(|chunk| {
            let (hash, rest) = chunk.split_once('\n').unwrap_or((chunk, ""));
            let mut stat = Stat::default();
            let mut n = 0;
            // " 3 files changed, 12 insertions(+), 3 deletions(-)"
            for tok in rest.split_whitespace() {
                match tok.parse() {
                    Ok(v) => n = v,
                    Err(_) if tok.starts_with("file") => stat.files = n,
                    Err(_) if tok.starts_with("insertion") => stat.add = n,
                    Err(_) if tok.starts_with("deletion") => stat.del = n,
                    Err(_) => {}
                }
            }
            (hash.trim().to_string(), stat)
        })
        .collect()
}

#[derive(PartialEq)]
pub struct FileChange {
    pub path: String,
    pub kind: Kind,
    pub add: u32,
    pub del: u32,
    pub binary: bool,
    pub staged: bool,
    /// Where this file's section starts in the change view, if it is shown.
    pub line: Option<usize>,
}

impl FileChange {
    fn new(path: &str, kind: Kind) -> FileChange {
        FileChange { path: path.to_string(), kind, add: 0, del: 0, binary: false, staged: false, line: None }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Title,
    /// Secondary facts: who, when.
    Meta,
    /// Prose, such as a commit message body.
    Info,
    /// Heading of a file's section; the index into `Detail::files`.
    File(u32),
    /// A gap between two shown stretches of a file.
    Hunk,
    Context,
    Add,
    Del,
    /// Why something is not shown.
    Note,
}

#[derive(PartialEq)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
    /// Line number in the file: the new one, or the old one for a removal.
    pub no: u32,
    /// Byte range of `text` that differs from its paired line; empty if none.
    pub changed: (u32, u32),
}

impl Line {
    fn new(kind: LineKind, text: impl Into<String>) -> Line {
        Line { kind, text: text.into(), no: 0, changed: (0, 0) }
    }
}

/// Everything shown for one moment of the timeline.
#[derive(Default, PartialEq)]
pub struct Detail {
    pub files: Vec<FileChange>,
    pub lines: Vec<Line>,
    /// Largest line number shown, to size the gutter.
    pub max_no: u32,
}

/// Parses `--raw --numstat -z` output: what happened to each path, then how
/// many lines each gained and lost.
fn parse_changes(raw: &[u8]) -> Vec<FileChange> {
    let text = String::from_utf8_lossy(raw);
    let mut tokens = text.split('\0').filter(|t| !t.is_empty());
    let mut files: Vec<FileChange> = Vec::new();
    let mut next = 0;
    while let Some(tok) = tokens.next() {
        if tok.starts_with(':') {
            let code = tok.rsplit(' ').next().unwrap_or("M").as_bytes()[0];
            let kind = match code {
                b'A' | b'C' => Kind::Added,
                b'D' => Kind::Deleted,
                b'U' => Kind::Conflict,
                b'R' => Kind::Renamed,
                _ => Kind::Modified,
            };
            // Renames and copies list the old path first.
            if matches!(code, b'R' | b'C') {
                tokens.next();
            }
            if let Some(path) = tokens.next() {
                files.push(FileChange::new(path, kind));
            }
            continue;
        }
        let mut f = tok.splitn(3, '\t');
        let (add, del, path) = (f.next().unwrap_or(""), f.next().unwrap_or(""), f.next().unwrap_or(""));
        let path = if path.is_empty() {
            tokens.next();
            tokens.next().unwrap_or("")
        } else {
            path
        };
        // Both lists come in the same order, so the match is normally `next`.
        let at = if files.get(next).is_some_and(|f| f.path == path) {
            Some(next)
        } else {
            files.iter().position(|f| f.path == path)
        };
        if let Some(i) = at {
            next = i + 1;
            files[i].binary = add == "-";
            files[i].add = add.parse().unwrap_or(0);
            files[i].del = del.parse().unwrap_or(0);
        }
    }
    files
}

/// Expands tabs and drops control characters so a line is safe to draw.
fn clean(line: &str) -> String {
    let mut s = String::with_capacity(line.len());
    let mut col = 0;
    for c in line.chars() {
        if c == '\t' {
            let pad = TAB - col % TAB;
            s.extend(std::iter::repeat_n(' ', pad));
            col += pad;
        } else if !c.is_control() {
            s.push(c);
            col += 1;
        }
    }
    s
}

/// Starts a file's section and records where it begins.
fn open_file(d: &mut Detail, i: usize) {
    d.lines.push(Line::new(LineKind::Info, ""));
    d.files[i].line = Some(d.lines.len());
    d.lines.push(Line::new(LineKind::File(i as u32), ""));
}

/// Appends a git patch as numbered lines under one heading per file. The
/// patch's own headers are dropped: `d.files` already says what they say.
fn push_patch(d: &mut Detail, raw: &[u8], detailed: bool) {
    let text = String::from_utf8_lossy(raw);
    let (mut next, mut in_hunk, mut skip, mut shown) = (0, false, true, 0);
    let (mut old_no, mut new_no) = (0u32, 0u32);
    for l in text.lines() {
        if let Some(names) = l.strip_prefix("diff --git ") {
            let named = |f: &FileChange| names.strip_suffix(f.path.as_str()).is_some_and(|r| r.ends_with(" b/"));
            let i = if d.files.get(next).is_some_and(named) {
                next
            } else if let Some(i) = d.files.iter().position(named) {
                i
            } else {
                let path = names.rsplit_once(" b/").map_or(names, |(_, p)| p);
                d.files.push(FileChange::new(path, Kind::Modified));
                d.files.len() - 1
            };
            next = i + 1;
            open_file(d, i);
            let f = &d.files[i];
            (in_hunk, shown) = (false, 0);
            skip = f.binary || (!detailed && f.kind == Kind::Deleted);
            if f.binary {
                d.lines.push(Line::new(LineKind::Note, "Not text, so there is nothing to show."));
            } else if skip {
                let lines = if f.del == 1 { "1 line".into() } else { format!("{} lines", f.del) };
                d.lines.push(Line::new(LineKind::Note, format!("Removed, {lines}.")));
            }
            continue;
        }
        if skip {
            continue;
        }
        if let Some(hunk) = l.strip_prefix("@@ -") {
            // "@@ -12,5 +14,6 @@ fn name()"
            let start = |s: &str| s.split([',', ' ']).next().and_then(|n| n.parse().ok()).unwrap_or(1);
            let (ranges, context) = hunk.split_once(" @@").unwrap_or((hunk, ""));
            old_no = start(ranges);
            new_no = ranges.split_once(" +").map_or(1, |(_, new)| start(new));
            // A file shown from its top needs no "skipped to here" marker.
            if in_hunk || new_no > 1 {
                let mut line = Line::new(LineKind::Hunk, clean(context.trim()));
                line.no = new_no;
                d.lines.push(line);
            }
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (kind, no) = match l.as_bytes().first() {
            Some(b'+') => (LineKind::Add, &mut new_no),
            Some(b'-') => (LineKind::Del, &mut old_no),
            // "\ No newline at end of file"
            Some(b'\\') => continue,
            _ => {
                old_no += 1;
                (LineKind::Context, &mut new_no)
            }
        };
        let mut line = Line::new(kind, clean(l.get(1..).unwrap_or("")));
        line.no = *no;
        *no += 1;
        shown += 1;
        if shown <= FILE_LINE_CAP {
            d.lines.push(line);
        } else if shown == FILE_LINE_CAP + 1 {
            d.lines.push(Line::new(LineKind::Note, "A very large change; the rest is not shown."));
        }
    }
}

/// Appends a file git does not track yet, as entirely new lines.
fn push_new_file(d: &mut Detail, root: &Path, path: &str, budget: &mut u64) {
    d.files.push(FileChange::new(path, Kind::Untracked));
    let i = d.files.len() - 1;
    open_file(d, i);
    let full = root.join(path);
    let size = match std::fs::metadata(&full) {
        Ok(m) if m.is_file() => m.len(),
        _ => return,
    };
    if size > NEW_FILE_LIMIT.min(*budget) {
        d.lines.push(Line::new(LineKind::Note, "A large new file; its content is not shown."));
        return;
    }
    let Ok(bytes) = std::fs::read(&full) else { return };
    *budget -= size.min(*budget);
    if bytes.contains(&0) {
        d.files[i].binary = true;
        d.lines.push(Line::new(LineKind::Note, "Not text, so there is nothing to show."));
        return;
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut count = 0;
    for l in text.lines() {
        count += 1;
        if count <= FILE_LINE_CAP {
            let mut line = Line::new(LineKind::Add, clean(l));
            line.no = count as u32;
            d.lines.push(line);
        }
    }
    d.files[i].add = count as u32;
    if count > FILE_LINE_CAP {
        d.lines.push(Line::new(LineKind::Note, "A very large file; the rest is not shown."));
    }
}

/// Bytes two strings share at the start and at the end, without overlap.
fn common_ends(a: &str, b: &str) -> (usize, usize) {
    let prefix: usize = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    let (ra, rb) = (&a[prefix..], &b[prefix..]);
    let suffix = ra.chars().rev().zip(rb.chars().rev()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    (prefix, suffix)
}

/// Where a run of removals is followed by as many additions, marks the part
/// of each line that actually changed, so a one-word edit reads as one word.
fn mark_changed(lines: &mut [Line]) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != LineKind::Del {
            i += 1;
            continue;
        }
        let dels = lines[i..].iter().take_while(|l| l.kind == LineKind::Del).count();
        let adds = lines[i + dels..].iter().take_while(|l| l.kind == LineKind::Add).count();
        if dels == adds {
            for k in i..i + dels {
                let (old, new) = (&lines[k].text, &lines[k + dels].text);
                let (prefix, suffix) = common_ends(old, new);
                // Lines that share less than half are different lines, not edits.
                if (prefix + suffix) * 2 < old.len().max(new.len()) {
                    continue;
                }
                let (old_end, new_end) = (old.len() - suffix, new.len() - suffix);
                lines[k].changed = (prefix as u32, old_end as u32);
                lines[k + dels].changed = (prefix as u32, new_end as u32);
            }
        }
        i += dels + adds;
    }
}

fn finish(mut d: Detail, cut: bool) -> Detail {
    if cut {
        d.lines.push(Line::new(LineKind::Note, "This change is too large to show in full."));
    }
    mark_changed(&mut d.lines);
    d.max_no = d.lines.iter().map(|l| l.no).max().unwrap_or(0);
    d
}

pub fn commit_detail(root: &Path, hash: &str, path: Option<&str>, detailed: bool) -> Detail {
    let mut d = Detail::default();
    let common = ["show", "--diff-merges=first-parent", "-M"];
    let mut args = common.to_vec();
    args.extend(["--format=", "--raw", "--numstat", "-z", hash]);
    d.files = parse_changes(&run(root, &with_path(args, path)));

    let mut args = common.to_vec();
    args.extend(["--patch", HEAD_FORMAT, hash]);
    let (raw, cut) = run_limited(root, &with_path(args, path));
    let split = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut it = head.lines();
    let mut field = || it.next().unwrap_or("");
    let (full, author, when, refs, subject) = (field(), field(), field(), field(), field());
    d.lines.push(Line::new(LineKind::Title, clean(subject)));
    let mut meta = format!("{author} · {when}");
    if detailed {
        meta.push_str(&format!(" · {}", full.get(..10).unwrap_or(full)));
        if !refs.is_empty() {
            meta.push_str(&format!(" · {refs}"));
        }
    }
    d.lines.push(Line::new(LineKind::Meta, clean(&meta)));
    // One blank line before the body and between its paragraphs, none after.
    let mut gap = true;
    for l in it {
        if l.trim().is_empty() {
            gap = true;
            continue;
        }
        if std::mem::take(&mut gap) {
            d.lines.push(Line::new(LineKind::Info, ""));
        }
        d.lines.push(Line::new(LineKind::Info, clean(l)));
    }
    push_patch(&mut d, raw.get(split..).unwrap_or(&[]), detailed);
    finish(d, cut)
}

/// Everything that differs from the last commit: edits, staged or not, then
/// files git does not track yet.
pub fn worktree_detail(root: &Path, status: &Status, path: Option<&str>, detailed: bool) -> Detail {
    let mut d = Detail::default();
    let base = if status.oid == "(initial)" { EMPTY_TREE } else { "HEAD" };
    d.files = parse_changes(&run(root, &with_path(vec!["diff", base, "-M", "--raw", "--numstat", "-z"], path)));
    let staged: HashSet<&str> = status.files.iter().filter(|f| f.staged).map(|f| f.path.as_str()).collect();
    for f in &mut d.files {
        f.staged = staged.contains(f.path.as_str());
    }
    d.lines.push(Line::new(LineKind::Title, "Not committed yet"));
    d.lines.push(Line::new(LineKind::Meta, ""));
    let (raw, cut) = run_limited(root, &with_path(vec!["diff", base, "-M", "--patch"], path));
    push_patch(&mut d, &raw, detailed);
    let mut budget = PATCH_LIMIT as u64;
    for f in status.files.iter().filter(|f| f.untracked && path.is_none_or(|p| under(&f.path, p))) {
        push_new_file(&mut d, root, &f.path, &mut budget);
    }
    d.lines[1].text = match d.files.len() {
        1 => "1 file differs from the last commit".into(),
        n => format!("{n} files differ from the last commit"),
    };
    finish(d, cut)
}

/// Whether `path` is `filter` itself or lies inside it as a directory.
pub fn under(path: &str, filter: &str) -> bool {
    path.strip_prefix(filter).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}
