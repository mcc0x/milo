//! Read-only git plumbing: which files are not yet committed or pushed.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::read::{self, Content};
use crate::when;

/// New files larger than this are listed without counting their lines.
const NEW_FILE_LIMIT: u64 = 256 << 10;
/// A diff of one file larger than this is not loaded.
const DIFF_LIMIT: u64 = 8 << 20;
/// What an empty repository is compared against before its first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// A git command that only reads, and that a repository cannot turn into
/// something else: a repository's own configuration may name programs for
/// git to run, and looking at a project must not run them.
fn command(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    // --no-optional-locks: a viewer must never contend with a real git command.
    c.arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=false"])
        .args(["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit"])
        .args(args)
        // A path given to git is a name, never a pattern.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    c
}

fn run(root: &Path, args: &[&str]) -> Vec<u8> {
    command(root, args).output().map(|o| o.stdout).unwrap_or_default()
}

/// Runs git and keeps its output only if that is at most `limit` bytes.
fn run_capped(root: &Path, args: &[&str], limit: u64) -> Option<Vec<u8>> {
    let mut child = command(root, args).stdout(Stdio::piped()).spawn().ok()?;
    let mut out = Vec::new();
    let read = child.stdout.take()?.take(limit + 1).read_to_end(&mut out);
    let fits = read.is_ok() && out.len() as u64 <= limit;
    if !fits {
        let _ = child.kill();
    }
    let _ = child.wait();
    fits.then_some(out)
}

fn toplevel(dir: &Path) -> Option<String> {
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
    /// Paths left in conflict by a merge.
    unmerged: Vec<String>,
}

/// Parses `git status --porcelain=v2 --branch -z`.
fn parse_status(raw: &[u8]) -> Status {
    let mut st = Status::default();
    let text = String::from_utf8_lossy(raw);
    let mut entries = text.split('\0');
    // An upstream that no longer exists is still named, but not measured.
    let mut measured = false;
    while let Some(e) = entries.next() {
        if let Some(path) = e.strip_prefix("? ") {
            // A nested repository is reported as a directory, with a slash.
            st.untracked.push(path.trim_end_matches('/').to_string());
        } else if let Some(rest) = e.strip_prefix("u ") {
            // "u XY sub m1 m2 m3 mW h1 h2 h3 path": the path may hold spaces.
            st.unmerged.extend(rest.splitn(10, ' ').nth(9).map(str::to_string));
        } else if e.starts_with("2 ") {
            // A rename is followed by its old path, which is not an entry.
            entries.next();
        } else if let Some(v) = e.strip_prefix("# branch.oid ") {
            st.oid = v.to_string();
        } else if let Some(v) = e.strip_prefix("# branch.head ") {
            st.branch = v.to_string();
        } else if let Some(v) = e.strip_prefix("# branch.upstream ") {
            st.upstream = Some(v.to_string());
        } else if let Some(v) = e.strip_prefix("# branch.ab +") {
            measured = true;
            st.ahead = v.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        }
    }
    if !measured {
        st.upstream = None;
    }
    st
}

/// When a file was last written and how long it is: enough to notice an edit.
pub type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// One file that is not yet pushed.
#[derive(PartialEq, Debug)]
pub struct FileChange {
    pub path: String,
    /// Where a moved file was before.
    pub from: Option<String>,
    pub kind: Kind,
    pub add: u32,
    pub del: u32,
    /// Already committed, and only waiting to be pushed.
    pub committed: bool,
    pub stamp: Stamp,
}

impl FileChange {
    fn new(path: &str, kind: Kind, committed: bool) -> FileChange {
        FileChange { path: path.to_string(), from: None, kind, add: 0, del: 0, committed, stamp: None }
    }
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
            let code = tok.rsplit(' ').next().and_then(|c| c.bytes().next()).unwrap_or(b'M');
            let kind = match code {
                b'A' | b'C' => Kind::Added,
                b'D' => Kind::Deleted,
                b'U' => Kind::Conflict,
                b'R' => Kind::Renamed,
                _ => Kind::Modified,
            };
            // Renames and copies list the old path first.
            let old = if matches!(code, b'R' | b'C') { tokens.next() } else { None };
            if let Some(path) = tokens.next() {
                let mut file = FileChange::new(path, kind, committed);
                file.from = old.filter(|_| code == b'R').map(str::to_string);
                files.push(file);
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
    match read::file(path, NEW_FILE_LIMIT) {
        Content::Bytes(bytes) if !bytes.contains(&0) => bytes.split_inclusive(|&b| b == b'\n').count() as u32,
        _ => 0,
    }
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
    /// The commit the branch is on.
    tip: String,
    /// The newest commit the remote already has, when this branch is ahead.
    base: Option<String>,
    /// What pending changes are measured against: `base`, else the last commit.
    against: String,
    /// The status last read, as git printed it and as parsed.
    raw: Vec<u8>,
    status: Status,
    /// Line counts of untracked files, kept while their stamp stays the same.
    counted: HashMap<String, (Stamp, u32)>,
}

impl Repo {
    /// Opens the repository `dir` is in, or `dir` itself as a plain folder
    /// when it is in none. `None` if `dir` is not a directory. Nothing is
    /// known about a repository until `read`.
    pub fn open(dir: &Path) -> Option<Repo> {
        let (root, git) = match toplevel(dir) {
            Some(top) => (PathBuf::from(top), true),
            None => (dir.canonicalize().ok().filter(|d| d.is_dir())?, false),
        };
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Some(Repo {
            root,
            name,
            branch: String::new(),
            git,
            remote: None,
            tip: String::new(),
            base: None,
            against: "HEAD".to_string(),
            raw: Vec::new(),
            status: Status::default(),
            counted: HashMap::new(),
        })
    }

