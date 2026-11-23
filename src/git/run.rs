//! Runs read-only git commands without letting a repository run its own programs.

use std::cell::Cell;
use std::ffi::OsStr;
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::LazyLock;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{DIFF_LIMIT, Kind, PendingFile};
use crate::term;

/// Maximum output kept from a git command without a smaller per-command limit.
const OUTPUT_LIMIT: u64 = 256 << 20;
/// How long a git command may run before it is killed.
const DEADLINE: Duration = Duration::from_secs(15);

thread_local! {
    /// Only a refresh and its scoped Git helpers inherit this descriptor.
    static REFRESH_CANCEL: Cell<RawFd> = const { Cell::new(-1) };
}

/// The socket signals completion to the pane and cancellation to the worker.
pub(crate) struct RefreshTask {
    ready: UnixStream,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RefreshTask {
    pub(crate) fn spawn(work: impl FnOnce() + Send + 'static) -> io::Result<Self> {
        let (ready, worker) = UnixStream::pair()?;
        let thread = std::thread::Builder::new().spawn(move || {
            REFRESH_CANCEL.set(worker.as_raw_fd());
            work();
            REFRESH_CANCEL.set(-1);
            drop(worker);
        })?;
        Ok(Self { ready, thread: Some(thread) })
    }

    pub(crate) fn fd(&self) -> RawFd {
        self.ready.as_raw_fd()
    }
}

impl Drop for RefreshTask {
    fn drop(&mut self) {
        // Even on immediate process exit, reap Git before the worker disappears.
        let _ = self.ready.shutdown(std::net::Shutdown::Write);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Cache the executable so frequent commands can bypass the macOS git launcher.
pub(crate) static GIT: LazyLock<PathBuf> = LazyLock::new(|| {
    let mut launcher = isolated(Command::new("git"));
    launcher.arg("--exec-path");
    let real = checked_output(launcher, 4096, None)
        .ok()
        .and_then(|raw| raw.strip_suffix(b"\n").map(|path| Path::new(OsStr::from_bytes(path)).join("git")));
    real.filter(|path| path.is_absolute() && path.is_file()).unwrap_or_else(|| PathBuf::from("git"))
});

pub(crate) enum OutputError {
    TooLarge,
    Unavailable,
    Failed,
}

pub(super) fn output(root: &Path, guard: &[String], args: &[&str]) -> Option<Vec<u8>> {
    limited_output(root, guard, args, OUTPUT_LIMIT).ok()
}

/// Runs each command of `ahead` on a thread of its own while `main` runs on
/// this one, and gives what `output` would for each, in order. Commands that
/// do not need each other's answers then take as long as the slowest, not
/// as long as all of them.
pub(super) fn alongside<T, const N: usize>(
    root: &Path,
    guard: &[String],
    ahead: [Option<&[&str]>; N],
    main: impl FnOnce() -> T,
) -> (T, [Option<Vec<u8>>; N]) {
    let cancel = REFRESH_CANCEL.get();
    std::thread::scope(|scope| {
        let started = ahead.map(|args| {
            let args = args?;
            // Without a thread to spare, the command runs here after `main`.
            Some(
                std::thread::Builder::new()
                    .spawn_scoped(scope, move || {
                        REFRESH_CANCEL.set(cancel);
                        output(root, guard, args)
                    })
                    .map_err(|_| args),
            )
        });
        let answer = main();
        let answers = started.map(|running| match running? {
            Ok(thread) => thread.join().ok().flatten(),
            Err(args) => output(root, guard, args),
        });
        (answer, answers)
    })
}

pub(super) fn limited_output(root: &Path, guard: &[String], args: &[&str], limit: u64) -> Result<Vec<u8>, OutputError> {
    limited_output_until(root, guard, args, limit, None)
}

pub(super) fn limited_output_until(
    root: &Path,
    guard: &[String],
    args: &[&str],
    limit: u64,
    cancel: Option<BorrowedFd<'_>>,
) -> Result<Vec<u8>, OutputError> {
    checked_output(command(root, guard, args), limit, cancel)
}

pub(super) fn working_diff(
    root: &Path,
    guard: &[String],
    against: &str,
    path: &str,
    from: Option<&str>,
) -> Result<Vec<u8>, OutputError> {
    let mut cmd = command(root, guard, &["diff", against, "-M"]);
    cmd.args(WHOLE_FILE)
        .arg("--")
        .env_remove("GIT_LITERAL_PATHSPECS")
        .args(from.into_iter().chain(std::iter::once(path)).map(exact_pathspec));
    checked_output(cmd, DIFF_LIMIT, None)
}

fn exact_pathspec(path: &str) -> String {
    // Even a literal pathspec also matches children. Escaping the final character
    // makes Git use a whole-path glob, with no unescaped metacharacters.
    const MAGIC: &str = ":(glob)";
    let mut spec = String::with_capacity(MAGIC.len() + path.len() + 1);
    spec.push_str(MAGIC);
    for (i, ch) in path.char_indices() {
        if matches!(ch, '*' | '?' | '[' | '\\') || i + ch.len_utf8() == path.len() {
            spec.push('\\');
        }
        spec.push(ch);
    }
    spec
}

/// Count lines for the named paths, or the whole tree when there are none.
pub(super) fn diff_counts<'a>(
    root: &Path,
    guard: &[String],
    against: &str,
    renames: &str,
    mut paths: impl ExactSizeIterator<Item = &'a str>,
) -> Option<Vec<u8>> {
    let mut cmd = command(root, guard, &["diff", against, renames, "--raw", "--numstat", "-z", "--"]);
    if paths.len() == 1 {
        // Git's tree-to-index walk with one literal path early in a large index
        // can cost more than walking the entire tree. Add a redundant descendant
        // glob to change that walk without broadening the literal selection.
        let path = paths.next()?;
        if path.contains(['*', '?', '[', '\\']) {
            // Git also matches glob patterns as raw strings: escaping these
            // characters could include a different directory named with backslashes.
            cmd.arg(path);
        } else {
            cmd.env_remove("GIT_LITERAL_PATHSPECS").arg(format!(":(literal){path}")).arg(format!(":(glob){path}/**"));
        }
    } else {
        cmd.args(paths);
    }
    checked_output(cmd, OUTPUT_LIMIT, None).ok()
}

pub(super) enum CommitDiff<'a> {
    Files(BorrowedFd<'a>),
    Paths,
    File(&'a PendingFile),
}

// Full context within the output limit, with the prefixes the patch parser expects.
const WHOLE_FILE: [&str; 4] =
    ["--unified=8388608", "--output-indicator-new=+", "--output-indicator-old=-", "--output-indicator-context= "];

/// Reads one commit against its first parent, or the empty tree for a root commit.
pub(super) fn commit_diff(root: &Path, hash: &str, diff: CommitDiff<'_>) -> Result<Vec<u8>, OutputError> {
    let hash = super::CommitId::parse(hash).ok_or(OutputError::Failed)?.0;
    if let CommitDiff::File(file) = diff
        && let Some(from) = &file.from
    {
        // Compare these two paths exactly. A literal pathspec can also match
        // children if a path was a file on one side and a folder on the other.
        let mut cmd = command(root, &[], &["diff"]);
        cmd.args(WHOLE_FILE).args([
            "--end-of-options",
            &format!("{hash}^:{from}"),
            &format!("{hash}:{}", file.path),
            "--",
        ]);
        return checked_output(cmd, DIFF_LIMIT, None);
    }
    let mut cmd = command(root, &[], &["diff-tree", "--root", "--diff-merges=first-parent", "--no-commit-id", "-r"]);
    let (limit, cancel) = match diff {
        CommitDiff::Files(cancel) => {
            cmd.args(["-M", "--raw", "--numstat", "-z", "--end-of-options", hash, "--"]);
            (OUTPUT_LIMIT, Some(cancel))
        }
        CommitDiff::Paths => {
            // Keep both sides of renames visible, without counting lines in unrelated files.
            cmd.args(["-M", "--name-status", "-z", "--end-of-options", hash, "--"]);
            (OUTPUT_LIMIT, None)
        }
        CommitDiff::File(file) => {
            // A path also matches children. When a file becomes a folder or vice
            // versa, keep its addition or removal, not the changes below it.
            let filter = match file.kind {
                Kind::Added => "--diff-filter=A",
                Kind::Deleted => "--diff-filter=D",
                _ => "--diff-filter=MT",
            };
            cmd.args(WHOLE_FILE).args(["--no-renames", filter, "--end-of-options", hash, "--", &file.path]);
            (DIFF_LIMIT, None)
        }
    };
    checked_output(cmd, limit, cancel)
}

/// A missing ref or unrelated histories can give no object and exit with code 1.
pub(super) fn optional_oid(root: &Path, guard: &[String], args: &[&str]) -> Option<Option<String>> {
    let (status, raw) = capture(command(root, guard, args), 128, None).ok()?;
    if status.success() {
        let mut oid = String::from_utf8(raw).ok()?;
        let len = oid.strip_suffix('\n')?.len();
        oid.truncate(len);
        (matches!(len, 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit())).then_some(Some(oid))
    } else {
        (status.code() == Some(1) && raw.is_empty()).then_some(None)
    }
}

#[derive(Clone, Default)]
pub(super) struct Config {
    pub(super) guard: Vec<String>,
    pub(super) has_remote: bool,
}

/// Read remote names while listing the filters that must be disabled.
pub(super) fn config(root: &Path) -> Option<Config> {
    let names = limited_output(root, &[], &["config", "--null", "--name-only", "--list"], 1 << 20).ok()?;
    let names = std::str::from_utf8(&names).ok()?;
    let mut has_remote = false;
    let mut filters = Vec::new();
    for key in names.split_terminator('\0') {
        // Git config subsections (remote or filter names) can contain dots.
        if let Some((_, setting)) = key.strip_prefix("remote.").and_then(|key| key.rsplit_once('.')) {
            has_remote |= !setting.is_empty();
        }
        if let Some((name, _)) = key.strip_prefix("filter.").and_then(|key| key.rsplit_once('.')) {
            filters.push(name);
        }
    }
    filters.sort_unstable();
    filters.dedup();
    Some(Config { guard: filters.into_iter().map(str::to_owned).collect(), has_remote })
}

/// Reads the settings of the repository holding `dir` on a thread of its own
/// while `main` runs on this one. `None` leaves them to be read later.
pub(super) fn config_alongside<T>(dir: &Path, main: impl FnOnce() -> T) -> (Option<Config>, T) {
    let cancel = REFRESH_CANCEL.get();
    std::thread::scope(|scope| {
        let reading = std::thread::Builder::new()
            .spawn_scoped(scope, move || {
                REFRESH_CANCEL.set(cancel);
                config(dir)
            })
            .ok();
        let answer = main();
        (reading.and_then(|thread| thread.join().ok().flatten()), answer)
    })
}

/// Read the user's Git settings, but always inspect the requested checkout
/// with milo's own output options, even when launched by Git.
fn isolated(mut c: Command) -> Command {
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_GRAFT_FILE",
        "GIT_SHALLOW_FILE",
        "GIT_REPLACE_REF_BASE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CEILING_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_INTERNAL_SUPER_PREFIX",
        "GIT_EXEC_PATH",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_EXTERNAL_DIFF",
        "GIT_DIFF_OPTS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
        "GIT_TRACE",
        "GIT_TRACE_PACK_ACCESS",
        "GIT_TRACE_PACKET",
        "GIT_TRACE_PACKFILE",
        "GIT_TRACE_PERFORMANCE",
        "GIT_TRACE_SETUP",
        "GIT_TRACE_SHALLOW",
        "GIT_TRACE2",
        "GIT_TRACE2_EVENT",
        "GIT_TRACE2_PERF",
    ] {
        if std::env::var_os(key).is_some() {
            c.env_remove(key);
        }
    }
    c.env("LC_ALL", "C").stdin(Stdio::null()).stderr(Stdio::null());
    c
}

/// Disable the repository's own programs and avoid writing to its index.
fn command(root: &Path, guard: &[String], args: &[&str]) -> Command {
    let mut c = isolated(Command::new(&*GIT));
    c.args(["--no-pager", "--no-optional-locks"])
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=false"])
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"])
        .args(["-c", "safe.bareRepository=explicit"])
        // Comparing the whole tree must not write a refreshed index.
        .args(["-c", "diff.autoRefreshIndex=false", "-c", "diff.suppressBlankEmpty=false"]);
    // Filter names can contain '='. Unlike `-c`, the environment passes keys
    // and values separately.
    let settings = [("clean", ""), ("smudge", ""), ("process", ""), ("required", "false")];
    for (i, name) in guard.iter().enumerate() {
        for (j, (setting, value)) in settings.iter().enumerate() {
            let n = i * settings.len() + j;
            c.env(format!("GIT_CONFIG_KEY_{n}"), format!("filter.{name}.{setting}"))
                .env(format!("GIT_CONFIG_VALUE_{n}"), value);
        }
    }
    if !guard.is_empty() {
        c.env("GIT_CONFIG_COUNT", (guard.len() * settings.len()).to_string());
    }
    c.env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        // Older Git can try a lazy fetch despite GIT_NO_LAZY_FETCH.
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_TERMINAL_PROMPT", "0");
    match args {
        [kind @ ("diff" | "diff-tree"), tail @ ..] => c
            .args([*kind, "--no-color", "--no-ext-diff", "--no-textconv", "--no-relative", "-O/dev/null"])
            .args(["--ignore-submodules=dirty", "--submodule=short", "--src-prefix=a/", "--dst-prefix=b/"])
            .args(tail),
        _ => c.args(args),
    };
    c
}

fn checked_output(command: Command, limit: u64, cancel: Option<BorrowedFd<'_>>) -> Result<Vec<u8>, OutputError> {
    let (status, out) = capture(command, limit, cancel)?;
    if status.success() { Ok(out) } else { Err(OutputError::Failed) }
}

/// Kills and reaps a command on cancellation or when a limit is exceeded.
fn capture(
    mut command: Command,
    limit: u64,
    cancel: Option<BorrowedFd<'_>>,
) -> Result<(ExitStatus, Vec<u8>), OutputError> {
    let started = Instant::now();
    let cancel = cancel.map_or_else(|| REFRESH_CANCEL.get(), |fd| fd.as_raw_fd());
    let mut stop = libc::pollfd { fd: cancel, events: libc::POLLIN, revents: 0 };
    if cancel >= 0 {
        if term::wait_ready(std::slice::from_mut(&mut stop), 0) > 0 {
            return Err(OutputError::Failed);
        }
        // On cancellation, stop any helpers that may be holding stdout open.
        command.process_group(0);
    }
    let mut child = command.stdout(Stdio::piped()).spawn().map_err(|_| OutputError::Unavailable)?;
    let pipe = child.stdout.take().expect("git stdout was piped");
    let result = collect(pipe, limit, started, stop).and_then(|out| wait(&mut child, started, stop).map(|s| (s, out)));
    if result.is_err() {
        if cancel >= 0
            && let Ok(group) = libc::pid_t::try_from(child.id())
            && group > 1
        {
            // SAFETY: The child leads a new group and has not been reaped, so its ID
            // still belongs to this command even if it has already exited.
            unsafe { libc::killpg(group, libc::SIGKILL) };
        } else {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
    result
}

fn collect(mut pipe: ChildStdout, limit: u64, started: Instant, stop: libc::pollfd) -> Result<Vec<u8>, OutputError> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 1 << 16];
    let mut fds = [libc::pollfd { fd: pipe.as_raw_fd(), events: libc::POLLIN, revents: 0 }, stop];
    while running(started) {
        if term::wait_ready(&mut fds, 200) <= 0 {
            continue;
        }
        if fds[1].revents != 0 {
            break;
        }
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(out),
            Ok(n) if out.len() as u64 + n as u64 > limit => return Err(OutputError::TooLarge),
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    Err(OutputError::Failed)
}

fn wait(child: &mut Child, started: Instant, mut stop: libc::pollfd) -> Result<ExitStatus, OutputError> {
    // Closing stdout is not proof that the child has exited.
    let stop = std::slice::from_mut(&mut stop);
    if stop[0].fd >= 0 && term::wait_ready(stop, 0) > 0 {
        return Err(OutputError::Failed);
    }
    while running(started) {
        if let Some(status) = child.try_wait().map_err(|_| OutputError::Failed)? {
            return Ok(status);
        }
        if term::wait_ready(stop, 1) > 0 {
            break;
        }
    }
    Err(OutputError::Failed)
}

fn running(started: Instant) -> bool {
    started.elapsed() < DEADLINE && !term::TERMINATED.load(Ordering::Relaxed)
}
