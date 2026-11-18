//! Discovers a repository and keeps its last known status, files and history.
//!
//! Pending work has not been pushed. It is uncommitted if it has not been
//! committed, and unpushed if it has been committed but not pushed.

mod parse;
pub(crate) mod run;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::read::{self, Content};
use crate::watch::Scope;
use parse::{Status, parse_files, parse_history, parse_status};
use run::OutputError;

/// New files larger than this are listed without counting their lines.
const NEW_FILE_LIMIT: u64 = 256 << 10;
/// A diff of one file larger than this is not loaded.
const DIFF_LIMIT: u64 = 8 << 20;
/// Maximum number of uncommitted paths passed to `git diff` by name.
/// For more paths, compare the whole tree to the last commit.
const PATHS_LIMIT: usize = 300;
/// The object name git reports before a branch has its first commit.
const UNBORN: &str = "(initial)";
/// The empty tree serves as the base until the first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// How a path differs, in order of precedence when a folder summarises its contents.
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

/// When a file was last written and how long it is: enough to notice an
/// edit. A link is stamped as itself, never as what it points to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stamp {
    pub written: SystemTime,
    len: u64,
}

impl Stamp {
    pub fn at(path: &Path) -> Option<Self> {
        let m = std::fs::symlink_metadata(path).ok()?;
        Some(Self { written: m.modified().ok()?, len: m.len() })
    }
}

/// A pending file in either the uncommitted or unpushed group.
#[derive(Clone, PartialEq, Debug)]
pub struct PendingFile {
    pub path: String,
    /// Where a moved file was before.
    from: Option<String>,
    pub kind: Kind,
    pub add: u32,
    pub del: u32,
    pub unpushed: bool,
    pub stamp: Option<Stamp>,
}

impl PendingFile {
    pub(crate) fn new(path: &str, kind: Kind) -> Self {
        Self { path: path.to_string(), from: None, kind, add: 0, del: 0, unpushed: false, stamp: None }
    }
}

/// Lines in a file git does not track yet; 0 if it is large or not text.
/// A link counts as the one line it is, and is never followed.
fn new_file_lines(path: &Path) -> u32 {
    if path.is_symlink() {
        return 1;
    }
    match read::file(path, NEW_FILE_LIMIT) {
        Content::Bytes(bytes) if !bytes.contains(&0) => {
            u32::try_from(bytes.split_inclusive(|&b| b == b'\n').count()).expect("256 KiB of lines fits in a u32")
        }
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
    /// False when there is no remote to compare with.
    pub unpushed: bool,
    /// A merge commit has more than one parent.
    pub merge: bool,
}

/// The local tip and, when it is ahead, the commit used as the comparison base.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Ends {
    base: Option<String>,
    tip: String,
}

impl Ends {
    fn has_commit(&self) -> bool {
        !self.tip.is_empty() && self.tip != UNBORN
    }

    fn head(&self) -> &'static str {
        if self.has_commit() { "HEAD" } else { EMPTY_TREE }
    }
}

/// The default branch used in place of an upstream, and where it points.
struct StandIn {
    name: String,
    oid: String,
}

/// What could make the merge base change between two status reads.
#[derive(PartialEq, Eq)]
struct Basis<'a> {
    tip: &'a str,
    target: Option<&'a str>,
    ahead: u32,
}

impl<'a> Basis<'a> {
    fn new(status: &'a Status, stand_in: Option<&'a StandIn>) -> Self {
        let target = stand_in.map(|r| r.oid.as_str()).or(status.upstream.as_deref());
        Self { tip: &status.ends.tip, target, ahead: status.ahead }
    }
}

#[derive(Default)]
struct Unpushed {
    ends: Ends,
    files: Vec<PendingFile>,
}

#[derive(Default)]
struct Tracked {
    index: Option<Stamp>,
    paths: String,
}

struct Counted {
    stamp: Option<Stamp>,
    lines: u32,
}

/// A rename's source and the state it was found in.
struct RenameLookup {
    path: String,
    ends: Ends,
    index: Option<Stamp>,
    from: Option<String>,
}

