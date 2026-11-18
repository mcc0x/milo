//! Runs read-only git commands without letting a repository run its own programs.

use std::io::Read as _;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::LazyLock;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::term;

/// No general answer from git is kept beyond this.
const OUTPUT_LIMIT: u64 = 256 << 20;
/// How long a git command may run before it is killed.
const DEADLINE: Duration = Duration::from_secs(15);

/// The git program itself. On macOS the `git` on the path is a launcher
/// that takes as long again to start as git does, so the real one is found
/// once. If it cannot be found, use `git` from the path.
pub(crate) static GIT: LazyLock<PathBuf> = LazyLock::new(|| {
    let mut launcher = Command::new("git");
    launcher.arg("--exec-path").stdin(Stdio::null()).stderr(Stdio::null());
    let real = checked_output(launcher, OUTPUT_LIMIT)
        .ok()
        .and_then(|raw| String::from_utf8(raw).ok())
        .map(|path| Path::new(path.trim()).join("git"));
    real.filter(|path| path.is_file()).unwrap_or_else(|| PathBuf::from("git"))
});

pub(super) enum OutputError {
    TooLarge,
    Failed,
}

pub(super) fn output(root: &Path, guard: &[String], args: &[&str]) -> Option<Vec<u8>> {
    limited_output(root, guard, args, OUTPUT_LIMIT).ok()
}

pub(super) fn limited_output(root: &Path, guard: &[String], args: &[&str], limit: u64) -> Result<Vec<u8>, OutputError> {
    checked_output(command(root, guard, args), limit)
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

/// A filter is a program run on file content. Turn off every filter a project defines.
pub(super) fn filter_guard(root: &Path) -> Option<Vec<String>> {
    // Listing all keys succeeds even when there are no filters, unlike a pattern query.
    let names = String::from_utf8(output(root, &[], &["config", "--null", "--name-only", "--list"])?).ok()?;
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

/// A repository's configuration can name programs for git to run: a file
/// monitor, hooks, filters, a transport for fetching what is missing.
/// Looking at a project must not run them.
fn command(root: &Path, guard: &[String], args: &[&str]) -> Command {
    let mut c = Command::new(&*GIT);
    c.arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=false"])
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"])
        .args(["-c", "safe.bareRepository=explicit"])
        // Checking the index on every core costs more than it saves for a frequent viewer.
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
        if term::poll(&mut fds, 200) <= 0 {
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
