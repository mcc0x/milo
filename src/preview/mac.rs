use std::ffi::{CStr, OsStr};
use std::fs::{self, DirBuilder};
use std::io::{self, BufRead as _, IsTerminal as _, Read as _, Write as _};
use std::num::NonZeroU32;
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use super::Anchor;
use crate::fs_ops;

const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/Milo Preview"));
const FOCUS_FD_ENV: &str = "MILO_PREVIEW_FOCUS_FD";
const PIN_REFUSED: i32 = 2; // PinRefused in macos/Preview/main.m.
const PIN_REFUSAL_PREFIX: &[u8] = b"refused: ";
const PIN_REASON_MAX: usize = 160;
const PIN_REPLY_MAX: usize = PIN_REFUSAL_PREFIX.len() + PIN_REASON_MAX + 1; // Includes the newline.
const ABNORMAL_PIN_EXIT: &str = "the preview window check ended unexpectedly; no pane was opened";
const INVALID_PIN_SUCCESS: &str = "the preview window check returned an invalid window ID; no pane was opened";

fn focus_pipe() -> io::Result<(io::PipeReader, io::PipeWriter)> {
    let (reader, writer) = io::pipe()?;
    // SAFETY: writer owns this open pipe. A helper that has already closed
    // must not terminate the launcher when a synchronous Rex reply arrives.
    if unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETNOSIGPIPE, 1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((reader, writer))
}

fn inherit_focus_reader(command: &mut Command, reader: io::PipeReader) {
    command.env(FOCUS_FD_ENV, reader.as_raw_fd().to_string());
    let inherit = move || {
        // SAFETY: the closure owns this open reader.
        if unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    };
    // SAFETY: the hook runs only async-signal-safe fcntl and captures errno on failure.
    unsafe { command.pre_exec(inherit) };
}

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
    // Never replace the experimental executable a user may have listed in Accessibility.
    let dir = path.join("net.usemilo.preview.tiled");
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
fn spawn(args: &[&str], state: Stdio, focus: Option<io::PipeReader>) -> io::Result<Child> {
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
    // Only fixed protocol records belong on stdout; do not relay page diagnostics to the shell.
    let mut command = Command::new(path);
    command.args(args).stdin(state).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(reader) = focus {
        inherit_focus_reader(&mut command, reader);
    }
    command.spawn()
}

pub(crate) fn pin_window(owner_pid: u32) -> Result<NonZeroU32, String> {
    let mut helper = spawn(&["--pin-window", &owner_pid.to_string()], Stdio::null(), None)
        .map_err(|e| format!("could not run the preview window check: {e}"))?;
    let mut stdout = helper.stdout.take().expect("helper stdout is piped");
    let mut reply = [0; PIN_REPLY_MAX + 1];
    let mut size = 0;
    // Read one byte beyond the longest valid record to distinguish it from a
    // truncated one. Close the pipe before waiting even if it had more data.
    let read = loop {
        if size == reply.len() {
            break Ok(());
        }
        match stdout.read(&mut reply[size..]) {
            Ok(0) => break Ok(()),
            Ok(n) => size += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => break Err(e),
        }
    };
    drop(stdout);
    let status = helper.wait().map_err(|_| ABNORMAL_PIN_EXIT)?;
    read.map_err(|_| ABNORMAL_PIN_EXIT)?;
    parse_pin_reply(status.code(), &reply[..size])
}

fn parse_pin_reply(code: Option<i32>, reply: &[u8]) -> Result<NonZeroU32, String> {
    let line = reply.strip_suffix(b"\n");
    match code {
        Some(0) => line
            .filter(|id| id.len() <= 10 && matches!(id.first().copied(), Some(b'1'..=b'9')))
            .and_then(|id| std::str::from_utf8(id).ok()?.parse().ok())
            .ok_or_else(|| INVALID_PIN_SUCCESS.into()),
        Some(PIN_REFUSED) => {
            let reason = line
                .and_then(|line| line.strip_prefix(PIN_REFUSAL_PREFIX))
                .filter(|reason| {
                    !reason.is_empty()
                        && reason.len() <= PIN_REASON_MAX
                        && !reason.starts_with(b" ")
                        && !reason.ends_with(b" ")
                        && reason.iter().all(|byte| (b' '..=b'~').contains(byte))
                })
                .and_then(|reason| std::str::from_utf8(reason).ok());
            Err(match reason {
                Some(reason) => format!("Preview could not pin a Rex window: {reason}. No pane was opened."),
                None => ABNORMAL_PIN_EXIT.into(),
            })
        }
        _ => Err(ABNORMAL_PIN_EXIT.into()),
    }
}

