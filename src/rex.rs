//! Calls Rex for floating readers, setup, and the new-tab listener.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use crate::term;

const PROBE_WAIT: Duration = Duration::from_secs(3);

/// Inspect the server catalog, not the foreground app. Doctor must not start a server.
pub(crate) fn layout_available() -> Option<bool> {
    let mut command = Command::new("rex");
    command.args(["--autostart=false", "--timeout", "3s", "actions", "--json"]).env_remove("REX_CLIENT");
    let output = probe_output(&mut command, PROBE_WAIT)?;
    let [actions] = crate::json::Value::parse(&output)?.fields(["actions"]);
    Some(
        actions?
            .elements()?
            .any(|action| action.fields(["name"])[0].is_some_and(|name| name.is("milo_remember_layout"))),
    )
}

/// The shell that opened setup, even if focus has moved somewhere else.
pub struct Target {
    pub session: OsString,
    pub block: OsString,
}

impl Target {
    pub fn current() -> Option<Self> {
        Some(Self { session: std::env::var_os("REX_SESSION")?, block: std::env::var_os("REX_BLOCK")? })
    }
}

/// Run a noninteractive local Lua script against an existing Rex session.
pub(crate) fn script(
    target: Option<&Target>,
    session: &OsStr,
    lua: &str,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Option<String> {
    let mut command = Command::new("rex");
    command.args(["--autostart=false", "--timeout", "3s", "do", "-", "-s"]).arg(session);
    if let Some(target) = target {
        command.arg("-b").arg(&target.block);
    }
    let mut child =
        command.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let written = child.stdin.take().is_some_and(|mut pipe| pipe.write_all(lua.as_bytes()).is_ok());
    let output = child.wait_with_output().ok()?;
    if !written || !output.status.success() {
        return None;
    }
    let mut text = String::from_utf8(output.stdout).ok()?;
    text.truncate(text.trim_end().len());
    Some(text)
}

/// Doctor's read-only script has a local deadline as well as Rex's request
/// timeout. Passing Lua as an argument keeps stdin from blocking the probe.
pub(crate) fn diagnostic_script(target: &Target, lua: &str, caller: &str) -> Option<String> {
    let mut command = Command::new("rex");
    command
        .args(["--autostart=false", "--timeout", "3s", "do", "-e", lua, "-s"])
        .arg(&target.session)
        .arg("-b")
        .arg(&target.block)
        .args(["milo_operation=doctor", caller]);
    let mut text = String::from_utf8(probe_output(&mut command, PROBE_WAIT)?).ok()?;
    text.truncate(text.trim_end().len());
    Some(text)
}

fn probe_output(command: &mut Command, wait: Duration) -> Option<Vec<u8>> {
    const LIMIT: usize = 1 << 20;
    let deadline = Instant::now() + wait;
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let result = (|| {
        let mut stdout = child.stdout.take()?;
        let mut poll = [libc::pollfd { fd: stdout.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        let mut output = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match term::wait_ready(&mut poll, term::milliseconds(left)) {
                0 => return None,
                n if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => continue,
                n if n < 0 => return None,
                _ => {}
            }
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) if output.len() + n <= LIMIT => output.extend_from_slice(&chunk[..n]),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                _ => return None,
            }
        }
        // A child can close stdout before it has finished.
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            if let Some(status) = child.try_wait().ok()? {
                return Some((status, output));
            }
            std::thread::sleep(left.min(Duration::from_millis(1)));
        }
    })();
    match result {
        Some((status, output)) => status.success().then_some(output),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Setup reports its own messages, not process output that can contain paths or settings.
pub fn run(args: &[&OsStr]) -> bool {
    let mut command = Command::new("rex");
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if args.starts_with(&[OsStr::new("config"), OsStr::new("check")]) {
        command.env("MILO_CHECKING_REX", "1");
    }
    command.status().is_ok_and(|status| status.success())
}

/// Replaces milo without releasing the listener's inherited lock.
///
/// The Rex server starts the listener with a short system path that leaves
/// `rex` out, so when it is not found by name, the server's own program is used.
pub fn exec_listener(script: &Path) -> io::Error {
    let listen = |rex: &OsStr| {
        Command::new(rex).arg("do").arg(script).arg("--all").stdin(Stdio::null()).stdout(Stdio::null()).exec()
    };
    let error = listen(OsStr::new("rex"));
    match parent_program() {
        Some(parent) if error.kind() == io::ErrorKind::NotFound && parent.file_name() == Some(OsStr::new("rex")) => {
            listen(parent.as_os_str())
        }
        _ => error,
    }
}

/// The program that started milo, if the system will say.
#[cfg(target_os = "macos")]
fn parent_program() -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let size = u32::try_from(path.len()).ok()?;
    // SAFETY: Asking for the parent's ID touches no memory.
    let parent = unsafe { libc::getppid() };
    // SAFETY: The buffer is writable for the size given.
    let len = unsafe { libc::proc_pidpath(parent, path.as_mut_ptr().cast(), size) };
    path.truncate(usize::try_from(len).ok().filter(|&len| len > 0)?);
    Some(OsString::from_vec(path).into())
}

