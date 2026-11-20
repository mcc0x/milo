//! Discovers a repository and keeps its last known status, files and history.
//!
//! Pending work has not been pushed. It is uncommitted if it has not been
//! committed, and unpushed if it has been committed but not pushed.

mod parse;
mod pending;
pub(crate) mod run;

use std::cell::{Cell, OnceCell};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::read::{self, Content};
use crate::watch::{Events, GitFilter, Ignored, Scope};
use parse::{Status, parse_files, parse_history, parse_status};
use pending::{DiffStamp, TrackedDiff, tracked_records};
use run::OutputError;

/// A diff of one file larger than this is not loaded.
const DIFF_LIMIT: u64 = 8 << 20;
pub(crate) const GIT_UNAVAILABLE: &str = "Git could not be run.";
/// Maximum number of uncommitted paths passed to `git diff` by name.
/// For more paths, compare the whole tree to the last commit.
const PATHS_LIMIT: usize = 300;
/// The object name git reports before a branch has its first commit.
const UNBORN: &str = "(initial)";

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
pub struct FileStamp {
    pub written: SystemTime,
    len: u64,
}

impl FileStamp {
    pub fn at(path: &Path) -> Option<Self> {
        let m = std::fs::symlink_metadata(path).ok()?;
        Self::of(&m)
    }

    pub(crate) fn of(m: &std::fs::Metadata) -> Option<Self> {
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
    /// The outline has not yet tried to count the lines of this new file.
    pub(crate) lines_pending: bool,
    pub unpushed: bool,
    pub stamp: Option<FileStamp>,
}

impl PendingFile {
    pub(crate) fn new(path: &str, kind: Kind) -> Self {
        Self {
            path: path.to_string(),
            from: None,
            kind,
            add: 0,
            del: 0,
            lines_pending: kind == Kind::Untracked,
            unpushed: false,
            stamp: None,
        }
    }
}

/// A Git diff's openable changes and any names the panes cannot represent.
#[derive(Default)]
pub(crate) struct DiffFiles {
    pub(crate) files: Vec<PendingFile>,
    pub(crate) unrepresented: usize,
}

/// An object name supplied by the caller, not a branch name or revision expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitId<'a>(&'a str);

impl<'a> CommitId<'a> {
    pub fn parse(text: &'a str) -> Option<Self> {
        ((4..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())).then_some(Self(text))
    }
}

/// The resolved name and date of a commit opened by its ID.
pub struct CommitVersion {
    pub hash: String,
    pub time: i64,
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
struct Ends {
    base: Option<String>,
    tip: String,
}

impl Ends {
    fn has_commit(&self) -> bool {
        !self.tip.is_empty() && self.tip != UNBORN
    }
}

/// The default branch to try in place of an upstream, even before it exists.
#[derive(PartialEq, Eq)]
struct StandIn {
    name: String,
    oid: Option<String>,
}

impl StandIn {
    fn remote(&self) -> Option<&str> {
        self.oid.as_ref()?;
        self.name.strip_prefix("refs/remotes/")?.strip_suffix("/HEAD")
    }
}

#[derive(Default, PartialEq, Eq)]
struct Comparison {
    upstream: Option<String>,
    stand_in: Option<StandIn>,
}

impl Comparison {
    fn oid(&self) -> Option<&str> {
        self.upstream.as_deref().or_else(|| self.stand_in.as_ref()?.oid.as_deref())
    }
}

/// A poll keeps the same status and comparison for the subsequent read.
struct Snapshot {
    raw: Vec<u8>,
    status: Option<Status>,
    comparison: Comparison,
    shallow: Option<DiffStamp>,
}

#[derive(Default)]
enum ReadFor {
    #[default]
    Pending,
    History,
}

/// The state a loaded history list was drawn from.
pub struct HistoryVersion {
    pub branch: String,
    tip: String,
    target: Option<String>,
    shallow: Option<DiffStamp>,
    pub ahead: Option<u32>,
}

impl HistoryVersion {
    pub fn matches(&self, repo: &Repo) -> bool {
        self.tip == repo.ends().tip
            && self.branch == repo.branch()
            && self.target.as_deref() == repo.history_target()
            && self.shallow == repo.shallow
            && self.ahead == repo.upstream_ahead()
    }
}

#[derive(Default)]
struct Unpushed {
    ends: Ends,
    diff: DiffFiles,
}

#[derive(Default)]
struct Tracked {
    index: Option<DiffStamp>,
    paths: String,
    incomplete: bool,
}

#[derive(Default)]
pub struct Repo {
    pub root: PathBuf,
    pub name: String,
    /// Whether Git identified a worktree.
    pub git: bool,
    git_unavailable: bool,
    ok: Cell<bool>,
    /// Where this checkout keeps its index, and where the repository keeps
    /// its branches. They differ in a linked worktree.
    git_dir: Option<PathBuf>,
    common_dir: Option<PathBuf>,
    shallow_file: Option<PathBuf>,
    shallow: Option<DiffStamp>,
    empty_tree: OnceCell<String>,
    index_lock: Option<PathBuf>,
    config: run::Config,
    read_for: ReadFor,
    /// When set, status is limited to this path.
    file: Option<String>,
    comparison: Comparison,
    /// The status last read, as git printed it and as parsed.
    raw: Vec<u8>,
    status: Status,
    /// Shared with the watcher until Git reports different ignored paths.
    watch_ignored: Arc<Ignored>,
    /// A status `poll` found to differ, kept for `read` to take in.
    polled: Option<Snapshot>,
    unpushed: Unpushed,
    pending_tracked: Option<TrackedDiff>,
    tracked: Tracked,
}

struct GitDirs {
    root: PathBuf,
    git: Option<PathBuf>,
    common: Option<PathBuf>,
}

/// Find just the worktree identity, without loading project state.
pub fn worktree(dir: &Path) -> Option<PathBuf> {
    discover_worktree(dir).ok().flatten()
}

/// Distinguish a missing executable from an ordinary directory.
pub(crate) fn discover_worktree(dir: &Path) -> Result<Option<PathBuf>, &'static str> {
    let raw = discovery_output(dir, &["rev-parse", "--show-toplevel"])?;
    Ok(raw.map(|raw| PathBuf::from(OsStr::from_bytes(&raw))).filter(|root| root.is_absolute()))
}

/// What people call a checkout: its repository and, in a linked worktree, that worktree's folder.
pub struct Checkout {
    pub root: PathBuf,
    pub project: String,
    pub worktree: Option<String>,
}

/// Name the checkout containing `dir`, without loading project state. An
/// error means Git could not be asked, which says nothing about `dir`.
pub fn checkout(dir: &Path) -> Result<Option<Checkout>, &'static str> {
    let Some(dirs) = git_dirs(dir)? else { return Ok(None) };
    let Some(folder) = dirs.root.file_name().map(|name| name.to_string_lossy().into_owned()) else {
        return Ok(None);
    };
    // A linked worktree keeps its index apart from the repository's branches.
    let repository = match (&dirs.git, &dirs.common) {
        (Some(git), Some(common)) if git != common => repository_name(common),
        _ => None,
    };
    Ok(Some(match repository {
        Some(project) if project != folder => Checkout { root: dirs.root, project, worktree: Some(folder) },
        _ => Checkout { root: dirs.root, project: folder, worktree: None },
    }))
}

