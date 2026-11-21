//! Runs read-only git commands without letting a repository run its own programs.

use std::ffi::OsStr;
use std::io::Read as _;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
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
    let mut launcher = isolated(Command::new("git"));
    launcher.arg("--exec-path");
    let real = checked_output(launcher, 4096)
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

pub(super) fn limited_output(root: &Path, guard: &[String], args: &[&str], limit: u64) -> Result<Vec<u8>, OutputError> {
    checked_output(command(root, guard, args), limit)
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
    checked_output(cmd, DIFF_LIMIT)
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

pub(super) enum CommitDiff<'a> {
    Files,
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
        return checked_output(cmd, DIFF_LIMIT);
    }
    let mut cmd = command(root, &[], &["diff-tree", "--root", "--diff-merges=first-parent", "--no-commit-id", "-r"]);
    let limit = match diff {
        CommitDiff::Files => {
            cmd.args(["-M", "--raw", "--numstat", "-z", "--end-of-options", hash, "--"]);
            OUTPUT_LIMIT
        }
        CommitDiff::Paths => {
            // Keep both sides of renames visible, without counting lines in unrelated files.
            cmd.args(["-M", "--name-status", "-z", "--end-of-options", hash, "--"]);
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
    let (status, raw) = capture(command(root, guard, args), 128).ok()?;
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

fn checked_output(command: Command, limit: u64) -> Result<Vec<u8>, OutputError> {
    let (status, out) = capture(command, limit)?;
    if status.success() { Ok(out) } else { Err(OutputError::Failed) }
}

/// Kills and reaps a command that cannot finish within the time or output limit.
fn capture(mut command: Command, limit: u64) -> Result<(ExitStatus, Vec<u8>), OutputError> {
    let started = Instant::now();
    let mut child = command.stdout(Stdio::piped()).spawn().map_err(|_| OutputError::Unavailable)?;
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