/// The program that started milo, if the system will say.
#[cfg(not(target_os = "macos"))]
fn parent_program() -> Option<std::path::PathBuf> {
    // SAFETY: Asking for the parent's ID touches no memory.
    let parent = unsafe { libc::getppid() };
    std::fs::read_link(format!("/proc/{parent}/exe")).ok()
}

/// A string as a JSON literal.
fn json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => out.extend(['\\', c]),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether this is running in a Rex pane, where a file can be opened in a
/// floating window.
pub fn inside() -> bool {
    std::env::var_os("REX_SESSION").is_some()
}

/// Opens a floating reader in the caller's tab and returns its block ID.
/// The old reader stays in place if the replacement cannot be created.
pub fn open(root: &Path, file: &str, commit: Option<&str>, previous: Option<&str>) -> Result<String, &'static str> {
    let target = Target::current().ok_or("Reading a file needs a Rex pane.")?;
    let exe = std::env::current_exe().map_err(|_| "Rex did not open the file.")?;
    let (_, name) = file.rsplit_once('/').unwrap_or(("", file));
    let path = if commit.is_some() { file.into() } else { root.join(file) };
    let command = reader_command(&exe, &path, commit);
    let previous = previous.map(json).unwrap_or_else(|| "null".into());
    let args = format!(
        r#"{{"label":{},"cwd":{},"command":{command},"previous":{previous}}}"#,
        json(name),
        json(&root.to_string_lossy()),
    );
    script(Some(&target), &target.session, include_str!("rex_reader.lua"), ["--args", &args])
        .filter(|reply| reply.starts_with("block:") && !reply.contains(char::is_whitespace))
        .ok_or("Rex did not open the file.")
}