/// The folder holding `.git`, or a bare repository's own folder without its `.git` ending.
fn repository_name(common: &Path) -> Option<String> {
    let own = common.file_name()?.to_string_lossy();
    if own.starts_with('.') {
        return Some(common.parent()?.file_name()?.to_string_lossy().into_owned());
    }
    Some(own.strip_suffix(".git").filter(|name| !name.is_empty()).unwrap_or(&own).to_owned())
}

fn discovery_output(dir: &Path, args: &[&str]) -> Result<Option<Vec<u8>>, &'static str> {
    match run::limited_output(dir, &[], args, 1 << 16) {
        Ok(mut raw) if raw.last() == Some(&b'\n') => {
            raw.pop();
            Ok(Some(raw))
        }
        Err(OutputError::Unavailable) => Err(GIT_UNAVAILABLE),
        _ => Ok(None),
    }
}

fn git_dirs(dir: &Path) -> Result<Option<GitDirs>, &'static str> {
    let ask = |args: &[&str]| discovery_output(dir, args);
    let path = |bytes: &[u8]| PathBuf::from(OsStr::from_bytes(bytes));
    let one = |args: &[&str]| ask(args).map(|raw| raw.as_deref().map(path));
    let raw =
        ask(&["rev-parse", "--show-toplevel", "--absolute-git-dir", "--path-format=absolute", "--git-common-dir"])?;
    let Some(raw) = raw else {
        // Older Git can still give the project root.
        return Ok(discover_worktree(dir)?.map(|root| GitDirs { root, git: None, common: None }));
    };
    let mut paths = raw.split(|&b| b == b'\n');
    if let (Some(root), Some(git), Some(common), None) = (paths.next(), paths.next(), paths.next(), paths.next()) {
        return Ok(Some(GitDirs { root: path(root), git: Some(path(git)), common: Some(path(common)) }));
    }
    // A newline can be part of a path. Separate answers have no ambiguous delimiters.
    let Some(root) = discover_worktree(dir)? else { return Ok(None) };
    Ok(Some(GitDirs {
        root,
        git: one(&["rev-parse", "--absolute-git-dir"])?,
        common: one(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?,
    }))
}

impl Repo {
    /// Opens the repository containing `dir`, or the directory as a plain folder.
    pub fn open(dir: &Path) -> Option<Self> {
        let discovered = git_dirs(dir);
        let git_unavailable = discovered.is_err();
        let (root, git, git_dir, common_dir) = match discovered.ok().flatten() {
            Some(found) => (found.root, true, found.git, found.common),
            None => (dir.canonicalize().ok().filter(|d| d.is_dir())?, false, None, None),
        };
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let index_lock = git_dir.as_ref().map(|dir| dir.join("index.lock"));
        let shallow_file = common_dir.as_ref().map(|dir| dir.join("shallow"));
        let filter = git_dir.as_deref().zip(common_dir.as_deref()).and_then(|(git, common)| GitFilter::new(git, common));
        let mut repo = Self {
            root,
            name,
            git,
            git_unavailable,
            ok: Cell::new(true),
            git_dir,
            common_dir,
            shallow_file,
            index_lock,
            ..Self::default()
        };
        if let Some(filter) = filter {
            Arc::get_mut(&mut repo.watch_ignored).unwrap().git = Some(filter);
        }
        Some(repo)
    }

