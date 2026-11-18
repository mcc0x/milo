//! Read-only git plumbing: which files are not yet committed or pushed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::when;

/// New files larger than this are listed without counting their lines.
const NEW_FILE_LIMIT: u64 = 256 << 10;
/// What an empty repository is compared against before its first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn run(root: &Path, args: &[&str]) -> Vec<u8> {
    // --no-optional-locks: a viewer must never contend with a real git command.
    Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        // A path given to git is a name, never a pattern.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default()
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
struct Status {
    branch: String,
    oid: String,
    upstream: Option<String>,
    ahead: u32,
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
        } else if let Some(v) = e.strip_prefix("# branch.ab +") {
            st.ahead = v.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        }
    }
    st
}

/// One file that is not yet pushed.
#[derive(PartialEq)]
pub struct FileChange {
    pub path: String,
    pub kind: Kind,
    pub add: u32,
    pub del: u32,
    /// Already committed, and only waiting to be pushed.
    pub committed: bool,
}

/// Parses `--raw --numstat -z` output: what happened to each path, then how
/// many lines each gained and lost.
fn parse_changes(raw: &[u8], committed: bool, files: &mut Vec<FileChange>) {
    let first = files.len();
    let text = String::from_utf8_lossy(raw);
    let mut tokens = text.split('\0').filter(|t| !t.is_empty());
    let mut next = first;
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
                files.push(FileChange { path: path.to_string(), kind, add: 0, del: 0, committed });
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
            files[first..].iter().position(|f| f.path == path).map(|i| first + i)
        };
        if let Some(i) = at {
            next = i + 1;
            // Binary files report "-" and keep their counts at zero.
            files[i].add = add.parse().unwrap_or(0);
            files[i].del = del.parse().unwrap_or(0);
        }
    }
}