fn reader_command(exe: &Path, file: &Path, commit: Option<&str>) -> String {
    let args = commit.map_or_else(String::new, |hash| format!(r#","--commit",{}"#, json(hash)));
    format!("[{},\"view\",{}{args}]", json(&exe.to_string_lossy()), json(&file.to_string_lossy()))
}

/// A local Rex script watches the selected shell without periodic CLI calls.
pub struct ShellObserver {
    child: Child,
    output: ChildStdout,
    frame: Frame,
}

pub enum ShellChange {
    Unchanged,
    Directory(PathBuf),
    Unreported,
    Closed,
}

impl ShellObserver {
    pub fn start(anchor: &OsStr) -> Option<Self> {
        let target = Target::current()?;
        let anchor = exact_block(anchor)?;
        let pane = exact_block(&target.block)?;
        let args = format!(r#"{{"anchor":{},"pane":{}}}"#, json(anchor), json(pane));
        let mut child = Command::new("rex")
            .args(["--autostart=false", "do", "--session"])
            .arg(target.session)
            .args(["--fail-fast", "--args", &args, "-e", include_str!("rex_shell.lua")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let output = child.stdout.take().expect("observer stdout was piped");
        let observer = Self { child, output, frame: Frame::default() };
        // SAFETY: The file descriptor is an open, privately owned pipe read end.
        let nonblocking = unsafe { libc::fcntl(observer.fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        (nonblocking == 0).then_some(observer)
    }

    pub fn fd(&self) -> RawFd {
        self.output.as_raw_fd()
    }

    pub fn read(&mut self) -> ShellChange {
        let mut bytes = [0; 8192];
        match self.output.read(&mut bytes) {
            Ok(0) => ShellChange::Closed,
            Ok(n) => self.frame.read(&bytes[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) => {
                ShellChange::Unchanged
            }
            Err(_) => ShellChange::Closed,
        }
    }
}

impl Drop for ShellObserver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Use real block IDs, never CLI labels, positions or suffixes.
fn exact_block(id: &OsStr) -> Option<&str> {
    let id = id.to_str()?;
    let suffix = id.strip_prefix("block:")?;
    (suffix.len() == 36 && suffix.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')).then_some(id)
}

#[derive(Default)]
struct Frame {
    line: Vec<u8>,
    oversized: bool,
}

impl Frame {
    fn read(&mut self, mut bytes: &[u8]) -> ShellChange {
        const LINE_LIMIT: usize = b"milo-cwd ".len() + 2 * 16384;
        let mut latest = ShellChange::Unchanged;
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|&b| b == b'\n');
            let fragment = &bytes[..newline.unwrap_or(bytes.len())];
            self.oversized |= self.line.len() + fragment.len() > LINE_LIMIT;
            if !self.oversized {
                self.line.extend_from_slice(fragment);
            }
            let Some(end) = newline else { break };
            if !self.oversized
                && let Some(update) = decode_change(&self.line)
            {
                latest = update;
            }
            self.line.clear();
            self.oversized = false;
            bytes = &bytes[end + 1..];
        }
        latest
    }
}

fn decode_change(line: &[u8]) -> Option<ShellChange> {
    let code = line.strip_prefix(b"milo-cwd ")?;
    if code == b"-" {
        return Some(ShellChange::Unreported);
    }
    if !code.len().is_multiple_of(2) || !code.starts_with(b"2f") {
        return None;
    }
    let mut bytes = Vec::with_capacity(code.len() / 2);
    let hex = |b| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    for pair in code.as_chunks::<2>().0 {
        let byte = hex(pair[0])? * 16 + hex(pair[1])?;
        if byte == 0 {
            return None;
        }
        bytes.push(byte);
    }
    Some(ShellChange::Directory(PathBuf::from(OsString::from_vec(bytes))))
}

#[cfg(test)]
mod tests {
    use super::{Frame, ShellChange, json, probe_output, reader_command};
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn diagnostic_processes_have_a_deadline_even_if_they_close_stdout() {
        let mut fast = Command::new("printf");
        fast.arg("catalog");
        assert_eq!(probe_output(&mut fast, Duration::from_secs(5)).unwrap(), b"catalog");
        for script in ["exec sleep 2", "exec >/dev/null; exec sleep 2"] {
            let mut slow = Command::new("sh");
            slow.args(["-c", script]);
            assert!(probe_output(&mut slow, Duration::from_millis(50)).is_none());
        }
    }

    #[test]
    fn strings_are_escaped_for_json() {
        assert_eq!(json(r#"a "b" \c"#), r#""a \"b\" \\c""#);
        assert_eq!(json("tab\there"), "\"tab\\u0009here\"");
    }

    #[test]
    fn shell_frames_keep_paths_verbatim_and_ignore_non_protocol_output() {
        let mut decoder = Frame::default();
        assert!(matches!(decoder.read(b"listening: Rex\nmilo-cwd 2f6"), ShellChange::Unchanged));
        assert!(matches!(
            decoder.read(b"10a62\nunrelated output\n"),
            ShellChange::Directory(path) if path == Path::new("/a\nb"),
        ));
        assert!(matches!(decoder.read(b"milo-cwd -\n"), ShellChange::Unreported));
    }

    #[test]
    fn an_oversized_observer_line_cannot_hide_the_next_valid_frame() {
        let mut decoder = Frame::default();
        let data = [b'x'; 8192];
        for _ in 0..8 {
            assert!(matches!(decoder.read(&data), ShellChange::Unchanged));
        }
        assert!(matches!(decoder.read(b"\nmilo-cwd -\n"), ShellChange::Unreported));
    }

    #[test]
    fn a_floating_reader_passes_the_path_and_commit_as_separate_words() {
        let command = reader_command(Path::new("/bin/milo"), Path::new("-odd \"name"), Some("a123456"));
        assert_eq!(command, r#"["/bin/milo","view","-odd \"name","--commit","a123456"]"#);
    }
}