/// The helper and observer run only for this terminal. In particular, failure
/// to start WebKit must not leave the observer waiting for more session events.
pub(crate) fn anchor(url: &str, anchor: Anchor<'_>) -> Result<(), String> {
    let result = run_anchor(url, anchor);
    if let Err(error) = &result {
        let input = io::stdin();
        if input.is_terminal() && io::stderr().is_terminal() {
            // Keep the diagnostic visible without keeping WebKit or Rex running.
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

    let (input, state) = io::pipe().map_err(|_| "could not open the preview observer pipe")?;
    let mut watch = crate::rex::preview_observer_command(anchor, state)?;
    let (reader, reply) = focus_pipe().map_err(|_| "could not open the preview focus acknowledgment pipe")?;
    let mut helper = spawn(&[url, &anchor.native_window_id.to_string()], input.into(), Some(reader))
        .map_err(|_| "could not launch the local WebKit helper")?;
    // No observation may precede the helper's clock baseline, including
    // when installation or launch stalls across a system clock change.
    let mut watcher = match after_clock_ready(&mut helper, move || {
        watch.spawn().map_err(|_| "couldn't start the local Rex preview observer".into())
    }) {
        Ok(watcher) => watcher,
        Err(error) => {
            let _ = helper.kill();
            let _ = helper.wait();
            return Err(error);
        }
    };
    let result = serve_actions(helper, reply, || crate::rex::return_preview_focus(anchor).is_ok());
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
        if result.is_ok() && !status.success() {
            return Err("the Rex preview observer failed; the preview was closed".into());
        }
    } else {
        let _ = watcher.kill();
        let _ = watcher.wait();
    }
    result
}

fn after_clock_ready<T>(helper: &mut Child, start: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let output = helper.stdout.as_mut().expect("helper stdout is piped");
    let mut fds = [libc::pollfd { fd: output.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
    let mut receipt = [0];
    if crate::term::wait_ready(&mut fds, 8_000) <= 0 || output.read_exact(&mut receipt).is_err() || receipt != *b"R" {
        return Err("the WebKit helper did not initialize the preview observer; no page was shown".into());
    }
    start()
}

fn serve_actions(
    mut helper: Child,
    mut reply: io::PipeWriter,
    mut return_focus: impl FnMut() -> bool,
) -> Result<(), String> {
    let mut actions = io::BufReader::new(helper.stdout.take().expect("helper stdout is piped"));
    let mut action = String::new();
    loop {
        action.clear();
        match actions.read_line(&mut action) {
            Ok(0) => break,
            Ok(_) if action == "focus\n" => {
                // A tab may disappear while Rex handles the request. Never
                // activate a different terminal or discard the existing page.
                let result = if return_focus() { b"1" } else { b"0" };
                if reply.write_all(result).is_err() {
                    // The helper may have closed while Rex was handling the
                    // request. EOF makes any surviving helper close as well.
                    break;
                }
            }
            _ => {
                let _ = helper.kill();
                break;
            }
        }
    }
    drop(reply);
    if helper.wait().map_err(|_| "could not wait for the WebKit helper")?.success() {
        Ok(())
    } else {
        Err("WebKit preview stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_helper_that_exits_during_startup_cannot_start_the_observer() {
        let mut helper = Command::new("/usr/bin/true").stdout(Stdio::piped()).spawn().unwrap();
        let result = after_clock_ready(&mut helper, || -> Result<(), String> {
            panic!("observed the Rex layout before the helper established its clock baseline")
        });
        assert!(result.is_err());
        helper.wait().unwrap();
    }

    #[test]
    fn pin_success_requires_a_single_canonical_window_id() {
        assert_eq!(parse_pin_reply(Some(0), b"4294967295\n").unwrap().get(), u32::MAX);
        for reply in [
            b"".as_slice(),
            b"42",
            b"0\n",
            b"042\n",
            b"+42\n",
            b"42 \n",
            b"42\n43\n",
            b"4294967296\n",
            b"refused: another window overlaps Rex\n",
        ] {
            assert_eq!(parse_pin_reply(Some(0), reply).unwrap_err(), INVALID_PIN_SUCCESS);
        }
    }

    #[test]
    fn pin_refusal_requires_the_refusal_status_and_bounded_printable_ascii() {
        let refusal = b"refused: another window overlaps Rex\n";
        assert_eq!(
            parse_pin_reply(Some(PIN_REFUSED), refusal).unwrap_err(),
            "Preview could not pin a Rex window: another window overlaps Rex. No pane was opened."
        );
        for code in [None, Some(1)] {
            assert_eq!(parse_pin_reply(code, refusal).unwrap_err(), ABNORMAL_PIN_EXIT);
        }
        for reply in [
            b"42\n".as_slice(),
            b"refused: \n",
            b"refused:  overlap\n",
            b"refused: overlap \n",
            b"refused: overlap",
            b"refused: overlap\r\n",
            b"refused: overlap\nextra\n",
            b"refused: \x1b[2J\n",
            b"refused: \xff\n",
        ] {
            assert_eq!(parse_pin_reply(Some(PIN_REFUSED), reply).unwrap_err(), ABNORMAL_PIN_EXIT);
        }
        let max = format!("refused: {}\n", "x".repeat(PIN_REASON_MAX));
        assert_eq!(max.len(), PIN_REPLY_MAX);
        assert!(parse_pin_reply(Some(PIN_REFUSED), max.as_bytes()).unwrap_err().contains(&"x".repeat(PIN_REASON_MAX)));
        let over_max = format!("refused: {}\n", "x".repeat(PIN_REASON_MAX + 1));
        assert_eq!(parse_pin_reply(Some(PIN_REFUSED), over_max.as_bytes()).unwrap_err(), ABNORMAL_PIN_EXIT);
    }

    #[test]
    fn slow_focus_acknowledges_only_after_the_rpc_approves() {
        let (reader, reply) = focus_pipe().unwrap();
        let monitor = reader.try_clone().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", r#"printf 'Rfocus\n'; test "$(/usr/bin/head -c 1 "/dev/fd/$MILO_PREVIEW_FOCUS_FD")" = 1"#])
            .stdout(Stdio::piped());
        inherit_focus_reader(&mut command, reader);
        let mut helper = command.spawn().unwrap();
        drop(command);
        after_clock_ready(&mut helper, || Ok(())).unwrap();
        let mut attempts = 0;
        serve_actions(helper, reply, || {
            attempts += 1;
            let mut fds = [libc::pollfd { fd: monitor.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            // Simulate a Rex RPC that takes long enough for the child to wait on the pipe.
            assert_eq!(crate::term::wait_ready(&mut fds, 25), 0);
            true
        })
        .unwrap();
        assert_eq!(attempts, 1);
    }

    #[test]
    fn refused_focus_preserves_the_helper_for_a_later_explicit_return() {
        let (reader, reply) = focus_pipe().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", r#"
                for expected in 0 1; do
                    printf 'focus\n'
                    test "$(/usr/bin/head -c 1 "/dev/fd/$MILO_PREVIEW_FOCUS_FD")" = "$expected" || exit 1
                done
            "#])
            .stdout(Stdio::piped());
        inherit_focus_reader(&mut command, reader);
        let helper = command.spawn().unwrap();
        drop(command);
        let mut attempts = 0;
        serve_actions(helper, reply, || {
            attempts += 1;
            attempts == 2
        })
        .unwrap();
        assert_eq!(attempts, 2);
    }

    #[test]
    fn helper_can_end_during_a_successful_focus_rpc() {
        let helper = Command::new("/bin/sh").args(["-c", "printf 'focus\\n'"]).stdout(Stdio::piped()).spawn().unwrap();
        let (reader, reply) = focus_pipe().unwrap();
        drop(reader);
        serve_actions(helper, reply, || true).unwrap();
    }
}