/// Lines in a file git does not track yet; 0 if it is large or not text.
fn new_file_lines(path: &Path) -> u32 {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() <= NEW_FILE_LIMIT => {}
        _ => return 0,
    }
    match std::fs::read(path) {
        Ok(bytes) if !bytes.contains(&0) => bytes.split_inclusive(|&b| b == b'\n').count() as u32,
        _ => 0,
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

/// One commit on the current branch.
pub struct Commit {
    pub hash: String,
    pub subject: String,
    /// When it was committed, in seconds since the epoch.
    pub time: i64,
    /// The local calendar day of `time`, to tell one day's work from the next.
    pub day: i64,
    /// Already on the remote. Always true when there is no remote.
    pub pushed: bool,
    /// Joins two lines of history rather than changing anything itself.
    pub merge: bool,
}

/// Everything not yet pushed: first what is not committed, then what is
/// committed but still local.
fn pending(root: &Path, status: &Status, base: Option<&str>) -> Vec<FileChange> {
    let mut files = Vec::new();
    let head = if status.oid == "(initial)" { EMPTY_TREE } else { "HEAD" };
    parse_changes(&run(root, &["diff", head, "-M", "--raw", "--numstat", "-z"]), false, &mut files);
    for path in &status.untracked {
        let add = new_file_lines(&root.join(path));
        files.push(FileChange { path: path.clone(), kind: Kind::Untracked, add, del: 0, committed: false });
    }
    if let Some(base) = base {
        parse_changes(&run(root, &["diff", base, "HEAD", "-M", "--raw", "--numstat", "-z"]), true, &mut files);
    }
    files
}

/// When each pending file was last written, to notice edits cheaply.
type Stamp = Vec<Option<(SystemTime, u64)>>;

pub struct Repo {
    pub root: PathBuf,
    pub name: String,
    pub branch: String,
    /// False for a folder outside any repository: files are listed, and
    /// nothing is ever pending.
    pub git: bool,
    /// What this branch is pushed to, by name: its upstream, else the
    /// remote's default branch. Without one nothing is "unpushed".
    pub remote: Option<String>,
    /// The newest commit the remote already has, when this branch is ahead.
    base: Option<String>,
    /// What pending changes are measured against: `base`, else the last commit.
    against: String,
    raw: Vec<u8>,
    stamp: Stamp,
}

impl Repo {
    /// Opens the repository `dir` is in, or `dir` itself as a plain folder
    /// when it is in none. `None` if `dir` is not a directory.
    pub fn open(dir: &Path) -> Option<Repo> {
        let (root, git) = match toplevel(dir) {
            Some(top) => (PathBuf::from(top), true),
            None => (dir.canonicalize().ok().filter(|d| d.is_dir())?, false),
        };
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (branch, base, raw, stamp) = (String::new(), None, Vec::new(), Vec::new());
        Some(Repo { root, name, branch, git, remote: None, base, against: String::new(), raw, stamp })
    }

    fn status_raw(&self) -> Vec<u8> {
        run(&self.root, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"])
    }

    fn stamp(&self, files: &[FileChange]) -> Stamp {
        let of = |f: &FileChange| {
            let m = std::fs::metadata(self.root.join(&f.path)).ok()?;
            Some((m.modified().ok()?, m.len()))
        };
        files.iter().map(of).collect()
    }

    /// Reads what is pending afresh.
    pub fn load(&mut self) -> Vec<FileChange> {
        if !self.git {
            return Vec::new();
        }
        self.raw = self.status_raw();
        let status = parse_status(&self.raw);
        self.base = pushed_base(&self.root, &status);
        let head = if status.oid == "(initial)" { EMPTY_TREE } else { "HEAD" };
        self.against = self.base.clone().unwrap_or_else(|| head.to_string());
        let files = pending(&self.root, &status, self.base.as_deref());
        // Without an upstream, the remote's default branch still counts as one.
        let origin = || !run(&self.root, &["rev-parse", "--verify", "--quiet", "origin/HEAD"]).is_empty();
        self.remote = status.upstream.clone().or_else(|| origin().then(|| "origin".to_string()));
        self.branch = status.branch;
        self.stamp = self.stamp(&files);
        files
    }

    /// The whole of one file as a diff against what is already pushed, or
    /// against the last commit when nothing is waiting to be pushed: every
    /// line, with the lines it lost in place. Empty if it does not differ.
    pub fn file_diff(&self, path: &str) -> Vec<u8> {
        run(&self.root, &["diff", &self.against, "-M", "--unified=1000000", "--", path])
    }

    /// The newest `limit` commits on the current branch, newest first.
    pub fn history(&self, limit: usize) -> Vec<Commit> {
        if !self.git {
            return Vec::new();
        }
        let unpushed: HashSet<String> = match &self.base {
            Some(base) => {
                let raw = run(&self.root, &["rev-list", "-n", "1000", &format!("{base}..HEAD")]);
                String::from_utf8_lossy(&raw).lines().map(str::to_string).collect()
            }
            None => HashSet::new(),
        };
        let n = limit.to_string();
        let raw = run(&self.root, &["log", "-n", &n, "--no-show-signature", "--format=%H%x00%ct%x00%P%x00%s"]);
        String::from_utf8_lossy(&raw)
            .lines()
            .filter_map(|line| {
                let mut f = line.splitn(4, '\0');
                let (hash, time, parents, subject) = (f.next()?, f.next()?, f.next()?, f.next()?);
                let time = time.parse().unwrap_or(0);
                Some(Commit {
                    pushed: !unpushed.contains(hash),
                    hash: hash.to_string(),
                    subject: subject.to_string(),
                    time,
                    day: when::local_day(time),
                    merge: parents.contains(' '),
                })
            })
            .collect()
    }

    /// When the `i`th file of the last load was written, if it still exists.
    pub fn modified(&self, i: usize) -> Option<SystemTime> {
        self.stamp.get(i).copied().flatten().map(|(at, _)| at)
    }

    /// Whether anything moved since the last load: git's own view of the
    /// repository, or the content of a file that was already pending.
    pub fn stale(&self, files: &[FileChange]) -> bool {
        self.git && (self.status_raw() != self.raw || self.stamp(files) != self.stamp)
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
