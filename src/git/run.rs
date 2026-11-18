//! Runs read-only git commands without letting a repository run its own programs.

use std::io::Read as _;
use std::os::fd::AsRawFd;
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

/// Cache the executable so frequent commands can bypass the macOS git launcher.
pub(crate) static GIT: LazyLock<PathBuf> = LazyLock::new(|| {
    let mut launcher = Command::new("git");
    launcher.arg("--exec-path").stdin(Stdio::null()).stderr(Stdio::null());
    let real = checked_output(launcher, OUTPUT_LIMIT)
        .ok()
        .and_then(|raw| String::from_utf8(raw).ok())
        .map(|path| Path::new(path.trim()).join("git"));
    real.filter(|path| path.is_file()).unwrap_or_else(|| PathBuf::from("git"))
});

pub(crate) enum OutputError {
    TooLarge,
    Failed,
}

pub(super) fn output(root: &Path, guard: &[String], args: &[&str]) -> Option<Vec<u8>> {
    limited_output(root, guard, args, OUTPUT_LIMIT).ok()
}

pub(super) fn limited_output(root: &Path, guard: &[String], args: &[&str], limit: u64) -> Result<Vec<u8>, OutputError> {
    checked_output(command(root, guard, args), limit)
}

pub(super) enum CommitDiff<'a> {
    Files,
    File(&'a PendingFile),
}

// Full context within the output limit, with the prefixes the patch parser expects.
const WHOLE_FILE: [&str; 4] =
    ["--unified=8388608", "--output-indicator-new=+", "--output-indicator-old=-", "--output-indicator-context= "];

/// Reads one commit against its first parent, or the empty tree for a root commit.
pub(super) fn commit_diff(
    root: &Path,
    guard: &[String],
    hash: &str,
    diff: CommitDiff<'_>,
) -> Result<Vec<u8>, OutputError> {
    let hash = super::CommitId::parse(hash).ok_or(OutputError::Failed)?.0;
    if let CommitDiff::File(file) = diff
        && let Some(from) = &file.from
    {
        // Compare these two paths exactly. A literal pathspec can also match
        // children if a path was a file on one side and a folder on the other.
        let mut cmd = command(root, guard, &["diff", "--no-color", "--no-ext-diff", "--no-textconv"]);
        cmd.args(WHOLE_FILE).args([
            "--end-of-options",
            &format!("{hash}^:{from}"),
            &format!("{hash}:{}", file.path),
            "--",
        ]);
        return checked_output(cmd, DIFF_LIMIT);
    }
    let mut cmd = command(
        root,
        guard,
        &[
            "diff-tree",
            "--root",
            "--diff-merges=first-parent",
            "--no-commit-id",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--submodule=short",
            "-r",
        ],
    );
    let limit = match diff {
        CommitDiff::Files => {
            cmd.args(["-M", "--raw", "--numstat", "-z", "--end-of-options", hash, "--"]);
            OUTPUT_LIMIT
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
            DIFF_LIMIT
        }
    };
    checked_output(cmd, limit)
}

/// A missing ref or unrelated histories can give no object and exit with code 1.
pub(super) fn optional_oid(root: &Path, guard: &[String], args: &[&str]) -> Option<Option<String>> {
    let (status, raw) = capture(command(root, guard, args), OUTPUT_LIMIT).ok()?;
    if status.success() {
        let mut oid = String::from_utf8(raw).ok()?;
        oid.truncate(oid.trim_end().len());
        (!oid.is_empty()).then_some(Some(oid))
    } else {
        (status.code() == Some(1) && raw.is_empty()).then_some(None)
    }
}

pub(super) struct Config {
    pub(super) guard: Vec<String>,
    pub(super) has_remote: bool,
}

/// Read remote names while listing the filters that must be disabled.
pub(super) fn config(root: &Path) -> Option<Config> {
    let names = output(root, &[], &["config", "--null", "--name-only", "--list"])?;
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
    let mut guard = Vec::new();
    for name in filters {
        for setting in ["clean=", "smudge=", "process=", "required=false"] {
            guard.extend(["-c".to_string(), format!("filter.{name}.{setting}")]);
        }
    }
    Some(Config { guard, has_remote })
}

/// Disable the repository's own programs and avoid writing to its index.
fn command(root: &Path, guard: &[String], args: &[&str]) -> Command {
    let mut c = Command::new(&*GIT);
    c.arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=false"])
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"])
        .args(["-c", "safe.bareRepository=explicit"])
        // Repeated small reads do not repay the cost of parallel index preloading.
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

fn checked_output(command: Command, limit: u64) -> Result<Vec<u8>, OutputError> {
    let (status, out) = capture(command, limit)?;
    if status.success() { Ok(out) } else { Err(OutputError::Failed) }
}

/// Kills and reaps a command that cannot finish within the time or output limit.
fn capture(mut command: Command, limit: u64) -> Result<(ExitStatus, Vec<u8>), OutputError> {
    let started = Instant::now();
    let mut child = command.stdout(Stdio::piped()).spawn().map_err(|_| OutputError::Failed)?;
    let pipe = child.stdout.take().expect("git stdout was piped");
    let result = collect(pipe, limit, started).and_then(|out| wait(&mut child, started).map(|status| (status, out)));
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn collect(mut pipe: ChildStdout, limit: u64, started: Instant) -> Result<Vec<u8>, OutputError> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 1 << 16];
    while running(started) {
        let mut fds = [libc::pollfd { fd: pipe.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if term::wait_ready(&mut fds, 200) <= 0 {
            continue;
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

fn wait(child: &mut Child, started: Instant) -> Result<ExitStatus, OutputError> {
    // Closing stdout is not proof that the child has exited.
    while running(started) {
        if let Some(status) = child.try_wait().map_err(|_| OutputError::Failed)? {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err(OutputError::Failed)
}

fn running(started: Instant) -> bool {
    started.elapsed() < DEADLINE && !term::TERMINATED.load(Ordering::Relaxed)
}