    pub(crate) fn git_unavailable(&self) -> bool {
        self.git_unavailable
    }

    pub(crate) fn common_dir(&self) -> Option<&Path> {
        self.common_dir.as_deref()
    }

    /// Rediscover a plain folder when Git's directory or gitfile appears.
    pub(crate) fn discover_git(&self) -> Option<Self> {
        if self.git || !self.root.join(".git").exists() {
            return None;
        }
        Self::open(&self.root).filter(|repo| repo.git)
    }

    /// History compares commits directly. Only pending file diffs need a merge base.
    pub fn for_history(mut self) -> Self {
        self.read_for = ReadFor::History;
        self
    }

    /// Limits status to one file without narrowing the repository's root.
    pub fn only(&mut self, path: &str) {
        self.file = Some(path.to_string());
        self.polled = None;
        if !self.status.ignored_dirs.is_empty() || !self.status.ignored_files.is_empty() {
            self.watch_ignored = self.exclusions(&self.status);
        }
    }

    pub fn branch(&self) -> &str {
        &self.status.branch
    }

    /// The upstream, or the name of the remote whose default branch stands in for it.
    pub fn remote(&self) -> Option<&str> {
        self.status.upstream.as_deref().or_else(|| self.comparison.stand_in.as_ref().and_then(StandIn::remote))
    }

    /// A remote can be configured even when there is no branch to compare with.
    pub fn has_remote(&self) -> bool {
        self.config.has_remote
    }

    fn guard(&self) -> &[String] {
        &self.config.guard
    }

    /// False when git could not be asked: what is shown may be out of date.
    pub fn ok(&self) -> bool {
        self.ok.get()
    }

    fn ends(&self) -> &Ends {
        &self.status.ends
    }

    /// The empty tree depends on the repository's object format.
    fn empty_tree(&self) -> Option<&str> {
        if let Some(oid) = self.empty_tree.get() {
            return Some(oid);
        }
        let oid = self.require(self.optional_oid(&["hash-object", "-t", "tree", "--stdin"])?)?;
        Some(self.empty_tree.get_or_init(|| oid))
    }

    /// Compare pending files to the base when there is one, otherwise to the sampled tip.
    fn against(&self) -> Option<&str> {
        let ends = self.ends();
        match ends.base.as_deref() {
            Some(base) => Some(base),
            None if ends.has_commit() => Some(&ends.tip),
            None => self.empty_tree(),
        }
    }

    /// A required answer that git did not give must not replace what is known.
    fn require<T>(&self, answer: Option<T>) -> Option<T> {
        if answer.is_none() {
            self.ok.set(false);
        }
        answer
    }

    fn git(&self, args: &[&str]) -> Option<Vec<u8>> {
        self.require(run::output(&self.root, self.guard(), args))
    }

    fn optional_oid(&self, args: &[&str]) -> Option<Option<String>> {
        self.require(run::optional_oid(&self.root, &[], args))
    }

    fn resolve(&self, name: &str) -> Option<Option<String>> {
        self.optional_oid(&["rev-parse", "--verify", "--quiet", "--end-of-options", name])
    }

    /// Watches the worktree and any shared Git directory outside it, as in a
    /// linked worktree. The current status supplies the ignored paths.
    pub fn watch_scope(&self) -> Scope {
        let mut roots = vec![self.root.clone()];
        for dir in [self.common_dir.as_ref(), self.git_dir.as_ref()].into_iter().flatten() {
            if !roots.iter().any(|root| dir.starts_with(root)) {
                roots.push(dir.clone());
            }
        }
        // Older git versions do not say where their records are. Then only a
        // repository that plainly keeps them inside itself can be trusted.
        let inside = || std::fs::symlink_metadata(self.root.join(".git")).is_ok_and(|m| m.is_dir());
        let complete = !self.git || self.common_dir.is_some() || inside();
        Scope { roots, ignored: Arc::clone(&self.watch_ignored), complete }
    }

    /// A Reader needs only its file, applicable worktree rules and Git's metadata.
    pub(crate) fn reader_filter(&self, relative: &Path) -> Arc<Ignored> {
        let file = self.root.join(relative);
        // Don't guess how a volume normalizes or folds non-ASCII path names.
        if !file.as_os_str().as_encoded_bytes().is_ascii() {
            return Arc::new(Ignored { git: self.watch_ignored.git.clone(), ..Ignored::default() });
        }
        let worktree = self.git.then(|| self.root.clone());
        let metadata = [self.git.then(|| self.root.join(".git")), self.git_dir.clone(), self.common_dir.clone()];
        Arc::new(Ignored {
            git: self.watch_ignored.git.clone(),
            events: Events::Reader { file, worktree, metadata },
            ..Ignored::default()
        })
    }