#[derive(Default)]
pub struct Repo {
    pub root: PathBuf,
    pub name: String,
    /// A plain folder has no git state.
    pub git: bool,
    ok: Cell<bool>,
    /// Where this checkout keeps its index, and where the repository keeps
    /// its branches. They differ in a linked worktree.
    git_dir: Option<PathBuf>,
    common_dir: Option<PathBuf>,
    /// Settings that switch off the repository's own filters.
    guard: Option<Vec<String>>,
    /// When set, status is limited to this path.
    file: Option<String>,
    stand_in: Option<StandIn>,
    /// The status last read, as git printed it and as parsed.
    raw: Vec<u8>,
    status: Status,
    /// A status `poll` found to differ, kept for `read` to take in.
    polled: Option<Vec<u8>>,
    /// Line counts of untracked files, kept while their stamp stays the same.
    counted: HashMap<String, Counted>,
    unpushed: Unpushed,
    tracked: Tracked,
    moved: Option<RenameLookup>,
}

/// Git versions without `--path-format` can still give the project root.
fn git_dirs(dir: &Path) -> Option<String> {
    let ask = |args: &[&str]| String::from_utf8(run::output(dir, &[], args)?).ok();
    ask(&["rev-parse", "--show-toplevel", "--absolute-git-dir", "--path-format=absolute", "--git-common-dir"])
        .or_else(|| ask(&["rev-parse", "--show-toplevel"]))
}

impl Repo {
    /// Opens the repository containing `dir`, or the directory as a plain folder.
    pub fn open(dir: &Path) -> Option<Self> {
        let found = git_dirs(dir);
        let mut lines = found.as_deref().unwrap_or("").lines().filter(|l| !l.is_empty()).map(PathBuf::from);
        let top = lines.next();
        let git = top.is_some();
        let root = match top {
            Some(top) => top,
            None => dir.canonicalize().ok().filter(|d| d.is_dir())?,
        };
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let guard = git.then(|| run::filter_guard(&root)).flatten();
        let ok = Cell::new(!git || guard.is_some());
        Some(Self { root, name, git, ok, git_dir: lines.next(), common_dir: lines.next(), guard, ..Self::default() })
    }

    /// Limits status to one file without narrowing the repository's root.
    pub fn only(&mut self, path: &str) {
        self.file = Some(path.to_string());
        self.polled = None;
    }

    pub fn branch(&self) -> &str {
        &self.status.branch
    }

    /// The upstream, or the name of the remote whose default branch stands in for it.
    pub fn remote(&self) -> Option<&str> {
        self.status.upstream.as_deref().or_else(|| self.stand_in.as_ref().map(|r| r.name.trim_end_matches("/HEAD")))
    }

    /// False when git could not be asked: what is shown may be out of date.
    pub fn ok(&self) -> bool {
        self.ok.get()
    }

    pub fn ends(&self) -> &Ends {
        &self.status.ends
    }

    /// Compare pending files to the base when there is one, otherwise to HEAD.
    fn against(&self) -> &str {
        self.ends().base.as_deref().unwrap_or_else(|| self.ends().head())
    }

    /// A required answer that git did not give must not replace what is known.
    fn require<T>(&self, answer: Option<T>) -> Option<T> {
        if answer.is_none() {
            self.ok.set(false);
        }
        answer
    }

    fn git(&self, args: &[&str]) -> Option<Vec<u8>> {
        self.require(run::output(&self.root, self.guard.as_deref()?, args))
    }

    fn optional_oid(&self, args: &[&str]) -> Option<Option<String>> {
        self.require(run::optional_oid(&self.root, self.guard.as_deref()?, args))
    }

    fn resolve(&self, name: &str) -> Option<Option<String>> {
        self.optional_oid(&["rev-parse", "--verify", "--quiet", "--end-of-options", name])
    }

