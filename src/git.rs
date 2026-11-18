//! Read-only git plumbing: which files are not yet committed or pushed.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::Ordering;
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime};

use crate::read::{self, Content};
use crate::{term, watch::Scope, when};

/// New files larger than this are listed without counting their lines.
const NEW_FILE_LIMIT: u64 = 256 << 10;
/// A diff of one file larger than this is not loaded.
const DIFF_LIMIT: u64 = 8 << 20;
/// No answer from git is kept beyond this.
const OUTPUT_LIMIT: u64 = 256 << 20;
/// A git command that has not finished by now is given up on.
const DEADLINE: Duration = Duration::from_secs(15);
/// Up to this many changed paths are named to `git diff`; beyond it the
/// whole tree is compared.
const PATHS_LIMIT: usize = 300;
/// What an empty repository is compared against before its first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The git program itself. On macOS the `git` on the path is a launcher
/// that takes as long again to start as git does, so the real one is found
/// once. If it cannot be found, use `git` from the path.
pub(super) static GIT: LazyLock<PathBuf> = LazyLock::new(|| {
    let found = Command::new("git").arg("--exec-path").stdin(Stdio::null()).stderr(Stdio::null()).output();
    let core = found.ok().and_then(|o| String::from_utf8(o.stdout).ok()).unwrap_or_default();
    let real = Path::new(core.trim()).join("git");
    if real.is_file() { real } else { PathBuf::from("git") }
});

/// A git command that only reads, and that a repository cannot turn into
/// something else. A repository's own configuration can name programs for
/// git to run: a file-system monitor, hooks, filters, a transport for
/// fetching what is missing. Looking at a project must not run them, so
/// each is switched off here; `guard` does the same for the filters the
/// repository defines.
fn command(root: &Path, guard: &[String], args: &[&str]) -> Command {
    let mut c = Command::new(&*GIT);
    c.arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=false"])
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"])
        .args(["-c", "safe.bareRepository=explicit"])
        // Git otherwise checks the index on every core at once, which for a
        // viewer that looks often costs far more than it saves.
        .args(["-c", "core.preloadIndex=false"])
        // Comparing the whole tree must not write a refreshed index.
        .args(["-c", "diff.autoRefreshIndex=false"])
        .args(guard)
        .args(args)
        // A path given to git is a name, never a pattern.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(test)]
    crate::fixture::isolate_git(&mut c);
    c
}

fn output(command: Command, limit: u64) -> Option<Vec<u8>> {
    let (status, out) = run(command, limit)?;
    status.success().then_some(out)
}