    fn exclusions(&self, status: &Status) -> Arc<Ignored> {
        // A file-limited status can miss exclusions, which only widens the watch.
        let [git, common] = [self.git_dir.as_deref(), self.common_dir.as_deref()]
            .map(|dir| dir.and_then(|dir| dir.strip_prefix(&self.root).ok()));
        let metadata = [Some(Path::new(".git")), git, common];
        let file = self.file.as_deref().map(Path::new);
        let exclusions = |paths: &[PathBuf]| {
            paths
                .iter()
                .filter(|path| {
                    // Ignore rules can ignore themselves, but changing them or
                    // Git's metadata can make a previously ignored path visible.
                    let ignore_rules = path
                        .file_name()
                        .is_some_and(|name| name.as_encoded_bytes().eq_ignore_ascii_case(b".gitignore"));
                    let needed = ignore_rules
                        || metadata.iter().flatten().any(|git| git.starts_with(path) || path.starts_with(git))
                        || file.is_some_and(|file| file.starts_with(path));
                    !needed
                })
                .map(|path| self.root.join(path))
                .collect()
        };
        Arc::new(Ignored {
            dirs: exclusions(&status.ignored_dirs),
            files: exclusions(&status.ignored_files),
            git: self.watch_ignored.git.clone(),
            ..Ignored::default()
        })
    }

    /// Whether git is in the middle of changing things here, so that what
    /// is on disk is half of one state and half of another.
    pub fn busy(&self) -> bool {
        self.index_lock.as_ref().is_some_and(|path| path.symlink_metadata().is_ok())
    }

    fn status_raw(&mut self) -> Option<Vec<u8>> {
        // Filters added while the pane is open must be disabled too.
        self.config = self.require(run::config(&self.root))?;
        let path = self.file.as_deref();
        let args = [
            "status",
            "--porcelain=v2",
            "--branch",
            "--ahead-behind",
            "--ignore-submodules=dirty",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
            "--",
            path.unwrap_or(""),
        ];
        let args = if path.is_some() { &args[..] } else { &args[..args.len() - 2] };
        self.require(run::output(&self.root, self.guard(), args))
    }

    fn snapshot(&mut self) -> Option<Snapshot> {
        let shallow = self.shallow_file.as_deref().map(DiffStamp::at);
        let raw = self.status_raw()?;
        let status = (raw != self.raw).then(|| parse_status(&raw));
        let comparison = self.comparison(status.as_ref().unwrap_or(&self.status))?;
        Some(Snapshot { raw, status, comparison, shallow })
    }

    /// Asks git whether the repository state has moved and checks tracked file stamps.
    /// The outline samples untracked stamps separately. Keeps changed status for `read`.
    pub fn poll(&mut self, files: &[PendingFile]) -> bool {
        self.polled = None;
        if !self.git {
            return false;
        }
        let had_remote = self.has_remote();
        let Some(snapshot) = self.snapshot() else { return false };
        let changed = !self.ok()
            || had_remote != self.has_remote()
            || snapshot.status.is_some()
            || snapshot.comparison != self.comparison
            || snapshot.shallow != self.shallow
            || self.tracked_diff_moved()
            || files.iter().any(|f| {
                f.kind != Kind::Untracked
                    && (f.unpushed || self.pending_tracked.is_none())
                    && FileStamp::at(&self.root.join(&f.path)) != f.stamp
            });
        self.polled = changed.then_some(snapshot);
        changed
    }

    /// Polls and takes in changed status, leaving nothing for a later read.
    pub(crate) fn read_if_changed(&mut self) -> bool {
        let changed = self.poll(&[]);
        if changed {
            self.read();
        }
        changed
    }

    /// Takes in the repository's status and comparison base, keeping both on failure.
    pub fn read(&mut self) {
        if !self.git {
            return;
        }
        let Some(Snapshot { raw, status, comparison, shallow }) = self.polled.take().or_else(|| self.snapshot()) else {
            return;
        };
        let current = status.as_ref().unwrap_or(&self.status);
        let tip_changed = current.ends.tip != self.ends().tip;
        let graph_changed = shallow != self.shallow;
        let basis_changed = tip_changed || graph_changed || comparison.oid() != self.comparison.oid();
        let base = match self.read_for {
            ReadFor::History => None,
            ReadFor::Pending if basis_changed => {
                let Some(base) = self.merge_base(&current.ends.tip, comparison.oid()) else { return };
                base
            }
            ReadFor::Pending => self.status.ends.base.take(),
        };
        let base_changed = (basis_changed || matches!(self.read_for, ReadFor::History)) && base != self.ends().base;
        if self.pending_tracked.is_some()
            && (!self.ok()
                || base_changed
                || tip_changed
                || (status.is_some() && !tracked_records(&raw).eq(tracked_records(&self.raw))))
        {
            self.invalidate_tracked_diff();
        }
        if let Some(status) = status {
            if status.ignored_dirs != self.status.ignored_dirs || status.ignored_files != self.status.ignored_files {
                self.watch_ignored = self.exclusions(&status);
            }
            (self.raw, self.status) = (raw, status);
        }
        self.status.ends.base = base;
        (self.comparison, self.shallow) = (comparison, shallow);
        self.ok.set(true);
    }

