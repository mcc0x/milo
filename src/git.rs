//! Discovers a repository and keeps its last known status, files and history.
//!
//! Pending work has not been pushed. It is uncommitted if it has not been
//! committed, and unpushed if it has been committed but not pushed.

mod parse;
mod pending;
pub(crate) mod run;
pub(crate) mod worktrees;

use crate::sys::BorrowedFd;
use std::cell::{Cell, OnceCell};
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::read::{self, Content};
use crate::watch::{Events, GitFilter, Ignored, Scope};
use parse::{Status, parse_files, parse_history, parse_status};
use pending::{DiffStamp, TrackedDiff, tracked_records};
use run::OutputError;

/// A diff of one file larger than this is not loaded.
pub(crate) const DIFF_LIMIT: u64 = 8 << 20;
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
#[derive(Clone, Default)]
pub(crate) struct DiffFiles {
    pub(crate) files: Vec<PendingFile>,
    pub(crate) unrepresented: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitFilesError {
    ShallowBoundary,
    Git,
}

/// Reads the files commits touched. It owns its paths, so that a read can
/// run off the pane's loop while the repository stays with the pane.
pub(crate) struct CommitFiles {
    root: PathBuf,
    shallow_file: Option<PathBuf>,
}

impl CommitFiles {
    /// Reads the files touched by this commit, compared with its first parent.
    pub(crate) fn at(&self, hash: &str, cancel: BorrowedFd<'_>) -> Result<DiffFiles, CommitFilesError> {
        parent_history(&self.root, self.shallow_file.as_deref(), hash, Some(cancel))?;
        let raw =
            run::commit_diff(&self.root, hash, run::CommitDiff::Files(cancel)).map_err(|_| CommitFilesError::Git)?;
        Ok(parse_files(&raw))
    }
}

/// Git treats a shallow boundary as a root, even when the commit has a parent.
fn parent_history(
    root: &Path,
    shallow_file: Option<&Path>,
    hash: &str,
    cancel: Option<BorrowedFd<'_>>,
) -> Result<(), CommitFilesError> {
    let Some(file) = shallow_file else { return Ok(()) };
    match read::file(file, DIFF_LIMIT) {
        Content::Gone => return Ok(()),
        Content::Bytes(bytes) => {
            if !bytes.split(|&b| b == b'\n').any(|oid| oid == hash.as_bytes()) {
                return Ok(());
            }
        }
        _ => return Err(CommitFilesError::Git),
    }

    // A shallow marker alone is not enough: a depth-limited clone can
    // mark a genuine root. Read the actual header, not Git's ancestry view.
    let hash = CommitId::parse(hash).ok_or(CommitFilesError::Git)?.0;
    let raw =
        run::limited_output_until(root, &[], &["cat-file", "commit", "--end-of-options", hash], DIFF_LIMIT, cancel)
            .map_err(|_| CommitFilesError::Git)?;
    let mut headers = raw.split(|&b| b == b'\n');
    if !headers.next().is_some_and(|line| line.starts_with(b"tree ")) {
        return Err(CommitFilesError::Git);
    }
    if let Some(parent) = headers.next().and_then(|line| line.strip_prefix(b"parent ")) {
        return Err(if parent.len() == hash.len() && parent.iter().all(u8::is_ascii_hexdigit) {
            CommitFilesError::ShallowBoundary
        } else {
            CommitFilesError::Git
        });
    }
    Ok(())
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
#[derive(Clone, Default)]
struct Ends {
    base: Option<String>,
    tip: String,
}

/// The default branch to try in place of an upstream, even before it exists.
#[derive(Clone, PartialEq, Eq)]
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

#[derive(Clone, Default, PartialEq, Eq)]
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
#[derive(Clone)]
struct Snapshot {
    status: Option<Arc<Status>>,
    comparison: Comparison,
    shallow: Option<DiffStamp>,
}

#[derive(Clone, Copy, Default)]
enum ReadFor {
    #[default]
    Pending,
    History,
}

/// The state a loaded history list was drawn from.
#[derive(Clone)]
pub struct HistoryVersion {
    pub branch: String,
    tip: String,
    target: Option<String>,
    shallow: Option<DiffStamp>,
    pub ahead: Option<u32>,
}

impl HistoryVersion {
    pub fn matches(&self, repo: &Repo) -> bool {
        self.tip == repo.status.tip
            && self.branch == repo.branch()
            && self.target.as_deref() == repo.history_target()
            && self.shallow == repo.shallow
            && self.ahead == repo.upstream_ahead()
    }
}

#[derive(Clone, Default)]
struct Unpushed {
    ends: Ends,
    diff: DiffFiles,
}

#[derive(Clone, Default)]
struct Tracked {
    index: Option<DiffStamp>,
    /// Shared, so that a copy of the repository does not copy every path.
    paths: Arc<str>,
    incomplete: bool,
}

#[derive(Clone, Default)]
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
    /// The settings `open_to_read` read, for the first status to use.
    opened_config: Option<run::Config>,
    read_for: ReadFor,
    /// When set, status is limited to this path.
    file: Option<String>,
    comparison: Comparison,
    /// Past status stays readable while a worker takes a new sample.
    status: Arc<Status>,
    /// Only pending file diffs use a merge base, which is not part of status.
    base: Option<String>,
    /// Shared with the watcher until Git reports different ignored paths.
    watch_ignored: Arc<Ignored>,
    /// A status `poll` found to differ, kept for `read` to take in.
    polled: Option<Snapshot>,
    unpushed: Unpushed,
    pending_tracked: Option<TrackedDiff>,
    tracked: Tracked,
}

/// What a refresh brings back to the pane that asked for it.
pub(crate) struct Refreshed {
    /// The copy that was refreshed, to take the place of the one copied.
    pub(crate) repo: Repo,
    /// The pending files to load, unless nothing moved or Git could not answer.
    pub(crate) pending: Option<Vec<PendingFile>>,
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
    Ok(raw.and_then(|raw| crate::sys::os_string(raw).map(PathBuf::from)).filter(|root| root.is_absolute()))
}

/// The ID of the commit a checkout's history starts from. Clones of one
/// repository share it whatever folder they are in, so it tells whether two
/// checkouts on different computers are the same project. A repository with
/// no commit, or a shallow clone cut short of its start, has none to compare.
pub(crate) fn root_commit(root: &Path) -> Option<String> {
    if run::output(root, &[], &["rev-parse", "--is-shallow-repository"])?.as_slice() != b"false\n" {
        return None;
    }
    first_visible_commit(root)
}

fn first_visible_commit(root: &Path) -> Option<String> {
    // In a shallow checkout Git stops at the local boundary, not the root on another host.
    run::optional_oid(root, &[], &["rev-list", "--max-parents=0", "--max-count=1", "HEAD"])?
}

/// Identity for reviewing an explicit mapping on this host, never for matching a repo across hosts.
/// The Git directory distinguishes two unborn repositories installed in the same worktree directory.
pub(crate) fn mapped_checkout(root: &Path) -> Option<(u64, u64, Option<String>)> {
    let raw = discovery_output(root, &["rev-parse", "--absolute-git-dir"]).ok()??;
    let git = Path::new(crate::sys::os_str(&raw)?);
    if !git.is_absolute() {
        return None;
    }
    let directory = folder_identity(git)?;
    let first = first_visible_commit(root);
    if first.is_none() && run::optional_oid(root, &[], &["rev-parse", "--verify", "--quiet", "HEAD"])?.is_some() {
        return None; // HEAD exists but its history could not be read.
    }
    Some((directory.0, directory.1, first))
}

/// Which folder `path` leads to, whatever way it is spelled.
#[cfg(unix)]
fn folder_identity(path: &Path) -> Option<(u64, u64)> {
    let found = path.metadata().ok()?;
    Some((found.dev(), found.ino()))
}

#[cfg(windows)]
fn folder_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::fs::OpenOptionsExt as _;
    // Opening a folder takes backup semantics; the handle then names the folder itself.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let folder = std::fs::OpenOptions::new().access_mode(0).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path).ok()?;
    if !folder.metadata().ok()?.is_dir() {
        return None;
    }
    crate::sys::identity(&folder).ok()
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
    // A linked worktree keeps its index apart from the repository's branches, in a
    // folder of its own among the repository's `worktrees`. A checkout's files can
    // point at any repository, so only that folder says whose worktree this is.
    let repository = match (&dirs.git, &dirs.common) {
        (Some(git), Some(common)) if linked(git, common) => repository_name(common, &dirs.root),
        _ => None,
    };
    Ok(Some(match repository {
        Some(project) if project != folder => Checkout { root: dirs.root, project, worktree: Some(folder) },
        _ => Checkout { root: dirs.root, project: folder, worktree: None },
    }))
}