/// Captures git's answer and exit status. A command that hangs, produces
/// too much output, or outlives a request to quit is killed: nothing here
/// may leave the program unable to respond.
fn run(mut command: Command, limit: u64) -> Option<(ExitStatus, Vec<u8>)> {
    let mut child = command.stdout(Stdio::piped()).spawn().ok()?;
    let mut pipe = child.stdout.take()?;
    let started = Instant::now();
    let (mut out, mut chunk) = (Vec::new(), [0u8; 1 << 16]);
    let complete = loop {
        if started.elapsed() > DEADLINE || term::TERMINATED.load(Ordering::Relaxed) {
            break false;
        }
        let mut fds = [libc::pollfd { fd: pipe.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if term::poll(&mut fds, 200) <= 0 {
            continue;
        }
        match pipe.read(&mut chunk) {
            Ok(0) => break true,
            Ok(n) if out.len() as u64 + n as u64 > limit => break false,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break false,
        }
    };
    if !complete {
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    complete.then_some((status, out))
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
    has_commit: bool,
    /// The upstream this branch is measured against.
    upstream: Option<String>,
    /// An upstream that is named but no longer exists.
    gone: Option<String>,
    ahead: u32,
    /// Tracked paths that differ, with where any moved file was before.
    changed: Vec<String>,
    /// Where the moved ones among them were before.
    sources: Vec<String>,
    untracked: Vec<String>,
    /// Directories git ignores whole; none of their contents are listed.
    ignored: Vec<String>,
    /// Paths left in conflict by a merge.
    unmerged: Vec<String>,
}

impl Status {
    fn head(&self) -> &'static str {
        if self.has_commit { "HEAD" } else { EMPTY_TREE }
    }
}

/// Parses `git status --porcelain=v2 --branch -z`.
fn parse_status(raw: &[u8]) -> Status {
    let mut st = Status::default();
    let text = String::from_utf8_lossy(raw);
    let mut entries = text.split('\0');
    // An upstream that no longer exists is still named, but not measured.
    let mut measured = false;
    // The path is the last of a fixed number of fields, and may hold spaces.
    let path = |entry: &str, fields: usize| entry.splitn(fields, ' ').nth(fields - 1).map(str::to_string);
    while let Some(e) = entries.next() {
        if let Some(path) = e.strip_prefix("? ") {
            // A nested repository is reported as a directory, with a slash.
            st.untracked.push(path.trim_end_matches('/').to_string());
        } else if let Some(dir) = e.strip_prefix("! ").and_then(|path| path.strip_suffix('/')) {
            st.ignored.push(dir.to_string());
        } else if e.starts_with("1 ") {
            st.changed.extend(path(e, 9));
        } else if e.starts_with("2 ") {
            // A rename is followed by its old path, which is not an entry.
            st.changed.extend(path(e, 10));
            let from = entries.next().unwrap_or_default();
            st.changed.push(from.to_string());
            st.sources.push(from.to_string());
        } else if e.starts_with("u ") {
            st.unmerged.extend(path(e, 11));
            st.changed.extend(path(e, 11));
        } else if let Some(v) = e.strip_prefix("# branch.oid ") {
            st.oid = v.to_string();
            st.has_commit = v != "(initial)";
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
        st.gone = st.upstream.take();
    }
    st
}

/// When a file was last written and how long it is: enough to notice an
/// edit. A link is stamped as itself, never as what it points to.
pub type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let m = std::fs::symlink_metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// One file that is not yet pushed.
#[derive(Clone, PartialEq, Debug)]
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
/// A link counts as the one line it is, and is never followed.
fn new_file_lines(path: &Path) -> u32 {
    if path.is_symlink() {
        return 1;
    }
    match read::file(path, NEW_FILE_LIMIT) {
        Content::Bytes(bytes) if !bytes.contains(&0) => bytes.split_inclusive(|&b| b == b'\n').count() as u32,
        _ => 0,
    }
}

/// The settings that switch off every filter a repository's own
/// configuration defines. A filter is a program git runs on file content;
/// one a project brought with it is not to be trusted with that.
fn filter_guard(root: &Path) -> Option<Vec<String>> {
    // Listing all keys succeeds even when there are no filters, unlike a pattern query.
    let listing = command(root, &[], &["config", "--null", "--name-only", "--list"]);
    let names = String::from_utf8(output(listing, OUTPUT_LIMIT)?).ok()?;
    // "filter.<name>.<setting>": the name may itself hold dots.
    let mut filters: Vec<&str> =
        names.split_terminator('\0').filter_map(|key| Some(key.strip_prefix("filter.")?.rsplit_once('.')?.0)).collect();
    filters.sort_unstable();
    filters.dedup();
    let mut guard = Vec::new();
    for name in filters {
        for setting in ["clean=", "smudge=", "process=", "required=false"] {
            guard.extend(["-c".to_string(), format!("filter.{name}.{setting}")]);
        }
    }
    Some(guard)
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
    /// default branch of its remote. Without one nothing is "unpushed".
    pub remote: Option<String>,
    /// False when git could not be asked: what is shown may be out of date.
    pub ok: bool,
    /// Where this checkout keeps its index, and where the repository keeps
    /// its branches. They differ in a linked worktree.
    git_dir: Option<PathBuf>,
    common_dir: Option<PathBuf>,
    /// Settings that switch off the repository's own filters.
    guard: Option<Vec<String>>,
    /// When set, only this path is asked about.
    scope: Option<String>,
    /// The commit the branch is on.
    tip: String,
    /// What stands in for an upstream when the branch has none, and the
    /// commit it was last seen on.
    stand_in: Option<(String, String)>,
    /// The newest commit the remote already has, when this branch is ahead,
    /// and what it was worked out from.
    base: Option<String>,
    basis: (String, Option<String>, u32),
    /// What pending changes are measured against: `base`, else the last commit.
    against: String,
    /// The status last read, as git printed it and as parsed.
    raw: Vec<u8>,
    status: Status,
    /// Line counts of untracked files, kept while their stamp stays the same.
    counted: HashMap<String, (Stamp, u32)>,
    /// What is committed but not pushed, kept while `base` and `tip` stay.
    committed: ((Option<String>, String), Vec<FileChange>),
    /// Every tracked file, kept while the index stays as it was.
    tracked: (Stamp, String),
    /// Where one file was moved from, and what that answer rests on.
    moved: Option<((String, String, String, Stamp), Option<String>)>,
}

impl Repo {
    /// Opens the repository `dir` is in, or `dir` itself as a plain folder
    /// when it is in none. `None` if `dir` is not a directory. Nothing is
    /// known about a repository until `read`.
    pub fn open(dir: &Path) -> Option<Repo> {
        let ask = |args: &[&str]| output(command(dir, &[], args), OUTPUT_LIMIT).and_then(|o| String::from_utf8(o).ok());
        // Older gits do not know --path-format; they still say where the top is.
        let found = ask(&["rev-parse", "--show-toplevel", "--absolute-git-dir", "--path-format=absolute", "--git-common-dir"])
            .or_else(|| ask(&["rev-parse", "--show-toplevel"]));
        let mut lines = found.as_deref().unwrap_or("").lines().filter(|l| !l.is_empty()).map(PathBuf::from);
        let (root, git) = match lines.next() {
            Some(top) => (top, true),
            None => (dir.canonicalize().ok().filter(|d| d.is_dir())?, false),
        };
        let (git_dir, common_dir) = (lines.next(), lines.next());
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let guard = git.then(|| filter_guard(&root)).flatten();
        let ok = !git || guard.is_some();
        Some(Repo {
            root,
            name,
            branch: String::new(),
            git,
            remote: None,
            ok,
            git_dir,
            common_dir,
            guard,
            scope: None,
            tip: String::new(),
            stand_in: None,
            base: None,
            basis: (String::new(), None, 0),
            against: "HEAD".to_string(),
            raw: Vec::new(),
            status: Status::default(),
            counted: HashMap::new(),
            committed: ((None, String::new()), Vec::new()),
            tracked: (None, String::new()),
            moved: None,
        })
    }

    /// Asks only about one path from now on: for a reader of a single file.
    pub fn only(&mut self, path: &str) {
        self.scope = Some(path.to_string());
    }

    fn git(&self, args: &[&str]) -> Option<Vec<u8>> {
        output(command(&self.root, self.guard.as_deref()?, args), OUTPUT_LIMIT)
    }

    /// Reads an object name. A missing ref or unrelated histories can
    /// legitimately return no object and exit with code 1.
    fn optional_oid(&self, args: &[&str]) -> Option<Option<String>> {
        let (status, raw) = run(command(&self.root, self.guard.as_deref()?, args), OUTPUT_LIMIT)?;
        if status.success() {
            let mut oid = String::from_utf8(raw).ok()?;
            oid.truncate(oid.trim_end().len());
            (!oid.is_empty()).then_some(Some(oid))
        } else {
            (status.code() == Some(1) && raw.is_empty()).then_some(None)
        }
    }

    /// Watches the worktree and any shared Git directory outside it, as in a
    /// linked worktree. Exclusions are set at startup; if a directory stops
    /// being ignored, only the periodic check sees later changes inside it.
    pub fn watch(&self) -> Scope {
        let mut roots = vec![self.root.clone()];
        roots.extend(self.common_dir.as_ref().filter(|dir| !dir.starts_with(&self.root)).cloned());
        // Older git versions do not say where their records are. Then only a
        // repository that plainly keeps them inside itself can be trusted.
        let inside = || std::fs::symlink_metadata(self.root.join(".git")).is_ok_and(|m| m.is_dir());
        let complete = !self.git || self.common_dir.is_some() || inside();

        // A reader's status covers one file, but the watch covers the whole tree.
        let full = self.scope.as_ref().and_then(|_| self.status_raw(None)).map(|raw| parse_status(&raw));
        let status = full.as_ref().unwrap_or(&self.status);
        let internal = [self.git_dir.as_deref(), self.common_dir.as_deref()]
            .map(|dir| dir.and_then(|dir| dir.strip_prefix(&self.root).ok()));
        let file = self.scope.as_deref().map(Path::new);
        let mut ignored: Vec<_> = status
            .ignored
            .iter()
            .map(Path::new)
            .filter_map(|dir| {
                let needed = Path::new(".git").starts_with(dir)
                    || internal.iter().flatten().any(|git| git.starts_with(dir))
                    || file.is_some_and(|file| file.starts_with(dir));
                (!needed).then(|| (dir.components().count(), dir))
            })
            .collect();
        ignored.sort_unstable();
        let ignored = ignored.into_iter().take(8).map(|(_, dir)| self.root.join(dir)).collect();
        Scope { roots, ignored, complete }
    }

    /// Whether git is in the middle of changing things here, so that what
    /// is on disk is half of one state and half of another.
    pub fn busy(&self) -> bool {
        self.git_dir.as_ref().is_some_and(|dir| dir.join("index.lock").exists())
    }

    fn status_raw(&self, scope: Option<&str>) -> Option<Vec<u8>> {
        let mut args = vec!["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all", "--ignored=matching"];
        if let Some(path) = scope {
            args.extend(["--", path]);
        }
        self.git(&args)
    }

    /// A fresh status if anything moved since the last `read`: git's own
    /// view of the repository, the content of one of `files`, or what stands
    /// in for an upstream. Hand it back to `read` so it is not asked for
    /// twice.
    pub fn poll(&mut self, files: &[FileChange]) -> Option<Vec<u8>> {
        if !self.git {
            return None;
        }
        let Some(raw) = self.status_raw(self.scope.as_deref()) else {
            self.ok = false;
            return None;
        };
        if !self.ok || raw != self.raw || files.iter().any(|f| stamp(&self.root.join(&f.path)) != f.stamp) {
            return Some(raw);
        }
        // A push or a fetch moves the remote's branch without touching this one.
        let (name, oid) = self.stand_in.as_ref()?;
        match self.optional_oid(&["rev-parse", "--verify", "--quiet", "--end-of-options", name]) {
            Some(found) => (found.as_ref() != Some(oid)).then_some(raw),
            None => {
                self.ok = false;
                None
            }
        }
    }

    /// Takes in the repository's state: the branch, what it is pushed to,
    /// and how far ahead it is. `raw` is a status from `poll`, if there is one.
    pub fn read(&mut self, raw: Option<Vec<u8>>) {
        if !self.git {
            return;
        }
        let Some(raw) = raw.or_else(|| self.status_raw(self.scope.as_deref())) else {
            self.ok = false;
            return;
        };
        let st = parse_status(&raw);
        let head = st.head();
        // Without an upstream, the default branch of the remote it had, or
        // of `origin`, still says what has been pushed.
        let stand_in = if st.upstream.is_none() && st.has_commit {
            let remote = st.gone.as_deref().and_then(|u| u.split_once('/')).map_or("origin", |(remote, _)| remote);
            let name = format!("{remote}/HEAD");
            let Some(oid) = self.optional_oid(&["rev-parse", "--verify", "--quiet", "--end-of-options", &name]) else {
                self.ok = false;
                return;
            };
            oid.map(|oid| (name, oid))
        } else {
            None
        };
        let target = st.upstream.as_ref().or_else(|| stand_in.as_ref().map(|(name, _)| name));

        // The base only moves when the tip, the target or the lead does.
        let seen = stand_in.as_ref().map(|(_, oid)| oid.clone()).or_else(|| st.upstream.clone());
        let basis = (st.oid.clone(), seen, st.ahead);
        if basis != self.basis {
            // An upstream that this branch is level with has nothing to compare.
            let ahead = st.upstream.is_none() || st.ahead > 0;
            self.base = match target.filter(|_| st.has_commit && ahead) {
                Some(target) => {
                    let Some(base) = self.optional_oid(&["merge-base", "--end-of-options", "HEAD", target]) else {
                        self.ok = false;
                        return;
                    };
                    base.filter(|base| *base != st.oid)
                }
                None => None,
            };
            self.basis = basis;
        }
        self.ok = true;
        self.remote = target.map(|t| {
            if st.upstream.is_some() { t.clone() } else { t.trim_end_matches("/HEAD").to_string() }
        });
        self.stand_in = stand_in;
        self.against = self.base.clone().unwrap_or_else(|| head.to_string());
        self.branch.clone_from(&st.branch);
        self.tip.clone_from(&st.oid);
        (self.raw, self.status) = (raw, st);
    }

    /// Everything not yet pushed, as of the last `read`: first what is not
    /// committed, then what is committed but still local. `None` leaves the
    /// last list in place when git could not be asked.
    pub fn pending(&mut self) -> Option<Vec<FileChange>> {
        if !self.git {
            return Some(Vec::new());
        }
        if !self.ok {
            return None;
        }
        let mut files = Vec::new();
        // Stamped before git looks, so an edit made meanwhile is not missed.
        let changed = &self.status.changed;
        let stamps: HashMap<&str, Stamp> = changed.iter().map(|p| (p.as_str(), stamp(&self.root.join(p)))).collect();
        if !changed.is_empty() {
            // Naming the paths saves comparing the rest of the tree. It
            // lets each file be measured against what is pushed, as the
            // reader shows it, without listing what is only committed.
            let named = changed.len() <= PATHS_LIMIT;
            let mut args = vec!["diff", if named { self.against.as_str() } else { self.status.head() }];
            args.extend(["-M", "--no-color", "--raw", "--numstat", "-z"]);
            if named {
                args.push("--");
                args.extend(changed.iter().map(String::as_str));
            }
            let Some(raw) = self.git(&args) else {
                self.ok = false;
                return None;
            };
            parse_changes(&raw, false, &mut files);
            if !named {
                // Without an index refresh the whole-tree diff can list
                // files that were touched without changing their contents.
                files.retain(|file| stamps.contains_key(file.path.as_str()));
            }
            // A file put back to what the remote has differs from nothing
            // there, and is work not committed all the same.
            let moved = &self.status.sources;
            for path in changed.iter().filter(|p| named && !moved.contains(p)) {
                if !files.iter().any(|f| f.path == *path) {
                    let kind = if stamps[path.as_str()].is_some() { Kind::Modified } else { Kind::Deleted };
                    files.push(FileChange::new(path, kind, false));
                }
            }
        }
        for file in &mut files {
            file.stamp = stamps.get(file.path.as_str()).copied().flatten();
        }
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

        // What is committed but not pushed only changes with the two ends.
        let ends = (self.base.clone(), self.tip.clone());
        if ends != self.committed.0 {
            let mut committed = Vec::new();
            if let Some(base) = &ends.0 {
                let args = ["diff", base, "HEAD", "-M", "--no-color", "--raw", "--numstat", "-z"];
                let Some(raw) = self.git(&args) else {
                    self.ok = false;
                    return None;
                };
                parse_changes(&raw, true, &mut committed);
            }
            self.committed = (ends, committed);
        }
        for file in &mut self.committed.1 {
            file.stamp = stamp(&self.root.join(&file.path));
        }
        files.extend(self.committed.1.iter().cloned());
        Some(files)
    }

    fn index_stamp(&self) -> Stamp {
        self.git_dir.as_ref().and_then(|dir| stamp(&dir.join("index")))
    }

    /// Every tracked file, relative to the root. Untracked ones are in
    /// `pending`.
    pub fn tracked(&mut self) -> impl Iterator<Item = &str> {
        let index = self.index_stamp();
        if index.is_none() || index != self.tracked.0 {
            match self.git(&["ls-files", "-z", "--cached"]) {
                Some(raw) => {
                    let files = String::from_utf8(raw)
                        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned());
                    self.tracked = (index, files);
                }
                None => self.ok = false,
            }
        }
        self.tracked.1.split_terminator('\0')
    }

    pub fn is_untracked(&self, path: &str) -> bool {
        self.status.untracked.iter().any(|p| p == path)
    }

    pub fn is_unmerged(&self, path: &str) -> bool {
        self.status.unmerged.iter().any(|p| p == path)
    }

    /// Where the file now at `path` was before, if it was moved there.
    /// Finding out means comparing the whole tree, so the answer is kept
    /// until something that could change it does.
    pub fn moved_from(&mut self, path: &str) -> Option<String> {
        let index = self.index_stamp();
        let key = (path.to_string(), self.against.clone(), self.tip.clone(), index);
        if index.is_none() || self.moved.as_ref().is_none_or(|(kept, _)| *kept != key) {
            if let Some(raw) = self.git(&["diff", &self.against, "-M", "--no-color", "--raw", "-z"]) {
                let mut files = Vec::new();
                parse_changes(&raw, false, &mut files);
                let from = files.into_iter().find(|f| f.path == path).and_then(|f| f.from);
                self.moved = Some((key, from));
            } else {
                self.ok = false;
            }
        }
        let ((known, _, _, _), from) = self.moved.as_ref()?;
        if known == path { from.clone() } else { None }
    }

    /// The whole of one file as a diff against what is already pushed, or
    /// against the last commit when nothing is waiting to be pushed: every
    /// line, with the lines it lost in place. `from` is where it was before,
    /// if it was moved. Empty if it does not differ, `None` if too large.
    pub fn file_diff(&self, path: &str, from: Option<&str>) -> Option<Vec<u8>> {
        // The repository's configuration must not swap the diff for a
        // program's output or colour it: this is read line by line.
        let mut args = vec!["diff", self.against.as_str(), "-M", "--no-color", "--no-ext-diff", "--no-textconv"];
        args.extend(["--unified=1000000", "--"]);
        args.extend(from);
        args.push(path);
        output(command(&self.root, self.guard.as_deref()?, &args), DIFF_LIMIT)
    }

    /// Changes whenever the history that `history` reports would.
    pub fn history_key(&self) -> (String, Option<String>) {
        (self.tip.clone(), self.base.clone())
    }

    /// The newest `limit` commits on the current branch, newest first.
    /// `None` means git could not be asked; keep the last answer in view.
    pub fn history(&mut self, limit: usize) -> Option<Vec<Commit>> {
        if !self.ok {
            return None;
        }
        if !self.git || !self.status.has_commit {
            // A branch before its first commit has no history to ask for.
            return Some(Vec::new());
        }
        let raw_unpushed = match &self.base {
            Some(base) => {
                let Some(raw) = self.git(&["rev-list", "-n", "1000", &format!("{base}..HEAD")]) else {
                    self.ok = false;
                    return None;
                };
                raw
            }
            None => Vec::new(),
        };
        let unpushed = String::from_utf8_lossy(&raw_unpushed);
        let unpushed: HashSet<&str> = unpushed.lines().collect();
        let n = limit.to_string();
        let args = ["log", "-n", &n, "--no-show-signature", "--format=%H%x00%ct%x00%P%x00%s"];
        let Some(raw) = self.git(&args) else {
            self.ok = false;
            return None;
        };
        let commits = String::from_utf8_lossy(&raw)
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
            .collect();
        Some(commits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;

    fn read_repo(f: &Fixture) -> Repo {
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read(None);
        assert!(repo.ok);
        repo
    }

    fn assert_only_page_to_push(files: &[FileChange]) {
        assert_eq!(files.len(), 1, "{files:?}");
        let page = &files[0];
        assert_eq!((page.path.as_str(), page.committed, page.add, page.del), ("page.txt", true, 1, 1));
    }

    #[test]
    fn changes_ahead_of_upstream_stop_being_pending_when_the_upstream_catches_up() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.push();
        f.write("page.txt", "second\n");
        f.commit("second");

        let mut repo = read_repo(&f);
        assert_eq!(repo.remote.as_deref(), Some("origin/main"));
        let pending = repo.pending().unwrap();
        assert_only_page_to_push(&pending);
        assert!(!repo.history(1).unwrap()[0].pushed);

        f.push();
        let raw = repo.poll(&pending).expect("the upstream caught up");
        repo.read(Some(raw));
        assert_eq!(repo.remote.as_deref(), Some("origin/main"));
        assert!(repo.pending().unwrap().is_empty());
        assert!(repo.history(1).unwrap()[0].pushed);
    }

    #[test]
    fn a_deleted_upstream_falls_back_to_its_own_remotes_default_branch() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.push();
        f.git(&["remote", "rename", "origin", "mirror"]);
        f.git(&["remote", "set-head", "mirror", "main"]);
        f.git(&["switch", "-q", "-c", "topic"]);
        f.write("page.txt", "second\n");
        f.commit("second");
        f.git(&["push", "-q", "-u", "mirror", "topic"]);

        let mut repo = read_repo(&f);
        assert_eq!(repo.remote.as_deref(), Some("mirror/topic"));
        assert!(repo.pending().unwrap().is_empty());
        f.git(&["push", "-q", "--delete", "mirror", "topic"]);
        let raw = repo.poll(&[]).expect("the upstream branch was deleted");
        repo.read(Some(raw));
        assert_eq!(repo.remote.as_deref(), Some("mirror"));
        assert_only_page_to_push(&repo.pending().unwrap());
        let history = repo.history(2).unwrap();
        assert!(!history[0].pushed && history[1].pushed);
    }

    #[test]
    fn without_a_remote_only_work_since_the_last_commit_is_pending() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.write("page.txt", "first\nsecond\n");
        f.commit("second");
        f.write("page.txt", "first\nsecond\nthird\n");

        let mut repo = read_repo(&f);
        assert_eq!(repo.remote, None);
        let pending = repo.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let page = &pending[0];
        assert_eq!((page.path.as_str(), page.committed, page.add, page.del), ("page.txt", false, 1, 0));
        let history = repo.history(2).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|c| c.pushed));
    }

    #[test]
    fn a_branch_without_an_upstream_uses_origin_head_and_notices_a_push_to_it() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.push();
        f.git(&["remote", "set-head", "origin", "main"]);
        f.git(&["switch", "-q", "-c", "topic"]);
        f.write("page.txt", "second\n");
        f.commit("second");

        let mut repo = read_repo(&f);
        assert_eq!(repo.remote.as_deref(), Some("origin"));
        let pending = repo.pending().unwrap();
        assert_only_page_to_push(&pending);
        let history = repo.history(2).unwrap();
        assert!(!history[0].pushed && history[1].pushed);
        assert!(repo.poll(&pending).is_none());

        f.git(&["push", "-q", "origin", "HEAD:main"]);
        let raw = repo.poll(&pending).expect("origin/HEAD moved without changing the worktree or local branch");
        repo.read(Some(raw));
        assert_eq!(repo.remote.as_deref(), Some("origin"));
        assert!(repo.pending().unwrap().is_empty());
        assert!(repo.history(1).unwrap()[0].pushed);
    }

    #[test]
    fn reading_a_repository_does_not_run_its_filters_hooks_or_file_monitor() {
        let f = Fixture::new();
        f.write(".gitattributes", "*.txt filter=mark\n");
        f.write("page.txt", "first\n");
        f.commit("first");
        f.write("page.txt", "changed\n");

        f.write(".git/test-hooks/post-index-change", "#!/bin/sh\ntouch .git/hook-ran\n");
        fs::set_permissions(f.root.join(".git/test-hooks/post-index-change"), Permissions::from_mode(0o755)).unwrap();
        f.git(&["config", "core.hooksPath", ".git/test-hooks"]);
        f.git(&["config", "filter.mark.clean", "touch .git/filter-ran; cat"]);
        f.git(&["config", "filter.mark.required", "true"]);
        f.git(&["config", "core.fsmonitor", "touch .git/fsmonitor-ran"]);

        let mut repo = read_repo(&f);
        let pending = repo.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, "page.txt");
        assert_eq!(repo.tracked().collect::<Vec<_>>(), [".gitattributes", "page.txt"]);
        assert_eq!(repo.history(1).unwrap()[0].subject, "first");
        assert!(repo.file_diff("page.txt", None).unwrap().windows(b"+changed".len()).any(|s| s == b"+changed"));
        assert_eq!(repo.moved_from("page.txt"), None);
        assert!(repo.poll(&pending).is_none());
        repo.only("page.txt");
        assert!(repo.watch().complete);
        assert!(repo.ok);
        for marker in ["filter-ran", "hook-ran", "fsmonitor-ran"] {
            assert!(!f.root.join(".git").join(marker).exists(), "reading ran the driver for {marker}");
        }
    }

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
    fn status_keeps_only_ignored_directories_apart_from_changes() {
        let st = parse_status(
            b"! cache with spaces/\0! build.log\0? scratch.txt\0\
              1 .M N... 100644 100644 100644 h1 h2 source.rs\0",
        );
        assert_eq!(st.ignored, ["cache with spaces"]);
        assert_eq!(st.untracked, ["scratch.txt"]);
        assert_eq!(st.changed, ["source.rs"]);
        assert!(st.unmerged.is_empty());
    }

    #[test]
    fn status_sets_aside_an_upstream_that_is_gone() {
        // Git still names a deleted upstream but prints no ahead/behind for it.
        let st = parse_status(b"# branch.oid abc\0# branch.head topic\0# branch.upstream gh/topic\0");
        assert_eq!((st.upstream, st.gone.as_deref()), (None, Some("gh/topic")));
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
    fn status_lists_every_tracked_path_that_differs() {
        let st = parse_status(
            b"1 .M N... 100644 100644 100644 h1 h2 src/a b.rs\0\
              2 R. N... 100644 100644 100644 h1 h2 R100 new name.txt\0old name.txt\0\
              u UU N... 100644 100644 100644 100644 h1 h2 h3 both.txt\0? loose.txt\0",
        );
        assert_eq!(st.changed, ["src/a b.rs", "new name.txt", "old name.txt", "both.txt"]);
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