    fn comparison(&self, status: &Status) -> Option<Comparison> {
        if !status.ends.has_commit() {
            return Some(Comparison::default());
        }
        if status.upstream.is_some() {
            // A short upstream name can resolve to an unrelated tag or branch.
            let upstream = if status.ahead > 0 {
                let name = format!("{}@{{upstream}}^{{commit}}", status.branch);
                Some(self.require(self.resolve(&name)?)?)
            } else {
                None
            };
            return Some(Comparison { upstream, stand_in: None });
        }
        let stand_in = if self.has_remote() {
            let remote = status.gone.as_deref().and_then(|u| u.split_once('/')).map_or("origin", |(remote, _)| remote);
            let name = format!("refs/remotes/{remote}/HEAD");
            let oid = self.resolve(&name)?;
            Some(StandIn { name, oid })
        } else {
            None
        };
        Some(Comparison { upstream: None, stand_in })
    }

    fn merge_base(&self, tip: &str, target: Option<&str>) -> Option<Option<String>> {
        let Some(target) = target.filter(|&target| target != tip) else { return Some(None) };
        self.optional_oid(&["merge-base", "--end-of-options", tip, target])
    }

    /// Returns pending files from the last `read`, uncommitted before unpushed.
    /// Returns `None` if git cannot answer.
    pub fn pending(&mut self) -> Option<Vec<PendingFile>> {
        if !self.ok() {
            return None;
        }
        let mut files = self.tracked_uncommitted()?;
        files.extend(self.status.untracked.iter().map(|path| PendingFile::new(path, Kind::Untracked)));
        self.add_unpushed(&mut files)?;
        Some(files)
    }

    fn add_unpushed(&mut self, files: &mut Vec<PendingFile>) -> Option<()> {
        let ends = self.ends();
        if *ends != self.unpushed.ends {
            let parsed = match &ends.base {
                Some(base) => {
                    let args = ["diff", base, &ends.tip, "-M", "--raw", "--numstat", "-z", "--"];
                    parse_files(&self.git(&args)?)
                }
                None => DiffFiles::default(),
            };
            self.unpushed = Unpushed { ends: ends.clone(), diff: parsed };
        }
        files.extend(self.unpushed.diff.files.iter().cloned().map(|mut file| {
            file.unpushed = true;
            file.stamp = FileStamp::at(&self.root.join(&file.path));
            file
        }));
        Some(())
    }

    /// Some pathname bytes cannot be used by the panes' UTF-8 path model.
    pub(crate) fn paths_incomplete(&self) -> bool {
        self.status.unrepresented != 0 || self.tracked.incomplete || self.unpushed.diff.unrepresented != 0
    }

    /// Tracked paths relative to the root. A failed refresh leaves the cache intact.
    pub fn tracked(&mut self) -> Option<impl Iterator<Item = &str>> {
        if !self.ok() {
            return None;
        }
        let index = self.diff_index();
        if index.is_none() || index != self.tracked.index {
            let raw = self.git(&["ls-files", "-z", "--cached"])?;
            let (paths, incomplete) = match String::from_utf8(raw) {
                Ok(paths) => (paths, false),
                Err(invalid) => {
                    let mut paths = String::new();
                    for path in invalid.as_bytes().split(|&b| b == 0).filter_map(parse::utf8_path) {
                        paths.push_str(path);
                        paths.push('\0');
                    }
                    (paths, true)
                }
            };
            self.tracked = Tracked { index, paths, incomplete };
        }
        Some(self.tracked.paths.split_terminator('\0'))
    }

    pub fn is_untracked(&self, path: &str) -> bool {
        self.status.untracked.iter().any(|p| p == path)
    }

    pub fn is_unmerged(&self, path: &str) -> bool {
        self.status.unmerged.iter().any(|p| p == path)
    }

    /// Rename similarity depends on the current worktree contents, not just its index.
    pub fn moved_from(&self, path: &str) -> Option<String> {
        let raw = self.git(&["diff", self.against()?, "-M", "--name-status", "-z", "--"])?;
        parse::changed_file(&raw, path)??.from
    }

    /// Every line, including losses, against the base or last commit.
    /// `None` means too large to show or that git could not be asked.
    pub fn file_diff(&self, path: &str, from: Option<&str>) -> Option<Vec<u8>> {
        match run::working_diff(&self.root, self.guard(), self.against()?, path, from) {
            Ok(diff) => Some(diff),
            // An oversized diff is not a git failure.
            Err(OutputError::TooLarge) => None,
            Err(OutputError::Failed | OutputError::Unavailable) => self.require(None),
        }
    }

    /// Counts paths with work to commit, counting a move once.
    pub fn uncommitted_count(&self) -> usize {
        self.status.tracked_uncommitted.len() - self.status.sources.len()
            + self.status.untracked.len()
            + self.status.unrepresented
    }

    /// The full number ahead of the upstream, if this branch has one.
    fn upstream_ahead(&self) -> Option<u32> {
        self.status.upstream.as_ref().map(|_| self.status.ahead)
    }

    pub fn history_version(&self) -> HistoryVersion {
        HistoryVersion {
            branch: self.branch().to_owned(),
            tip: self.ends().tip.clone(),
            target: self.history_target().map(str::to_owned),
            shallow: self.shallow,
            ahead: self.upstream_ahead(),
        }
    }

