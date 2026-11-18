//! Read-only git plumbing: what in a repository is not committed or pushed.

use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

/// Patches larger than this are cut off rather than loaded whole.
const PATCH_LIMIT: usize = 4 << 20;
/// Changed lines shown per file before the rest is summarised.
const FILE_LINE_CAP: usize = 3000;
/// New files larger than this are listed without their content.
const NEW_FILE_LIMIT: u64 = 256 << 10;
/// What an empty repository is compared against before its first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const TAB: usize = 4;

fn command(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    // --no-optional-locks: a viewer must never contend with a real git command.
    c.arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=never"])
        .args(args)
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

#[derive(Default)]
pub struct Status {
    pub branch: String,
    oid: String,
    upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    untracked: Vec<String>,
}

fn parse_status(raw: &[u8]) -> Status {
    let mut st = Status::default();
    for e in String::from_utf8_lossy(raw).split('\0') {
        if let Some(path) = e.strip_prefix("? ") {
            st.untracked.push(path.to_string());
        } else if let Some(v) = e.strip_prefix("# branch.oid ") {
            st.oid = v.to_string();
        } else if let Some(v) = e.strip_prefix("# branch.head ") {
            st.branch = v.to_string();
        } else if let Some(v) = e.strip_prefix("# branch.upstream ") {
            st.upstream = Some(v.to_string());
        } else if let Some(v) = e.strip_prefix("# branch.ab ") {
            let mut it = v.split(' ').map(|n| n.get(1..).and_then(|n| n.parse().ok()).unwrap_or(0));
            st.ahead = it.next().unwrap_or(0);
            st.behind = it.next().unwrap_or(0);
        }
    }
    st
}

#[derive(PartialEq)]
pub struct FileChange {
    pub path: String,
    pub kind: Kind,
    pub add: u32,
    pub del: u32,
    pub binary: bool,
    /// Where this file's section starts in the change view, if it is shown.
    pub line: Option<usize>,
}

impl FileChange {
    fn new(path: &str, kind: Kind) -> FileChange {
        FileChange { path: path.to_string(), kind, add: 0, del: 0, binary: false, line: None }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Heading of a group of files.
    Title,
    /// Secondary facts under a heading.
    Meta,
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

/// Files that share a state: not committed, or committed but not pushed.
#[derive(PartialEq)]
pub struct Group {
    pub title: &'static str,
    pub files: Range<usize>,
}

/// Everything that is not yet pushed, and optionally how it reads.
#[derive(Default, PartialEq)]
pub struct Detail {
    pub files: Vec<FileChange>,
    pub groups: Vec<Group>,
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
/// patch's own headers are dropped: the files from `first` on already say
/// what they say.
fn push_patch(d: &mut Detail, raw: &[u8], first: usize) {
    let text = String::from_utf8_lossy(raw);
    let (mut next, mut in_hunk, mut skip, mut shown) = (first, false, true, 0);
    let (mut old_no, mut new_no) = (0u32, 0u32);
    for l in text.lines() {
        if let Some(names) = l.strip_prefix("diff --git ") {
            let named = |f: &FileChange| names.strip_suffix(f.path.as_str()).is_some_and(|r| r.ends_with(" b/"));
            let i = if d.files.get(next).is_some_and(named) {
                next
            } else if let Some(i) = d.files[first..].iter().position(named) {
                first + i
            } else {
                let path = names.rsplit_once(" b/").map_or(names, |(_, p)| p);
                d.files.push(FileChange::new(path, Kind::Modified));
                d.files.len() - 1
            };
            next = i + 1;
            open_file(d, i);
            let f = &d.files[i];
            (in_hunk, shown) = (false, 0);
            skip = f.binary || f.kind == Kind::Deleted;
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

/// Adds a file git does not track yet; with `show`, as entirely new lines.
fn push_new_file(d: &mut Detail, root: &Path, path: &str, budget: &mut u64, show: bool) {
    d.files.push(FileChange::new(path, Kind::Untracked));
    let i = d.files.len() - 1;
    if show {
        open_file(d, i);
    }
    let note = |d: &mut Detail, text: &str| {
        if show {
            d.lines.push(Line::new(LineKind::Note, text));
        }
    };
    let full = root.join(path);
    let size = match std::fs::metadata(&full) {
        Ok(m) if m.is_file() => m.len(),
        _ => return,
    };
    if size > NEW_FILE_LIMIT.min(*budget) {
        return note(d, "A large new file; its content is not shown.");
    }
    let Ok(bytes) = std::fs::read(&full) else { return };
    *budget -= size.min(*budget);
    if bytes.contains(&0) {
        d.files[i].binary = true;
        return note(d, "Not text, so there is nothing to show.");
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut count = 0;
    for l in text.lines() {
        count += 1;
        if show && count <= FILE_LINE_CAP {
            let mut line = Line::new(LineKind::Add, clean(l));
            line.no = count as u32;
            d.lines.push(line);
        }
    }
    d.files[i].add = count as u32;
    if count > FILE_LINE_CAP {
        note(d, "A very large file; the rest is not shown.");
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

/// The commit that is already on the remote, if this branch is ahead of it.
/// A branch with no upstream is measured against the remote's default branch.
fn pushed_base(root: &Path, status: &Status) -> Option<String> {
    if status.oid == "(initial)" || (status.upstream.is_some() && status.ahead == 0) {
        return None;
    }
    let target = status.upstream.as_deref().unwrap_or("origin/HEAD");
    let base = String::from_utf8(run(root, &["merge-base", "HEAD", target])).ok()?;
    let base = base.trim();
    (!base.is_empty() && base != status.oid).then(|| base.to_string())
}

/// Collects everything not yet pushed: first what is not committed, then
/// what is committed but still local. With `patches`, also how each reads.
fn pending(root: &Path, status: &Status, patches: bool) -> Detail {
    let mut d = Detail::default();
    let mut cut = false;
    let head = if status.oid == "(initial)" { EMPTY_TREE } else { "HEAD" };

    d.files = parse_changes(&run(root, &["diff", head, "-M", "--raw", "--numstat", "-z"]));
    if patches {
        d.lines.push(Line::new(LineKind::Title, "Not committed yet"));
        let (raw, c) = run_limited(root, &["diff", head, "-M", "--patch"]);
        cut |= c;
        push_patch(&mut d, &raw, 0);
    }
    let mut budget = PATCH_LIMIT as u64;
    for path in &status.untracked {
        push_new_file(&mut d, root, path, &mut budget, patches);
    }
    if d.files.is_empty() {
        d.lines.clear();
    } else {
        d.groups.push(Group { title: "Not committed yet", files: 0..d.files.len() });
    }

    let first = d.files.len();
    if let Some(base) = pushed_base(root, status) {
        d.files.extend(parse_changes(&run(root, &["diff", &base, "HEAD", "-M", "--raw", "--numstat", "-z"])));
        if patches && d.files.len() > first {
            if !d.lines.is_empty() {
                d.lines.push(Line::new(LineKind::Info, ""));
            }
            d.lines.push(Line::new(LineKind::Title, "Not pushed yet"));
            let range = format!("{base}..HEAD");
            let subjects = run(root, &["log", "--format=%s", "-n", "20", &range]);
            for subject in String::from_utf8_lossy(&subjects).lines() {
                d.lines.push(Line::new(LineKind::Meta, format!("○ {}", clean(subject))));
            }
            let (raw, c) = run_limited(root, &["diff", &base, "HEAD", "-M", "--patch"]);
            cut |= c;
            push_patch(&mut d, &raw, first);
        }
    }
    if d.files.len() > first {
        d.groups.push(Group { title: "Not pushed yet", files: first..d.files.len() });
    }

    if cut {
        d.lines.push(Line::new(LineKind::Note, "This is too large to show in full."));
    }
    mark_changed(&mut d.lines);
    d.max_no = d.lines.iter().map(|l| l.no).max().unwrap_or(0);
    d
}

/// When each pending file was last written, to notice edits cheaply.
type Stamp = Vec<Option<(SystemTime, u64)>>;

pub struct Repo {
    pub root: PathBuf,
    pub name: String,
    pub status: Status,
    raw: Vec<u8>,
    stamp: Stamp,
}

impl Repo {
    pub fn open(root: PathBuf) -> Repo {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Repo { root, name, status: Status::default(), raw: Vec::new(), stamp: Vec::new() }
    }

    fn status_raw(&self) -> Vec<u8> {
        run(&self.root, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"])
    }

    fn stamp(&self, detail: &Detail) -> Stamp {
        let of = |f: &FileChange| {
            let m = std::fs::metadata(self.root.join(&f.path)).ok()?;
            Some((m.modified().ok()?, m.len()))
        };
        detail.files.iter().map(of).collect()
    }

    /// Reads what is pending afresh.
    pub fn load(&mut self, patches: bool) -> Detail {
        self.raw = self.status_raw();
        self.status = parse_status(&self.raw);
        let detail = pending(&self.root, &self.status, patches);
        self.stamp = self.stamp(&detail);
        detail
    }

    /// Whether anything moved since the last load: git's own view of the
    /// repository, or the content of a file that was already pending.
    pub fn stale(&self, detail: &Detail) -> bool {
        self.status_raw() != self.raw || self.stamp(detail) != self.stamp
    }

    /// Every tracked and untracked-but-not-ignored file, relative to the root.
    pub fn files(&self) -> Vec<String> {
        let raw = run(&self.root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"]);
        let mut files: Vec<String> =
            String::from_utf8_lossy(&raw).split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect();
        files.dedup();
        files
    }
}