    /// Watches the worktree and any shared Git directory outside it, as in a
    /// linked worktree. Exclusions are set at startup; if a directory stops
    /// being ignored, only the periodic check sees later changes inside it.
    pub fn watch_scope(&self) -> Scope {
        let mut roots = vec![self.root.clone()];
        roots.extend(self.common_dir.as_ref().filter(|dir| !dir.starts_with(&self.root)).cloned());
        // Older git versions do not say where their records are. Then only a
        // repository that plainly keeps them inside itself can be trusted.
        let inside = || std::fs::symlink_metadata(self.root.join(".git")).is_ok_and(|m| m.is_dir());
        let complete = !self.git || self.common_dir.is_some() || inside();

        // The Reader's status covers one file. A full status is useful only
        // for exclusions: if it fails, watching extra directories is safe.
        let full = self.file.as_ref().and_then(|_| self.status_raw(None)).map(|raw| parse_status(&raw));
        let status = full.as_ref().unwrap_or(&self.status);
        let internal = [self.git_dir.as_deref(), self.common_dir.as_deref()]
            .map(|dir| dir.and_then(|dir| dir.strip_prefix(&self.root).ok()));
        let file = self.file.as_deref().map(Path::new);
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

    fn status_raw(&self, path: Option<&str>) -> Option<Vec<u8>> {
        let args = [
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
            "--",
            path.unwrap_or(""),
        ];
        let args = if path.is_some() { &args[..] } else { &args[..args.len() - 2] };
        run::output(&self.root, self.guard.as_deref()?, args)
    }

    /// Asks git whether the repository state has moved and checks pending file stamps.
    /// Keeps a changed status for `read`, rather than asking git for it again.
    pub fn poll(&mut self, files: &[PendingFile]) -> bool {
        self.polled = None;
        if !self.git {
            return false;
        }
        let Some(raw) = self.require(self.status_raw(self.file.as_deref())) else { return false };
        let changed = !self.ok()
            || raw != self.raw
            || files.iter().any(|f| Stamp::at(&self.root.join(&f.path)) != f.stamp)
            || self.stand_in_moved();
        self.polled = changed.then_some(raw);
        changed
    }

    fn stand_in_moved(&self) -> bool {
        // A push or fetch moves the remote's branch without touching this one.
        self.stand_in
            .as_ref()
            .is_some_and(|r| self.resolve(&r.name).is_some_and(|oid| oid.as_deref() != Some(r.oid.as_str())))
    }

    /// Takes in the repository's status and comparison base, keeping both on failure.
    pub fn read(&mut self) {
        if !self.git {
            return;
        }
        let Some(raw) = self.polled.take().or_else(|| self.require(self.status_raw(self.file.as_deref()))) else {
            return;
        };
        let mut status = parse_status(&raw);
        let Some(stand_in) = self.stand_in(&status) else { return };
        if Basis::new(&status, stand_in.as_ref()) == Basis::new(&self.status, self.stand_in.as_ref()) {
            status.ends.base = self.status.ends.base.take();
        } else {
            let Some(base) = self.merge_base(&status, stand_in.as_ref()) else { return };
            status.ends.base = base;
        }
        (self.raw, self.status, self.stand_in) = (raw, status, stand_in);
        self.ok.set(true);
    }

    fn stand_in(&self, status: &Status) -> Option<Option<StandIn>> {
        if status.upstream.is_some() || !status.ends.has_commit() {
            return Some(None);
        }
        let remote = status.gone.as_deref().and_then(|u| u.split_once('/')).map_or("origin", |(remote, _)| remote);
        let name = format!("{remote}/HEAD");
        Some(self.resolve(&name)?.map(|oid| StandIn { name, oid }))
    }

    fn merge_base(&self, status: &Status, stand_in: Option<&StandIn>) -> Option<Option<String>> {
        // An upstream this branch is level with has nothing to compare.
        let target =
            status.upstream.as_deref().filter(|_| status.ahead > 0).or_else(|| stand_in.map(|r| r.name.as_str()));
        let Some(target) = target.filter(|_| status.ends.has_commit()) else { return Some(None) };
        let base = self.optional_oid(&["merge-base", "--end-of-options", "HEAD", target])?;
        Some(base.filter(|base| *base != status.ends.tip))
    }

    /// Returns pending files from the last `read`, uncommitted before unpushed.
    /// Returns `None` if git cannot answer.
    pub fn pending(&mut self) -> Option<Vec<PendingFile>> {
        if !self.ok() {
            return None;
        }
        let mut files = self.tracked_uncommitted()?;
        self.add_untracked(&mut files);
        self.add_unpushed(&mut files)?;
        Some(files)
    }

    fn tracked_uncommitted(&self) -> Option<Vec<PendingFile>> {
        let paths = &self.status.tracked_uncommitted;
        if paths.is_empty() {
            return Some(Vec::new());
        }
        // An edit during the diff must leave a different stamp for the next poll.
        let stamps: HashMap<&str, _> = paths.iter().map(|p| (p.as_str(), Stamp::at(&self.root.join(p)))).collect();
        let named = paths.len() <= PATHS_LIMIT;
        // Naming the paths lets the outline use the Reader's base without
        // including files that are only unpushed.
        let against = if named { self.against() } else { self.ends().head() };
        let args = ["diff", against, "-M", "--no-color", "--raw", "--numstat", "-z"];
        let raw = if named {
            let mut scoped = Vec::with_capacity(args.len() + 1 + paths.len());
            scoped.extend(args);
            scoped.push("--");
            scoped.extend(paths.iter().map(String::as_str));
            self.git(&scoped)?
        } else {
            self.git(&args)?
        };
        let mut files = parse_files(&raw);
        if named {
            self.add_missing_files(&mut files, &stamps);
        } else {
            // The whole-tree diff does not limit itself to these paths.
            files.retain(|file| stamps.contains_key(file.path.as_str()));
        }
        for file in &mut files {
            file.stamp = stamps.get(file.path.as_str()).copied().flatten();
        }
        self.mark_conflicts(&mut files);
        Some(files)
    }

    fn mark_conflicts(&self, files: &mut Vec<PendingFile>) {
        // Git can report a conflict as an ordinary edit or omit it from the diff.
        for path in &self.status.unmerged {
            match files.iter_mut().find(|f| f.path == *path) {
                Some(file) => file.kind = Kind::Conflict,
                None => files.push(PendingFile::new(path, Kind::Conflict)),
            }
        }
    }

    fn add_missing_files(&self, files: &mut Vec<PendingFile>, stamps: &HashMap<&str, Option<Stamp>>) {
        // A file put back to what the remote has can still differ from HEAD.
        for path in &self.status.tracked_uncommitted {
            if !self.status.sources.contains(path) && !files.iter().any(|f| f.path == *path) {
                let kind = if stamps[path.as_str()].is_some() { Kind::Modified } else { Kind::Deleted };
                files.push(PendingFile::new(path, kind));
            }
        }
    }

    fn add_untracked(&mut self, files: &mut Vec<PendingFile>) {
        let mut counted = HashMap::with_capacity(self.status.untracked.len());
        for path in &self.status.untracked {
            let full = self.root.join(path);
            let stamp = Stamp::at(&full);
            let lines = match self.counted.get(path) {
                Some(previous) if previous.stamp == stamp => previous.lines,
                _ => new_file_lines(&full),
            };
            counted.insert(path.clone(), Counted { stamp, lines });
            let mut file = PendingFile::new(path, Kind::Untracked);
            (file.add, file.stamp) = (lines, stamp);
            files.push(file);
        }
        self.counted = counted;
    }

    fn add_unpushed(&mut self, files: &mut Vec<PendingFile>) -> Option<()> {
        let ends = self.ends();
        if *ends != self.unpushed.ends {
            let parsed = match &ends.base {
                Some(base) => {
                    let args = ["diff", base, "HEAD", "-M", "--no-color", "--raw", "--numstat", "-z"];
                    parse_files(&self.git(&args)?)
                }
                None => Vec::new(),
            };
            self.unpushed = Unpushed { ends: ends.clone(), files: parsed };
        }
        files.extend(self.unpushed.files.iter().cloned().map(|mut file| {
            file.unpushed = true;
            file.stamp = Stamp::at(&self.root.join(&file.path));
            file
        }));
        Some(())
    }

    fn index_stamp(&self) -> Option<Stamp> {
        Stamp::at(&self.git_dir.as_ref()?.join("index"))
    }

    /// Tracked paths relative to the root. A failed refresh leaves the cache intact.
    pub fn tracked(&mut self) -> Option<impl Iterator<Item = &str>> {
        if !self.ok() {
            return None;
        }
        let index = self.index_stamp();
        if index.is_none() || index != self.tracked.index {
            let raw = self.git(&["ls-files", "-z", "--cached"])?;
            let paths = String::from_utf8(raw).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            self.tracked = Tracked { index, paths };
        }
        Some(self.tracked.paths.split_terminator('\0'))
    }

    pub fn is_untracked(&self, path: &str) -> bool {
        self.status.untracked.iter().any(|p| p == path)
    }

    pub fn is_unmerged(&self, path: &str) -> bool {
        self.status.unmerged.iter().any(|p| p == path)
    }

    /// Finds the source of a rename. The whole-tree comparison is cached
    /// for the same path, refs and index.
    pub fn moved_from(&mut self, path: &str) -> Option<String> {
        let index = self.index_stamp();
        let fresh = index.is_some()
            && self
                .moved
                .as_ref()
                .is_some_and(|known| known.path == path && &known.ends == self.ends() && known.index == index);
        if !fresh {
            let raw = self.git(&["diff", self.against(), "-M", "--no-color", "--raw", "-z"])?;
            let from = parse_files(&raw).into_iter().find(|f| f.path == path).and_then(|f| f.from);
            self.moved = Some(RenameLookup { path: path.to_string(), ends: self.ends().clone(), index, from });
        }
        self.moved.as_ref().filter(|known| known.path == path)?.from.clone()
    }

    /// Every line, including losses, against the base or last commit.
    /// `None` means too large to show or that git could not be asked.
    pub fn file_diff(&self, path: &str, from: Option<&str>) -> Option<Vec<u8>> {
        // The repository cannot substitute a program's output for this diff.
        let args = [
            "diff",
            self.against(),
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--unified=1000000",
            "--",
            from.unwrap_or(path),
            path,
        ];
        let args = if from.is_some() { &args[..] } else { &args[..args.len() - 1] };
        match run::limited_output(&self.root, self.guard.as_deref()?, args, DIFF_LIMIT) {
            Ok(diff) => Some(diff),
            // An oversized diff is not a git failure.
            Err(OutputError::TooLarge) => None,
            Err(OutputError::Failed) => self.require(None),
        }
    }

    /// The newest `limit` commits, newest first, or `None` on failure.
    pub fn history(&self, limit: usize) -> Option<Vec<Commit>> {
        if !self.ok() {
            return None;
        }
        if !self.ends().has_commit() {
            return Some(Vec::new());
        }
        let unpushed = match &self.ends().base {
            Some(base) => self.git(&["rev-list", "-n", "1000", &format!("{base}..HEAD")])?,
            None => Vec::new(),
        };
        let raw =
            self.git(&["log", "-n", &limit.to_string(), "--no-show-signature", "--format=%H%x00%ct%x00%P%x00%s"])?;
        Some(parse_history(&raw, &unpushed))
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
        repo.read();
        assert!(repo.ok());
        repo
    }

    fn assert_only_unpushed_page(files: &[PendingFile]) {
        assert_eq!(files.len(), 1, "{files:?}");
        let page = &files[0];
        assert_eq!((page.path.as_str(), page.unpushed, page.add, page.del), ("page.txt", true, 1, 1));
    }

    #[test]
    fn pushing_to_upstream_clears_the_pending_files() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.push();
        f.write("page.txt", "second\n");
        f.commit("second");

        let mut repo = read_repo(&f);
        assert_eq!(repo.remote(), Some("origin/main"));
        let pending = repo.pending().unwrap();
        assert_only_unpushed_page(&pending);
        assert!(repo.history(1).unwrap()[0].unpushed);

        f.push();
        assert!(repo.poll(&pending), "the upstream caught up");
        repo.read();
        assert_eq!(repo.remote(), Some("origin/main"));
        assert!(repo.pending().unwrap().is_empty());
        assert!(!repo.history(1).unwrap()[0].unpushed);
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
        assert_eq!(repo.remote(), Some("mirror/topic"));
        assert!(repo.pending().unwrap().is_empty());
        f.git(&["push", "-q", "--delete", "mirror", "topic"]);
        assert!(repo.poll(&[]), "the upstream branch was deleted");
        repo.read();
        assert_eq!(repo.remote(), Some("mirror"));
        assert_only_unpushed_page(&repo.pending().unwrap());
        let history = repo.history(2).unwrap();
        assert!(history[0].unpushed && !history[1].unpushed);
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
        assert_eq!(repo.remote(), None);
        let pending = repo.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let page = &pending[0];
        assert_eq!((page.path.as_str(), page.unpushed, page.add, page.del), ("page.txt", false, 1, 0));
        let history = repo.history(2).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|c| !c.unpushed));
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
        assert_eq!(repo.remote(), Some("origin"));
        let pending = repo.pending().unwrap();
        assert_only_unpushed_page(&pending);
        let history = repo.history(2).unwrap();
        assert!(history[0].unpushed && !history[1].unpushed);
        assert!(!repo.poll(&pending));

        f.git(&["push", "-q", "origin", "HEAD:main"]);
        assert!(repo.poll(&pending), "origin/HEAD moved without changing the worktree or local branch");
        repo.read();
        assert_eq!(repo.remote(), Some("origin"));
        assert!(repo.pending().unwrap().is_empty());
        assert!(!repo.history(1).unwrap()[0].unpushed);
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
        assert_eq!(repo.tracked().unwrap().collect::<Vec<_>>(), [".gitattributes", "page.txt"]);
        assert_eq!(repo.history(1).unwrap()[0].subject, "first");
        assert!(repo.file_diff("page.txt", None).unwrap().windows(b"+changed".len()).any(|s| s == b"+changed"));
        assert_eq!(repo.moved_from("page.txt"), None);
        assert!(!repo.poll(&pending));
        repo.only("page.txt");
        assert!(repo.watch_scope().complete);
        assert!(repo.ok());
        for marker in ["filter-ran", "hook-ran", "fsmonitor-ran"] {
            assert!(!f.root.join(".git").join(marker).exists(), "reading ran the driver for {marker}");
        }
    }

    #[test]
    fn read_takes_the_status_that_poll_kept() {
        let f = Fixture::new();
        let mut repo = read_repo(&f);
        f.write("new.txt", "first\n");
        assert!(repo.poll(&[]));

        f.write(".git/index", "corrupt index");
        repo.read();
        assert!(repo.ok());
        assert!(repo.is_untracked("new.txt"));
        repo.read();
        assert!(!repo.ok());
        assert!(repo.is_untracked("new.txt"));
    }

    #[test]
    fn a_failed_tracked_refresh_keeps_the_known_paths_and_index_stamp() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        let mut repo = read_repo(&f);
        assert_eq!(repo.tracked().unwrap().collect::<Vec<_>>(), ["page.txt"]);
        let previous = repo.tracked.index;
        f.write(".git/index", "corrupt index");
        assert!(repo.tracked().is_none());
        assert!(!repo.ok());
        assert_eq!(repo.tracked.paths, "page.txt\0");
        assert_eq!(repo.tracked.index, previous);
    }

    #[test]
    fn unpushed_files_can_still_be_shown_when_the_diff_objects_are_unavailable() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.push();
        f.write("page.txt", "second\n");
        f.commit("second");
        let mut warm = read_repo(&f);
        let mut cold = read_repo(&f);
        let pending = warm.pending().unwrap();
        assert_only_unpushed_page(&pending);

        // Hide the objects so a new diff cannot succeed.
        fs::rename(f.root.join(".git/objects"), f.root.join(".git/hidden-objects")).unwrap();
        assert!(cold.pending().is_none());
        assert!(!cold.ok());
        assert_eq!(warm.pending().unwrap(), pending);
        assert!(warm.ok());
    }
}