    fn history_target(&self) -> Option<&str> {
        self.comparison.oid().filter(|&oid| oid != self.ends().tip)
    }

    /// Finds the full object name and date without reading the work tree.
    pub fn resolve_commit(&self, id: CommitId<'_>) -> Option<CommitVersion> {
        let raw = self.git(&[
            "log", "-1", "--no-walk", "--no-show-signature", "--no-notes", "--no-decorate", "--no-patch",
            "--format=%H%x00%ct", "--end-of-options", id.0, "--",
        ])?;
        parse::commit_version(&raw, id)
    }

    /// Git treats a shallow boundary as a root. Without the real parent we
    /// cannot tell what changed there.
    fn has_parent_history(&self, hash: &str) -> bool {
        self.shallow_file.as_ref().is_none_or(|file| match read::file(file, DIFF_LIMIT) {
            Content::Gone => true,
            Content::Bytes(bytes) => !bytes.split(|&b| b == b'\n').any(|oid| oid == hash.as_bytes()),
            _ => false,
        })
    }

    /// Reads the files touched by this commit, compared with its first parent.
    pub(crate) fn files_at(&self, hash: &str) -> Option<DiffFiles> {
        if !self.has_parent_history(hash) {
            return None;
        }
        let raw = run::commit_diff(&self.root, hash, run::CommitDiff::Files).ok()?;
        Some(parse_files(&raw))
    }

    /// Finds the change for one path without asking Git for any line counts.
    pub(crate) fn changed_file_at(&self, hash: &str, path: &str) -> Option<Option<PendingFile>> {
        if !self.has_parent_history(hash) {
            return None;
        }
        let raw = run::commit_diff(&self.root, hash, run::CommitDiff::Paths).ok()?;
        parse::changed_file(&raw, path)
    }

    /// Reads a whole-file patch, using both paths when a file moved.
    pub fn file_diff_at(&self, hash: &str, file: &PendingFile) -> Result<Vec<u8>, OutputError> {
        if !self.has_parent_history(hash) {
            return Err(OutputError::Failed);
        }
        run::commit_diff(&self.root, hash, run::CommitDiff::File(file))
    }

    /// An unchanged file (including a move with no edits) has no patch body.
    pub fn blob_at(&self, hash: &str, path: &str) -> Result<Vec<u8>, OutputError> {
        let hash = CommitId::parse(hash).ok_or(OutputError::Failed)?.0;
        run::limited_output(
            &self.root,
            &[],
            &["cat-file", "blob", "--end-of-options", &format!("{hash}:{path}")],
            DIFF_LIMIT,
        )
    }

    /// The newest `limit` commits, newest first, or `None` on failure.
    pub fn history(&self, limit: usize) -> Option<Vec<Commit>> {
        if !self.ok() {
            return None;
        }
        if !self.ends().has_commit() {
            return Some(Vec::new());
        }
        let limit = limit.to_string();
        let tip = &self.ends().tip;
        let unpushed = match self.history_target() {
            Some(target) => self.git(&["rev-list", "-n", &limit, "--end-of-options", &format!("{target}..{tip}")])?,
            None => Vec::new(),
        };
        let raw = self.git(&[
            "log",
            "-n",
            &limit,
            "--no-show-signature",
            "--no-notes",
            "--no-decorate",
            "--no-patch",
            "-z",
            "--encoding=UTF-8",
            "--format=%H%x00%ct%x00%P%x00%s",
            "--end-of-options",
            tip,
            "--",
        ])?;
        self.require(parse_history(&raw, &unpushed))
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
    fn a_checkout_is_named_for_its_repository_and_its_linked_worktree() {
        let f = Fixture::new();
        f.write("sub/page.txt", "a file\n");
        f.commit("first");
        let named = |dir: &Path| checkout(dir).unwrap().map(|found| (found.project, found.worktree, found.root));
        assert_eq!(named(&f.root.join("sub")), Some(("project".into(), None, f.root.clone())));

        // A linked worktree belongs to the repository that owns it, wherever its folder is.
        f.git(&["worktree", "add", "-q", "../feature"]);
        let linked = f.root.with_file_name("feature");
        assert_eq!(named(&linked.join("sub")), Some(("project".into(), Some("feature".into()), linked.clone())));

        // A bare repository has no folder around it, so its own name is used.
        f.git(&["clone", "-q", "--bare", ".", "../tool.git"]);
        f.git(&["--git-dir", "../tool.git", "worktree", "add", "-q", "../hotfix"]);
        let hotfix = f.root.with_file_name("hotfix");
        assert_eq!(named(&hotfix), Some(("tool".into(), Some("hotfix".into()), hotfix.clone())));

        let plain = Fixture::folder();
        assert!(checkout(&plain.root).unwrap().is_none());
    }

    #[test]
    fn a_worktree_path_named_head_does_not_hide_unpushed_files() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.push();
        f.write("page.txt", "second\n");
        f.commit("second");
        f.write("HEAD", "not a ref\n");

        let mut repo = read_repo(&f);
        let pending = repo.pending().unwrap();
        let (local, unpushed) = pending.split_at(1);
        assert_eq!((local[0].path.as_str(), local[0].kind), ("HEAD", Kind::Untracked));
        assert_only_unpushed_page(unpushed);
    }

