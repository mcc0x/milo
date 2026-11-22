use std::ffi::{CStr, OsStr};
use std::fs::{self, DirBuilder};
use std::io::{self, BufRead as _, IsTerminal as _, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use super::Anchor;
use crate::fs_ops;

const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/Milo Preview"));

/// Install independently of the checkout, PATH, HOME or shell's temporary directory.
/// The OS may evict this cache; this location does not establish a privacy identity.
fn cache() -> io::Result<PathBuf> {
    let mut bytes = [0u8; 1024];
    // SAFETY: bytes is writable for the full size passed to confstr.
    let len = unsafe { libc::confstr(libc::_CS_DARWIN_USER_CACHE_DIR, bytes.as_mut_ptr().cast(), bytes.len()) };
    let path = bytes
        .get(..len)
        .and_then(|s| CStr::from_bytes_with_nul(s).ok())
        .map(|s| Path::new(OsStr::from_bytes(s.to_bytes())))
        .filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::other("no private macOS user cache"))?;
    let private = |p: &Path| -> io::Result<()> {
        let meta = fs::symlink_metadata(p)?;
        // SAFETY: geteuid takes no arguments and does not access any caller-supplied memory.
        if meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0 {
            Ok(())
        } else {
            Err(io::Error::other("preview needs a private user cache directory"))
        }
    };
    private(path)?;
    let dir = path.join("net.usemilo.preview");
    match DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => (),
    }
    private(&dir)?;
    Ok(dir)
}

fn matches_build(path: &Path) -> io::Result<bool> {
    let mut file = match fs_ops::open_read(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        other => other?,
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("unexpected file in preview cache"));
    }
    if meta.len() != HELPER.len() as u64 || meta.mode() & 0o777 != 0o500 {
        return Ok(false);
    }
    let mut buf = [0u8; 8192];
    for chunk in HELPER.chunks(buf.len()) {
        let part = &mut buf[..chunk.len()];
        file.read_exact(part)?;
        if part != chunk {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Keep the lock through spawn: two Milo versions must each launch the helper
/// they built. Publish a fresh inode rather than modifying a running executable.
fn spawn(url: &str, state: Stdio) -> io::Result<Child> {
    let dir = cache()?;
    let lock = fs_ops::open_dir(&dir)?;
    lock.lock()?;
    let path = dir.join("Milo Preview");
    if !matches_build(&path)? {
        let (staged, mut file) = fs_ops::Temp::create(dir.join(format!(".preview-{}", std::process::id())))?;
        file.write_all(HELPER)?;
        file.set_permissions(fs::Permissions::from_mode(0o500))?;
        drop(file);
        staged.replace(&path)?;
    }
    // Framework diagnostics may contain page data and must not overwrite the
    // marker. Only Milo's own static errors belong on the restored anchor tty.
    Command::new(path).arg(url).stdin(state).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
}

pub(crate) fn require_access() -> Result<(), String> {
    let mut check = spawn("--check-access", Stdio::null()).map_err(|e| format!("could not open Milo Preview: {e}"))?;
    match check.wait().map_err(|e| e.to_string())?.code() {
        Some(0) => Ok(()),
        Some(77) => Err("macOS Accessibility is unavailable for this launch context; no pane was opened. This build has no verified permission-free placement. Do not grant Rex or its server Accessibility as a workaround.".into()),
        _ => Err("could not check Milo Preview accessibility; no pane was opened".into()),
    }
}

/// The helper and observer run only for this terminal. In particular, failure
/// to start WebKit must not leave the observer waiting for more session events.
pub(crate) fn anchor(url: &str, anchor: Anchor<'_>) -> Result<(), String> {
    let result = run_anchor(url, anchor);
    if let Err(error) = &result {
        let input = io::stdin();
        if input.is_terminal() && io::stderr().is_terminal() {
            // Native restores the terminal before it exits. Keep the
            // diagnostic visible without keeping WebKit or Rex running.
            // Do not let a saturated or flow-stopped PTY keep the parent alive.
            // Use a separate file description so the shell's flags are unchanged.
            if let Ok(mut tty) = crate::term::terminal_output(io::stderr().as_raw_fd())
                && writeln!(tty, "milo preview: {error}\n\nPress Enter to dismiss (closes automatically in 15 seconds).").is_ok()
            {
                let mut fds = [libc::pollfd { fd: input.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
                let _ = crate::term::wait_ready(&mut fds, 15_000);
            }
            // Never retry on blocking stderr in the CLI's top-level error path.
            std::process::exit(1);
        }
    }
    result
}

fn run_anchor(url: &str, anchor: Anchor<'_>) -> Result<(), String> {
    use std::time::{Duration, Instant};

    let mut watcher = crate::rex::preview_watch(anchor)?;
    let result = run(url, anchor, &mut watcher);
    // EOF can reach the helper before Rex's exit status is waitable. Give it
    // a short grace period, but report nothing from the Rex process itself.
    let deadline = Instant::now() + Duration::from_millis(80);
    let status = loop {
        match watcher.try_wait() {
            Ok(None) if result.is_ok() && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            status => break status.ok().flatten(),
        }
    };
    if let Some(status) = status {
        if !status.success() {
            return Err("the Rex preview observer failed; the preview was closed".into());
        }
    } else {
        let _ = watcher.kill();
        let _ = watcher.wait();
    }
    result
}

fn run(url: &str, anchor: Anchor<'_>, watcher: &mut Child) -> Result<(), String> {
    let state = watcher.stdout.take().ok_or("missing preview observer pipe")?;
    let mut helper = spawn(url, state.into()).map_err(|_| "could not launch the local WebKit helper")?;
    let mut actions = io::BufReader::new(helper.stdout.take().expect("helper stdout is piped"));
    let mut action = String::new();
    loop {
        action.clear();
        match actions.read_line(&mut action) {
            Ok(0) => break,
            Ok(_) if action == "focus\n" => {
                let _ = crate::rex::return_preview_focus(anchor);
            }
            _ => {
                let _ = helper.kill();
                break;
            }
        }
    }
    if helper.wait().map_err(|_| "could not wait for the WebKit helper")?.success() {
        Ok(())
    } else {
        Err("WebKit preview stopped".into())
    }
}