    /// Whether everything git does here happens under `root`, where a watch
    /// on `root` can see it. A linked worktree or a submodule keeps its
    /// history elsewhere.
    pub fn self_contained(&self) -> bool {
        !self.git || self.root.join(".git").is_dir()
    }

    fn status_raw(&self) -> Vec<u8> {
        run(&self.root, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"])
    }

    /// A fresh status if anything moved since the last `read`: git's own
    /// view of the repository, or the content of one of `files`. Hand it
    /// back to `read` so it is not asked for twice.
    pub fn poll(&self, files: &[FileChange]) -> Option<Vec<u8>> {
        if !self.git {
            return None;
        }
        let raw = self.status_raw();
        let moved = raw != self.raw || files.iter().any(|f| stamp(&self.root.join(&f.path)) != f.stamp);
        moved.then_some(raw)
    }

    /// Takes in the repository's state: the branch, what it is pushed to,
    /// and how far ahead it is. `raw` is a status from `poll`, if there is one.
    pub fn read(&mut self, raw: Option<Vec<u8>>) {
        if !self.git {
            return;
        }
        self.raw = raw.unwrap_or_else(|| self.status_raw());
        let st = parse_status(&self.raw);
        let born = st.oid != "(initial)";
        // Without an upstream, the remote's default branch still counts as one.
        let origin = || !run(&self.root, &["rev-parse", "--verify", "--quiet", "origin/HEAD"]).is_empty();
        let target = st.upstream.clone().or_else(|| (born && origin()).then(|| "origin/HEAD".to_string()));
        self.remote = st.upstream.clone().or_else(|| target.as_ref().map(|_| "origin".to_string()));
        // An upstream that this branch is level with has nothing to compare.
        let ahead = st.upstream.is_none() || st.ahead > 0;
        self.base = target.filter(|_| born && ahead).and_then(|target| {
            let base = String::from_utf8(run(&self.root, &["merge-base", "HEAD", &target])).ok()?;
            let base = base.trim();
            (!base.is_empty() && base != st.oid).then(|| base.to_string())
        });
        let head = if born { "HEAD" } else { EMPTY_TREE };
        self.against = self.base.clone().unwrap_or_else(|| head.to_string());
        self.branch.clone_from(&st.branch);
        self.tip.clone_from(&st.oid);
        self.status = st;
    }

    /// Everything not yet pushed, as of the last `read`: first what is not
    /// committed, then what is committed but still local.
    pub fn pending(&mut self) -> Vec<FileChange> {
        if !self.git {
            return Vec::new();
        }
        let mut files = Vec::new();
        let head = if self.tip == "(initial)" { EMPTY_TREE } else { "HEAD" };
        parse_changes(&run(&self.root, &["diff", head, "-M", "--raw", "--numstat", "-z"]), false, &mut files);
        // A conflict reads as an ordinary edit in that diff, or not at all.
        for path in &self.status.unmerged {
            match files.iter_mut().find(|f| f.path == *path) {
                Some(file) => file.kind = Kind::Conflict,
                None => files.push(FileChange::new(path, Kind::Conflict, false)),
            }
        }
        let mut counted = HashMap::with_capacity(self.status.untracked.len());
        for path in &self.status.untracked {
            let full = self.root.join(path);
            let now = stamp(&full);
            let lines = match self.counted.get(path) {
                Some(&(then, lines)) if then == now => lines,
                _ => new_file_lines(&full),
            };
            counted.insert(path.clone(), (now, lines));
            let mut file = FileChange::new(path, Kind::Untracked, false);
            (file.add, file.stamp) = (lines, now);
            files.push(file);
        }
        self.counted = counted;
        if let Some(base) = &self.base {
            parse_changes(&run(&self.root, &["diff", base, "HEAD", "-M", "--raw", "--numstat", "-z"]), true, &mut files);
        }
        for file in files.iter_mut().filter(|f| f.kind != Kind::Untracked) {
            file.stamp = stamp(&self.root.join(&file.path));
        }
        files
    }

    /// Every tracked file, relative to the root. Untracked ones are in
    /// `pending`.
    pub fn tracked(&self) -> Vec<String> {
        let raw = run(&self.root, &["ls-files", "-z", "--cached"]);
        let mut files: Vec<String> =
            String::from_utf8_lossy(&raw).split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect();
        // A file in conflict is listed once per side.
        files.dedup();
        files
    }

    pub fn is_untracked(&self, path: &str) -> bool {
        self.status.untracked.iter().any(|p| p == path)
    }

    pub fn is_unmerged(&self, path: &str) -> bool {
        self.status.unmerged.iter().any(|p| p == path)
    }

    /// Where the file now at `path` was before, if it was moved there.
    pub fn moved_from(&self, path: &str) -> Option<String> {
        let mut files = Vec::new();
        parse_changes(&run(&self.root, &["diff", &self.against, "-M", "--raw", "-z"]), false, &mut files);
        files.into_iter().find(|f| f.path == path)?.from
    }

    /// The whole of one file as a diff against what is already pushed, or
    /// against the last commit when nothing is waiting to be pushed: every
    /// line, with the lines it lost in place. `from` is where it was before,
    /// if it was moved. Empty if it does not differ, `None` if too large.
    pub fn file_diff(&self, path: &str, from: Option<&str>) -> Option<Vec<u8>> {
        // The repository's configuration must not swap the diff for a
        // program's output or colour it: this is read line by line.
        let mut args =
            vec!["diff", &self.against, "-M", "--no-ext-diff", "--no-textconv", "--unified=1000000", "--"];
        args.extend(from);
        args.push(path);
        run_capped(&self.root, &args, DIFF_LIMIT)
    }

    /// Changes whenever the history that `history` reports would.
    pub fn history_key(&self) -> (String, Option<String>) {
        (self.tip.clone(), self.base.clone())
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(raw: &str) -> Vec<FileChange> {
        let mut files = Vec::new();
        parse_changes(raw.as_bytes(), false, &mut files);
        files
    }

    #[test]
    fn status_reads_branch_and_untracked() {
        let st = parse_status(
            b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0? a b.txt\0? nested/\0",
        );
        assert_eq!((st.branch.as_str(), st.oid.as_str(), st.ahead), ("main", "abc", 2));
        assert_eq!(st.upstream.as_deref(), Some("origin/main"));
        assert_eq!(st.untracked, ["a b.txt", "nested"]);
    }

    #[test]
    fn status_drops_an_upstream_that_is_gone() {
        // Git still names a deleted upstream but prints no ahead/behind for it.
        let st = parse_status(b"# branch.oid abc\0# branch.head topic\0# branch.upstream origin/topic\0");
        assert_eq!(st.upstream, None);
    }

    #[test]
    fn status_reads_conflicts_and_skips_rename_sources() {
        let st = parse_status(
            b"u UU N... 100644 100644 100644 100644 h1 h2 h3 both sides.txt\0\
              2 R. N... 100644 100644 100644 h1 h2 R100 new.txt\0? old looks untracked\0? real.txt\0",
        );
        assert_eq!(st.unmerged, ["both sides.txt"]);
        assert_eq!(st.untracked, ["real.txt"]);
    }

    #[test]
    fn changes_pair_kinds_with_counts() {
        let files = changes(
            ":100644 100644 a b M\0src/a.rs\0:000000 100644 0 c A\0new file.txt\0:100644 000000 d 0 D\0gone\0\
             3\t1\tsrc/a.rs\x005\t0\tnew file.txt\x000\t7\tgone\0",
        );
        let got: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.kind, f.add, f.del)).collect();
        assert_eq!(
            got,
            [("src/a.rs", Kind::Modified, 3, 1), ("new file.txt", Kind::Added, 5, 0), ("gone", Kind::Deleted, 0, 7)]
        );
    }

    #[test]
    fn changes_keep_where_a_moved_file_was() {
        let files = changes(":100644 100644 a b R090\0old/name.rs\0new/name.rs\x002\t1\t\0old/name.rs\0new/name.rs\0");
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].path.as_str(), files[0].kind), ("new/name.rs", Kind::Renamed));
        assert_eq!(files[0].from.as_deref(), Some("old/name.rs"));
        assert_eq!((files[0].add, files[0].del), (2, 1));
    }

    #[test]
    fn changes_leave_binary_counts_at_zero() {
        let files = changes(":100644 100644 a b M\0logo.png\0-\t-\tlogo.png\0");
        assert_eq!((files[0].add, files[0].del), (0, 0));
    }

    #[test]
    fn changes_survive_a_truncated_entry() {
        assert!(changes(":100644 100644 a b \0").is_empty());
    }
}