    #[test]
    fn a_worktree_path_named_head_does_not_hide_rename_sources() {
        let f = Fixture::new();
        f.write("old.txt", "page\n");
        f.commit("first");
        f.git(&["mv", "old.txt", "new.txt"]);
        f.write("HEAD", "not a ref\n");

        let repo = read_repo(&f);
        assert_eq!(repo.moved_from("new.txt").as_deref(), Some("old.txt"));
        assert!(repo.ok());
    }

    #[test]
    fn history_uses_the_same_tip_as_the_status_it_displays() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.push();
        let mut repo = read_repo(&f);
        f.write("page.txt", "second\n");
        f.commit("second");

        let commits = repo.history(1).unwrap();
        assert_eq!(commits[0].subject, "first");
        assert!(!commits[0].unpushed);
        repo.read();
        let commits = repo.history(1).unwrap();
        assert_eq!(commits[0].subject, "second");
        assert!(commits[0].unpushed);
    }

    #[test]
    fn moving_an_upstream_with_the_same_ahead_count_refreshes_pending_and_history() {
        let f = Fixture::new();
        f.write("base.txt", "base\n");
        f.commit("base");
        f.push();
        f.git(&["switch", "-q", "-c", "left"]);
        f.write("left.txt", "left\n");
        f.commit("left");
        f.git(&["switch", "-q", "-c", "right", "main"]);
        f.write("right.txt", "right\n");
        f.commit("right");
        f.git(&["switch", "-q", "main"]);
        f.git(&["merge", "-q", "--no-ff", "-m", "merge left", "left"]);
        f.git(&["merge", "-q", "--no-ff", "-m", "merge right", "right"]);
        f.git(&["update-ref", "refs/remotes/origin/main", "left"]);

        let mut repo = read_repo(&f);
        let version = repo.history_version();
        let pending = repo.pending().unwrap();
        assert_eq!(pending.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["right.txt"]);
        let is_unpushed = |repo: &Repo, subject: &str| {
            repo.history(12).unwrap().into_iter().find(|c| c.subject == subject).unwrap().unpushed
        };
        assert!(!is_unpushed(&repo, "left"));
        f.git(&["update-ref", "refs/remotes/origin/main", "right"]);
        assert!(!is_unpushed(&repo, "left"), "history should use the sampled upstream until the next read");

        let before = repo.raw.clone();
        assert!(repo.poll(&pending), "the upstream moved but the ahead/behind counts did not");
        repo.read();
        assert_eq!(repo.raw, before);
        assert!(!version.matches(&repo));
        assert!(is_unpushed(&repo, "left"));
        assert!(!is_unpushed(&repo, "right"));
        let pending = repo.pending().unwrap();
        assert_eq!(pending.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["left.txt"]);
    }

    #[test]
    fn staged_changes_in_an_unborn_sha256_repository_use_its_empty_tree() {
        let f = Fixture::folder();
        f.git(&["init", "-q", "--template=", "-b", "main", "--object-format=sha256"]);
        f.write("page.txt", "staged\n");
        f.git(&["add", "page.txt"]);
        f.write("page.txt", "staged\nunstaged\n");

        let mut repo = read_repo(&f);
        let files = repo.pending().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].kind, files[0].add, files[0].del), (Kind::Added, 2, 0));
        assert!(repo.file_diff("page.txt", None).unwrap().ends_with(b"+staged\n+unstaged\n"));
        assert!(repo.history(1).unwrap().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn git_lists_valid_siblings_without_mistaking_non_utf8_names_for_them() {
        let f = Fixture::new();
        let invalid = f.root.join(OsStr::from_bytes(b"page\xff.txt"));
        f.write("page\u{fffd}.txt", "valid\n");
        fs::write(&invalid, "raw path\n").unwrap();
        f.commit("both paths");
        let mut repo = read_repo(&f);
        assert_eq!(repo.tracked().unwrap().collect::<Vec<_>>(), ["page\u{fffd}.txt"]);
        assert!(repo.paths_incomplete());

        let diff = repo.files_at(&repo.ends().tip).unwrap();
        assert_eq!(diff.unrepresented, 1);
        assert_eq!(diff.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["page\u{fffd}.txt"]);

        fs::write(invalid, "edited raw path\n").unwrap();
        repo.read();
        assert_eq!(repo.uncommitted_count(), 1);
        assert!(repo.pending().unwrap().is_empty());
        assert!(repo.paths_incomplete());
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
    fn commits_not_in_the_remote_default_branch_are_unpushed_even_without_a_common_commit() {
        let f = Fixture::new();
        f.write("remote.txt", "remote\n");
        f.commit("on the remote");
        f.push();
        f.git(&["remote", "set-head", "origin", "main"]);
        f.git(&["switch", "-q", "--orphan", "topic"]);
        f.write("local.txt", "local\n");
        f.commit("start a different history");

        let mut repo = Repo::open(&f.root).unwrap().for_history();
        repo.read();
        let commits = repo.history(1).unwrap();
        assert_eq!(commits[0].subject, "start a different history");
        assert!(commits[0].unpushed);
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
    fn a_working_file_diff_excludes_former_directory_children() {
        use crate::diff::{self, LineKind};

        let f = Fixture::new();
        let line = "x".repeat(1023) + "\n";
        f.write("lib/child", &line.repeat((DIFF_LIMIT / 1024) as usize));
        f.commit("add directory");
        fs::remove_dir_all(f.root.join("lib")).unwrap();
        std::os::unix::fs::symlink("page.txt", f.root.join("lib")).unwrap();
        f.git(&["add", "-A"]);

        let repo = read_repo(&f);
        let patch = diff::parse(&repo.file_diff("lib", None).unwrap());
        assert_eq!(patch.kind, Kind::Added);
        assert_eq!(patch.lines.len(), 1);
        assert_eq!((patch.lines[0].kind, patch.lines[0].text.as_str()), (LineKind::Add, "page.txt"));
    }

    #[test]
    fn a_working_file_diff_detects_a_rename_from_a_former_child() {
        let f = Fixture::new();
        let from = ":[page]*/old?.txt";
        let path = ":[page]*";
        f.write(from, "same content\n");
        f.write(":[page]*/other.txt", "unrelated content\n");
        f.commit("add directory");
        fs::remove_dir_all(f.root.join(path)).unwrap();
        f.write(path, "same content\n");
        f.git(&["add", "-A"]);

        let repo = read_repo(&f);
        assert_eq!(repo.moved_from(path).as_deref(), Some(from));
        let patch = crate::diff::parse(&repo.file_diff(path, Some(from)).unwrap());
        assert_eq!(patch.kind, Kind::Renamed);
        assert!(patch.lines.is_empty());
    }

    #[test]
    fn a_working_file_diff_keeps_distant_context() {
        let f = Fixture::new();
        const CONTEXT_LINES: usize = 1_000_002;
        let mut file = String::with_capacity(2 * CONTEXT_LINES + "before\ntail\n".len());
        file.push_str("before\n");
        for _ in 0..CONTEXT_LINES {
            file.push_str("0\n");
        }
        file.push_str("tail\n");
        f.write("page.txt", &file);
        f.commit("add long file");
        file.replace_range(.."before".len(), "after");
        f.write("page.txt", &file);

        let repo = read_repo(&f);
        let patch = repo.file_diff("page.txt", None).unwrap();
        assert!(patch.ends_with(b" tail\n"), "distant unchanged lines were omitted");
    }

    #[test]
    fn filters_with_equals_in_their_name_are_disabled_even_if_added_later() {
        let f = Fixture::new();
        f.write(".gitattributes", "*.txt filter=plain=text\n");
        f.write("page.txt", "first\n");
        f.commit("first");
        let mut repo = read_repo(&f);

        f.git(&["config", "filter.plain=text.required", "true"]);
        f.write("page.txt", "different line\n");
        assert!(repo.poll(&[]));
        repo.read();
        let pending = repo.pending().unwrap();
        assert_eq!((pending[0].path.as_str(), pending[0].add, pending[0].del), ("page.txt", 1, 1));
        assert!(repo.ok());
    }

    #[test]
    fn the_users_git_config_keeps_trust_and_ignores() {
        const PROJECT: &str = "MILO_TEST_GLOBAL_IGNORES_PROJECT";
        if let Some(path) = std::env::var_os(PROJECT) {
            let mut repo = Repo::open(Path::new(&path)).unwrap();
            assert!(repo.git);
            repo.read();
            let files = repo.pending().unwrap();
            assert_eq!(files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), ["visible.txt"]);
            return;
        }

        let f = Fixture::new();
        f.write(
            ".git/test-home/.gitconfig",
            &format!("[core]\n\texcludesFile = ~/.global-ignore\n[safe]\n\tdirectory = {}\n", f.root.display()),
        );
        f.write(".git/test-home/.global-ignore", "*.scratch\n");
        f.write("ignored.scratch", "ignore this\n");
        f.write("visible.txt", "show this\n");
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "git::tests::the_users_git_config_keeps_trust_and_ignores"])
            .env(PROJECT, &f.root)
            .env("HOME", f.root.join(".git/test-home"))
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("GIT_CONFIG_GLOBAL")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
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
    fn a_commit_file_lookup_does_not_need_other_files_line_counts() {
        let f = Fixture::new();
        f.write("unrelated.txt", "an unrelated blob\n");
        f.write("page.txt", "stored content\n");
        f.commit("add two files");
        let repo = read_repo(&f);
        let hash = &repo.ends().tip;
        let oid = repo.git(&["rev-parse", "HEAD:unrelated.txt"]).unwrap();
        let oid = std::str::from_utf8(&oid).unwrap().trim();
        // Prevent Git from reading the unrelated blob from disk or the work tree.
        fs::remove_file(f.root.join(".git/objects").join(&oid[..2]).join(&oid[2..])).unwrap();
        fs::remove_file(f.root.join("unrelated.txt")).unwrap();

        assert!(repo.files_at(hash).is_none(), "the commit's line counts should be unavailable");
        let file = repo.changed_file_at(hash, "page.txt").unwrap().unwrap();
        assert_eq!(file.kind, Kind::Added);
        assert!(repo.file_diff_at(hash, &file).ok().unwrap().ends_with(b"+stored content\n"));
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