/// Whether `git` is where the repository at `common` keeps a linked worktree.
fn linked(git: &Path, common: &Path) -> bool {
    let worktrees = git.parent();
    worktrees.is_some_and(|dir| dir.file_name() == Some(OsStr::new("worktrees")) && dir.parent() == Some(common))
}

/// The folder holding `.git`, or a bare repository's own folder without its `.git` ending.
/// A hidden repository is named for the folder around it only from a worktree beside it:
/// one that holds its own worktree is its own folder, and what is around it is where it is kept.
fn repository_name(common: &Path, root: &Path) -> Option<String> {
    let own = common.file_name()?.to_string_lossy();
    if own.starts_with('.') {
        if root.ancestors().any(|folder| same_folder(folder, common)) {
            return None;
        }
        return Some(common.parent()?.file_name()?.to_string_lossy().into_owned());
    }
    Some(own.strip_suffix(".git").filter(|name| !name.is_empty()).unwrap_or(&own).to_owned())
}

/// Whether two paths lead to one folder. A folder can be spelled in more than
/// one way, in another case for instance, so the spellings are not compared.
pub(crate) fn same_folder(one: &Path, other: &Path) -> bool {
    folder_identity(one).is_some_and(|found| folder_identity(other) == Some(found))
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
    let path = |bytes: &[u8]| crate::sys::os_str(bytes).map(PathBuf::from);
    let raw =
        ask(&["rev-parse", "--show-toplevel", "--absolute-git-dir", "--path-format=absolute", "--git-common-dir"])?;
    let Some(raw) = raw else {
        // Older Git can still give the project root.
        return Ok(discover_worktree(dir)?.map(|root| GitDirs { root, git: None, common: None }));
    };
    let mut paths = raw.split(|&b| b == b'\n');
    if let (Some(root), Some(git), Some(common), None) = (paths.next(), paths.next(), paths.next(), paths.next()) {
        // A root that cannot be represented exactly is not a checkout this build can open.
        let Some(root) = path(root) else { return Ok(None) };
        return Ok(Some(GitDirs { root, git: path(git), common: path(common) }));
    }
    // A newline can be part of a path. Separate answers have no ambiguous delimiters.
    // Git before 2.31 prints `--path-format` back instead of following it, so only a
    // whole path is an answer. That Git does not say where its records are.
    let one = |args: &[&str]| ask(args).map(|raw| raw.as_deref().and_then(path).filter(|folder| folder.is_absolute()));
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
        Self::found(dir, git_dirs(dir))
    }

    /// Opens as `open` does, for a caller that reads the status next. The Git
    /// settings that read starts with are read while the repository is found,
    /// so opening takes one command's time, not two.
    pub fn open_to_read(dir: &Path) -> Option<Self> {
        let (config, discovered) = run::config_alongside(dir, || git_dirs(dir));
        let mut repo = Self::found(dir, discovered)?;
        repo.opened_config = config.filter(|_| repo.git);
        Some(repo)
    }

    fn found(dir: &Path, discovered: Result<Option<GitDirs>, &'static str>) -> Option<Self> {
        let git_unavailable = discovered.is_err();
        let (root, git, git_dir, common_dir) = match discovered.ok().flatten() {
            Some(found) => (found.root, true, found.git, found.common),
            None => (crate::sys::canonicalize(dir).ok().filter(|d| d.is_dir())?, false, None, None),
        };
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let index_lock = git_dir.as_ref().map(|dir| dir.join("index.lock"));
        let shallow_file = common_dir.as_ref().map(|dir| dir.join("shallow"));
        let filter =
            git_dir.as_deref().zip(common_dir.as_deref()).and_then(|(git, common)| GitFilter::new(git, common));
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
        match self.base.as_deref() {
            Some(base) => Some(base),
            None if self.status.has_commit() => Some(&self.status.tip),
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
        self.config = match self.opened_config.take() {
            Some(config) => config,
            None => self.require(run::config(&self.root))?,
        };
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
        let status = (raw != self.status.raw).then(|| Arc::new(parse_status(raw)));
        let comparison = self.comparison(status.as_ref().unwrap_or(&self.status))?;
        Some(Snapshot { status, comparison, shallow })
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

    /// A *refresh* is a poll and, when the repository moved, everything a load
    /// reads from Git, done on a copy of the repository so that it can run away
    /// from the pane loop. `files` are the pending files on screen.
    pub(crate) fn refreshed(mut self, files: &[PendingFile]) -> Refreshed {
        let pending = if self.poll(files) {
            self.read();
            self.pending()
        } else {
            None
        };
        Refreshed { repo: self, pending }
    }

    /// Takes in the repository's status and comparison base, keeping both on failure.
    pub fn read(&mut self) {
        if !self.git {
            return;
        }
        let Some(Snapshot { status, comparison, shallow }) = self.polled.take().or_else(|| self.snapshot()) else {
            return;
        };
        let current = status.as_ref().unwrap_or(&self.status);
        let tip_changed = current.tip != self.status.tip;
        let graph_changed = shallow != self.shallow;
        let basis_changed = tip_changed || graph_changed || comparison.oid() != self.comparison.oid();
        let base_changed = match self.read_for {
            ReadFor::History => self.base.take().is_some(),
            ReadFor::Pending if basis_changed => {
                let Some(base) = self.merge_base(&current.tip, comparison.oid()) else { return };
                let changed = base != self.base;
                self.base = base;
                changed
            }
            ReadFor::Pending => false,
        };
        if self.pending_tracked.is_some()
            && (!self.ok()
                || base_changed
                || tip_changed
                || status
                    .as_ref()
                    .is_some_and(|status| !tracked_records(&status.raw).eq(tracked_records(&self.status.raw))))
        {
            self.invalidate_tracked_diff();
        }
        if let Some(status) = status {
            if status.ignored_dirs != self.status.ignored_dirs || status.ignored_files != self.status.ignored_files {
                self.watch_ignored = self.exclusions(&status);
            }
            self.status = status;
        }
        (self.comparison, self.shallow) = (comparison, shallow);
        self.ok.set(true);
    }

    fn comparison(&self, status: &Status) -> Option<Comparison> {
        if !status.has_commit() {
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
    /// Lists the tracked paths for `listed` as well. Returns `None` if git
    /// cannot answer.
    pub fn pending(&mut self) -> Option<Vec<PendingFile>> {
        if !self.ok() {
            return None;
        }
        let ends = (self.base != self.unpushed.ends.base || self.status.tip != self.unpushed.ends.tip)
            .then(|| Ends { base: self.base.clone(), tip: self.status.tip.clone() });
        let unpushed = ends
            .as_ref()
            .and_then(|ends| Some(["diff", ends.base.as_deref()?, &ends.tip, "-M", "--raw", "--numstat", "-z", "--"]));
        // Stamp the index before Git lists it, so a later change is listed again.
        let index = self.diff_index();
        let list = index.is_none() || index != self.tracked.index;
        // The three reads need nothing from each other, so they run together.
        let ahead = [unpushed.as_ref().map(|args| &args[..]), list.then_some(&["ls-files", "-z", "--cached"][..])];
        let (root, guard) = (self.root.clone(), self.config.guard.clone());
        let (files, [diff, listed]) = run::alongside(&root, &guard, ahead, || self.tracked_uncommitted());
        let mut files = files?;
        files.extend(self.status.untracked.iter().map(|path| PendingFile::new(path, Kind::Untracked)));
        if let Some(ends) = ends {
            let diff = if ends.base.is_some() { parse_files(&self.require(diff)?) } else { DiffFiles::default() };
            self.unpushed = Unpushed { ends, diff };
        }
        files.extend(self.unpushed.diff.files.iter().cloned().map(|mut file| {
            file.unpushed = true;
            file.stamp = FileStamp::at(&self.root.join(&file.path));
            file
        }));
        if list {
            self.keep_tracked(index, self.require(listed)?);
        }
        Some(files)
    }

    /// Some pathname bytes cannot be used by the panes' UTF-8 path model.
    pub(crate) fn paths_incomplete(&self) -> bool {
        self.status.unrepresented != 0 || self.tracked.incomplete || self.unpushed.diff.unrepresented != 0
    }

    /// Keeps the tracked paths Git listed while the index was at `index`.
    fn keep_tracked(&mut self, index: Option<DiffStamp>, raw: Vec<u8>) {
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
        self.tracked.index = index;
        self.tracked.incomplete = incomplete;
        if paths != *self.tracked.paths {
            self.tracked.paths = paths.into();
        }
    }

    /// The tracked paths relative to the root, as `pending` last listed them.
    /// A list that Git could not give leaves the one before it.
    pub(crate) fn listed(&self) -> impl Iterator<Item = &str> {
        self.tracked.paths.split_terminator('\0')
    }

    /// The identity of the tracked path listing, shared by repository snapshots.
    pub(crate) fn listed_paths(&self) -> &Arc<str> {
        &self.tracked.paths
    }

    pub fn is_untracked(&self, path: &str) -> bool {
        self.status.untracked.iter().any(|p| p == path)
    }

    pub fn is_unmerged(&self, path: &str) -> bool {
        self.status.unmerged.iter().any(|p| p == path)
    }

    /// One binary preview's status, never a patch, text conversion or rename scan.
    /// Refused source files get index-only status so Git will not materialize or
    /// hash a worktree file the preview was unable to read within its limits.
    pub(crate) fn media_kind(&self, path: &str, read_worktree: bool) -> Option<Kind> {
        const LIMIT: u64 = 1 << 16;
        if read_worktree {
            let config = run::config(&self.root)?;
            let raw = run::limited_output(
                &self.root,
                &config.guard,
                &["status", "--porcelain=v2", "-z", "--no-renames", "--untracked-files=all", "--", path],
                LIMIT,
            )
            .ok()?;
            let mut kind = Kind::None;
            for record in raw.split(|&b| b == 0) {
                kind = match record {
                    [b'?', b' ', ..] => return Some(Kind::Untracked),
                    [b'u', b' ', ..] => return Some(Kind::Conflict),
                    // Worktree-only additions (from `git add -N`) use `.A`.
                    [b'1', b' ', b'A', _, b' ', ..] | [b'1', b' ', _, b'A', b' ', ..] => Kind::Added,
                    [b'1', b' ', b'D', _, b' ', ..] | [b'1', b' ', _, b'D', b' ', ..] => Kind::Deleted,
                    [b'1', b' ', _, _, b' ', ..] => Kind::Modified,
                    [] => continue,
                    _ => return None,
                };
            }
            return Some(kind);
        }
        let get = |args: &[&str]| run::limited_output(&self.root, &[], args, LIMIT).ok();
        let indexed = get(&["ls-files", "--stage", "-z", "--", path])?;
        if indexed.is_empty() {
            let untracked = get(&["ls-files", "--others", "--exclude-standard", "-z", "--", path])?;
            return Some(if untracked.is_empty() { Kind::None } else { Kind::Untracked });
        }
        let index = indexed.split(|&b| b == b'\t').next()?;
        let index = index.strip_suffix(b" 0");
        let Some(index) = index else { return Some(Kind::Conflict) };
        let Some(head) = self.resolve("HEAD")? else { return Some(Kind::Added) };
        let tree = get(&["ls-tree", "-z", &head, "--", path])?;
        if tree.is_empty() {
            return Some(Kind::Added);
        }
        let mut fields = tree.split(|&b| b == b' ' || b == b'\t');
        let mut indexed = index.split(|&b| b == b' ');
        let same_mode = fields.next()? == indexed.next()?;
        let _object_type = fields.next()?;
        let same_object = fields.next()? == indexed.next()?;
        Some(if same_mode && same_object { Kind::None } else { Kind::Modified })
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
            tip: self.status.tip.clone(),
            target: self.history_target().map(str::to_owned),
            shallow: self.shallow,
            ahead: self.upstream_ahead(),
        }
    }

    fn history_target(&self) -> Option<&str> {
        self.comparison.oid().filter(|&oid| oid != self.status.tip)
    }

    /// Finds the full object name and date without reading the work tree.
    pub fn resolve_commit(&self, id: CommitId<'_>) -> Option<CommitVersion> {
        let raw = self.git(&[
            "log", "-1", "--no-walk", "--no-show-signature", "--no-notes", "--no-decorate", "--no-patch",
            "--format=%H%x00%ct", "--end-of-options", id.0, "--",
        ])?;
        parse::commit_version(&raw, id)
    }

    fn check_parent_history(&self, hash: &str) -> Result<(), CommitFilesError> {
        parent_history(&self.root, self.shallow_file.as_deref(), hash, None)
    }

    pub(crate) fn commit_files(&self) -> CommitFiles {
        CommitFiles { root: self.root.clone(), shallow_file: self.shallow_file.clone() }
    }

    /// Finds the change for one path without asking Git for any line counts.
    pub(crate) fn changed_file_at(&self, hash: &str, path: &str) -> Option<Option<PendingFile>> {
        self.check_parent_history(hash).ok()?;
        let raw = run::commit_diff(&self.root, hash, run::CommitDiff::Paths).ok()?;
        parse::changed_file(&raw, path)
    }

    /// Reads a whole-file patch, using both paths when a file moved.
    pub fn file_diff_at(&self, hash: &str, file: &PendingFile) -> Result<Vec<u8>, OutputError> {
        self.check_parent_history(hash).map_err(|_| OutputError::Failed)?;
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
        if !self.status.has_commit() {
            return Some(Vec::new());
        }
        let limit = limit.to_string();
        let tip = &self.status.tip;
        let range = self.history_target().map(|target| format!("{target}..{tip}"));
        let unpushed = range.as_ref().map(|range| ["rev-list", "-n", &limit, "--end-of-options", range]);
        let log = [
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
        ];
        // Which commits are unpushed and what each commit says are separate reads.
        let (raw, [listed]) =
            run::alongside(&self.root, self.guard(), [unpushed.as_ref().map(|args| &args[..])], || self.git(&log));
        let unpushed = if unpushed.is_some() { self.require(listed)? } else { Vec::new() };
        self.require(parse_history(&raw?, &unpushed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use crate::sys::AsFd as _;
    use std::fs;
    #[cfg(unix)]
    use std::fs::Permissions;
    #[cfg(unix)]
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

        // So is one that keeps its worktrees inside itself. One kept out of sight
        // beside its worktrees is named for the folder that holds them all.
        f.git(&["--git-dir", "../tool.git", "worktree", "add", "-q", "../tool.git/inside"]);
        let inside = f.root.with_file_name("tool.git").join("inside");
        assert_eq!(named(&inside), Some(("tool".into(), Some("inside".into()), inside.clone())));
        f.git(&["clone", "-q", "--bare", ".", "../site/.bare"]);
        f.git(&["--git-dir", "../site/.bare", "worktree", "add", "-q", "../site/main"]);
        let main = f.root.with_file_name("site").join("main");
        assert_eq!(named(&main), Some(("site".into(), Some("main".into()), main.clone())));

        let plain = Fixture::folder();
        assert!(checkout(&plain.root).unwrap().is_none());
    }

    #[test]
    fn a_checkout_is_not_named_for_a_folder_its_own_files_point_to() {
        let named = |dir: &Path| checkout(dir).unwrap().map(|found| (found.project, found.worktree));
        let write = |path: PathBuf, text: &str| std::fs::write(path, text).unwrap();

        // Git follows a checkout's `commondir` to any repository, here a bare one two
        // folders up. Only a folder among that repository's `worktrees` says whose it is.
        let f = Fixture::new();
        f.write("page.txt", "a file\n");
        f.commit("first");
        f.git(&["init", "-q", "--bare", "--template=", "../.kept"]);
        write(f.root.join(".git/commondir"), "../../.kept\n");
        assert_eq!(named(&f.root), Some(("project".into(), None)));

        // A hidden folder that is its own repository is not named for the folder
        // around it either, from its top or from a worktree inside it.
        let hidden = f.root.with_file_name(".hidden");
        f.git(&["init", "-q", "--template=", "-b", "main", "../.hidden"]);
        for part in ["objects", "refs", "config"] {
            std::fs::rename(hidden.join(".git").join(part), hidden.join(part)).unwrap();
        }
        write(hidden.join(".git/commondir"), "..\n");
        assert_eq!(named(&hidden), Some((".hidden".into(), None)));
        std::fs::create_dir_all(hidden.join("worktrees/w")).unwrap();
        std::fs::create_dir(hidden.join("sub")).unwrap();
        write(hidden.join("worktrees/w/HEAD"), "ref: refs/heads/w\n");
        write(hidden.join("worktrees/w/commondir"), "../..\n");
        write(hidden.join("sub/.git"), "gitdir: ../worktrees/w\n");
        assert_eq!(named(&hidden.join("sub")), Some(("sub".into(), None)));

        // Where one folder answers to two spellings, a worktree cannot hide that it is inside.
        std::fs::create_dir_all(hidden.join("worktrees/v")).unwrap();
        std::fs::create_dir(hidden.join("spelled")).unwrap();
        write(hidden.join("worktrees/v/HEAD"), "ref: refs/heads/v\n");
        write(hidden.join("worktrees/v/commondir"), "../..\n");
        write(hidden.join("spelled/.git"), "gitdir: ../../.HIDDEN/worktrees/v\n");
        // Where they are two folders, there is no such repository.
        assert!(named(&hidden.join("spelled")).is_none_or(|found| found == ("spelled".into(), None)));
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
    fn a_history_refresh_takes_in_a_new_sample_without_changing_the_old_one() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        let mut shown = Repo::open(&f.root).unwrap().for_history();
        shown.read();
        let version = shown.history_version();
        let mut refresh = shown.clone();

        assert!(Arc::ptr_eq(&shown.status, &refresh.status));
        assert!(!refresh.poll(&[]));
        assert_eq!(shown.uncommitted_count(), 1);
        assert!(shown.history(1).unwrap().is_empty());

        f.commit("first");
        assert!(refresh.poll(&[]));
        assert!(version.matches(&refresh), "poll does not take in the new sample");
        refresh.read();
        assert_eq!(refresh.uncommitted_count(), 0);
        assert_eq!(refresh.history(1).unwrap()[0].subject, "first");
        assert_eq!(shown.uncommitted_count(), 1);
        assert!(shown.history(1).unwrap().is_empty());
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

        let before = Arc::clone(&repo.status);
        assert!(repo.poll(&pending), "the upstream moved but the ahead/behind counts did not");
        repo.read();
        assert!(Arc::ptr_eq(&repo.status, &before), "the upstream moved without changing the worktree status");
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
        use std::os::unix::ffi::OsStrExt as _;

        let f = Fixture::new();
        let invalid = f.root.join(OsStr::from_bytes(b"page\xff.txt"));
        f.write("page\u{fffd}.txt", "valid\n");
        fs::write(&invalid, "raw path\n").unwrap();
        f.commit("both paths");
        let mut repo = read_repo(&f);
        repo.pending().unwrap();
        assert_eq!(repo.listed().collect::<Vec<_>>(), ["page\u{fffd}.txt"]);
        assert!(repo.paths_incomplete());

        let (cancel, _keep_open) = std::io::pipe().unwrap();
        let diff = repo.commit_files().at(&repo.status.tip, cancel.as_fd()).unwrap();
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

    #[cfg(unix)]
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
        assert_eq!(repo.listed().collect::<Vec<_>>(), [".gitattributes", "page.txt"]);
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
    fn a_repository_opened_to_read_disables_filters_set_before_and_after_its_first_status() {
        let f = Fixture::new();
        f.write(".gitattributes", "*.txt filter=mark\n*.md filter=late\n");
        f.write("page.txt", "first\n");
        f.write("notes.md", "first\n");
        f.commit("first");
        f.write("page.txt", "changed\n");
        f.git(&["config", "filter.mark.clean", "touch .git/mark-ran; cat"]);
        f.git(&["config", "filter.mark.required", "true"]);

        let mut repo = Repo::open_to_read(&f.root).unwrap();
        repo.read();
        assert_eq!(repo.pending().unwrap().len(), 1);
        assert!(repo.opened_config.is_none(), "only the first status may use the settings read at opening");

        f.git(&["config", "filter.late.clean", "touch .git/late-ran; cat"]);
        f.git(&["config", "filter.late.required", "true"]);
        f.write("notes.md", "changed\n");
        assert!(repo.poll(&[]));
        repo.read();
        assert_eq!(repo.pending().unwrap().len(), 2);
        assert!(repo.ok());
        for marker in ["mark-ran", "late-ran"] {
            assert!(!f.root.join(".git").join(marker).exists(), "reading ran the filter for {marker}");
        }
    }

    #[test]
    fn media_status_marks_intent_to_add_and_worktree_deletion() {
        let f = Fixture::new();
        f.write("old.png", "already tracked\n");
        f.commit("base");
        let repo = Repo::open(&f.root).unwrap();

        f.write("new.png", "to be added\n");
        f.git(&["add", "-N", "--", "new.png"]);
        assert_eq!(repo.media_kind("new.png", true), Some(Kind::Added));
        f.git(&["add", "--", "new.png"]);
        assert_eq!(repo.media_kind("new.png", true), Some(Kind::Added));

        fs::remove_file(f.root.join("old.png")).unwrap();
        assert_eq!(repo.media_kind("old.png", true), Some(Kind::Deleted));
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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
    fn a_working_file_diff_names_exactly_one_file_whatever_its_characters() {
        let f = Fixture::new();
        let names = ["docs/日本語", "日本/語", "docs/notes.md", "docs/notes.md.bak", "a[b].txt"];
        for name in names {
            f.write(name, "before\n");
        }
        f.commit("add files");
        for name in names {
            f.write(name, &format!("after {name}\n"));
        }

        let repo = read_repo(&f);
        for name in names {
            let patch = repo.file_diff(name, None).unwrap();
            let text = String::from_utf8_lossy(&patch);
            assert!(text.contains(&format!("+after {name}\n")), "{name}: {text}");
            assert_eq!(text.matches("+after ").count(), 1, "{name} also matched another file: {text}");
        }
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
            &format!(
                "[core]\n\texcludesFile = ~/.global-ignore\n[safe]\n\tdirectory = {}\n",
                f.root.display().to_string().replace('\\', "/")
            ),
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
        repo.pending().unwrap();
        assert_eq!(repo.listed().collect::<Vec<_>>(), ["page.txt"]);
        let previous = repo.tracked.index;
        f.write(".git/index", "corrupt index");
        assert!(repo.pending().is_none());
        assert!(!repo.ok());
        assert_eq!(&*repo.tracked.paths, "page.txt\0");
        assert_eq!(repo.tracked.index, previous);
    }

    #[test]
    fn a_commit_file_lookup_does_not_need_other_files_line_counts() {
        let f = Fixture::new();
        f.write("unrelated.txt", "an unrelated blob\n");
        f.write("page.txt", "stored content\n");
        f.commit("add two files");
        let repo = read_repo(&f);
        let hash = &repo.status.tip;
        let oid = repo.git(&["rev-parse", "HEAD:unrelated.txt"]).unwrap();
        let oid = std::str::from_utf8(&oid).unwrap().trim();
        // Prevent Git from reading the unrelated blob from disk or the work tree.
        fs::remove_file(f.root.join(".git/objects").join(&oid[..2]).join(&oid[2..])).unwrap();
        fs::remove_file(f.root.join("unrelated.txt")).unwrap();

        let (cancel, _keep_open) = std::io::pipe().unwrap();
        assert!(
            matches!(repo.commit_files().at(hash, cancel.as_fd()), Err(CommitFilesError::Git)),
            "the commit's line counts should be unavailable"
        );
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
