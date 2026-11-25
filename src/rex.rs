//! Calls Rex for floating readers, Markdown previews, setup, and the new-tab listener.

mod attention;
mod preview;
mod server;
pub(crate) mod task;

pub(crate) use attention::{AttentionWatch, jump};
pub(crate) use preview::{available as preview_available, launch as preview, preview_anchor};

#[cfg(target_os = "macos")]
pub(crate) use server::native_app;
pub(crate) use server::{CommandVersion, RemoteEditor, Server, Viewers, command_version, server, viewers};

use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal as _, Read as _, Write as _};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::{fs_ops, term};

const PROBE_WAIT: Duration = Duration::from_secs(3);
/// How long a script or a settings command may take before it is ended. Rex allows each
/// request three seconds; a script makes several.
const CALL_WAIT: Duration = Duration::from_secs(15);
/// How long the thread writing a script may take to report, once the program has ended.
const PIPE_GRACE: Duration = Duration::from_millis(200);

/// Inspect the server catalog, not the foreground app. Doctor must not start a server.
pub(crate) fn layout_available() -> Option<bool> {
    let mut command = Command::new("rex");
    command.args(["--autostart=false", "--timeout", "3s", "actions", "--json"]).env_remove("REX_CLIENT");
    let output = probe_output(&mut command, PROBE_WAIT)?;
    layout_catalog(&output)
}

fn layout_catalog(output: &[u8]) -> Option<bool> {
    const ACTIONS: [&str; 11] = [
        "milo_layout", "milo_outline", "milo_history", "milo_usage", "milo_close", "milo_peek_outline",
        "milo_close_peek", "milo_remember_layout", "milo_reset_layout", "milo_name_session", "milo_open_output",
    ];
    let [actions] = crate::json::Value::parse(output)?.fields(["actions"]);
    let mut missing = (1u16 << ACTIONS.len()) - 1;
    for action in actions?.elements()? {
        if let [Some(name)] = action.fields(["name"])
            && let Some(index) = ACTIONS.iter().position(|action| name.is(action))
        {
            missing &= !(1 << index);
            if missing == 0 {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// The pane that ran Milo, even if focus has moved somewhere else.
pub struct Target {
    /// The session ID an older Rex put in the environment. Rex preview.23 and later name
    /// only the block, and its session is whichever one holds that block now.
    pub session: Option<OsString>,
    pub block: OsString,
}

impl Target {
    pub fn current() -> Option<Self> {
        Self::inherited(inherited_id("REX_SESSION"), inherited_id("REX_BLOCK"), term::host(), detached())
    }

    /// A terminal Rex drew vouches for the IDs it was given. A caller with no terminal, or
    /// one named by its block alone, must carry whole IDs: a label or the end of an ID could
    /// select some other session.
    fn inherited(session: Option<OsString>, block: Option<OsString>, host: term::Host, detached: bool) -> Option<Self> {
        let block = block?;
        let whole = exact_block(&block).is_some()
            && session.as_ref().is_none_or(|session| exact_id(session, "session:").is_some());
        let known = match host {
            term::Host::Rex => session.is_some() || whole,
            term::Host::Unknown if detached => whole,
            _ => false,
        };
        known.then_some(Self { session, block })
    }

    /// What `-s` takes for this pane's session. Rex accepts a whole block ID there and
    /// uses the session that holds the block.
    pub fn selector(&self) -> &OsStr {
        self.session.as_deref().unwrap_or(&self.block)
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
    command.args(args);
    scripted(&mut command, lua, CALL_WAIT)
}

fn scripted(command: &mut Command, lua: &str, wait: Duration) -> Option<String> {
    let mut text = String::from_utf8(output_within(command, Some(lua), wait)?).ok()?;
    text.truncate(text.trim_end().len());
    Some(text)
}

/// Doctor's read-only script has a local deadline as well as Rex's request
/// timeout. Passing Lua as an argument keeps stdin from blocking the probe.
pub(crate) fn diagnostic_script(target: &Target, lua: &str, caller: &str) -> Option<String> {
    let mut command = Command::new("rex");
    command
        .args(["--autostart=false", "--timeout", "3s", "do", "-e", lua, "-s"])
        .arg(target.selector())
        .arg("-b")
        .arg(&target.block)
        .args(["milo_operation=doctor", caller]);
    let mut text = String::from_utf8(probe_output(&mut command, PROBE_WAIT)?).ok()?;
    text.truncate(text.trim_end().len());
    Some(text)
}

fn probe_output(command: &mut Command, wait: Duration) -> Option<Vec<u8>> {
    output_within(command, None, wait)
}

/// What a program printed, if it succeeded within `wait` and within a mebibyte, having read
/// all of `input`. Otherwise it is ended and waited for.
fn output_within(command: &mut Command, input: Option<&str>, wait: Duration) -> Option<Vec<u8>> {
    const LIMIT: usize = 1 << 20;
    let deadline = Instant::now() + wait;
    let stdin = if input.is_some() { Stdio::piped() } else { Stdio::null() };
    let mut child = fs_ops::spawn(command.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::null())).ok()?;
    // Written from a thread that can be left behind: a program that never reads, or one that
    // hands the pipe to another, must not hold a pane past the deadline.
    let written = input.zip(child.stdin.take()).map(|(input, mut pipe)| {
        let (input, (send, written)) = (input.to_owned(), mpsc::channel());
        std::thread::spawn(move || send.send(pipe.write_all(input.as_bytes()).is_ok()));
        written
    });
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
        // Its end of the pipe closed with it, so the writer has an answer by now.
        Some((status, output)) => (status.success()
            && written.is_none_or(|written| written.recv_timeout(PIPE_GRACE) == Ok(true)))
        .then_some(output),
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
    command.stdout(Stdio::null()).stderr(Stdio::null());
    if let [config, check, path] = args
        && *config == "config"
        && *check == "check"
    {
        // The staging descriptor is close-on-exec. Pass it as Rex's stdin
        // rather than leaking it (or a named settings file) across exec.
        let Ok(input) = std::fs::File::open(path) else { return false };
        command.args([*config, *check, OsStr::new("/dev/stdin")]).stdin(input).env("MILO_CHECKING_REX", "1");
    } else {
        command.args(args).stdin(Stdio::null());
    }
    finished(&mut command, CALL_WAIT)
}

/// Whether a program succeeded within `wait`. One that has not ended by then is ended.
fn finished(command: &mut Command, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    let Ok(mut child) = fs_ops::spawn(command) else { return false };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
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

/// The local endpoint and host of one alert subscription. A socket replaced
/// after Rex restarts must not resolve labels for events from the old stream.
pub(crate) struct AlertServer {
    endpoint: String,
    socket: (u64, u64),
    pub(crate) host: String,
}

impl AlertServer {
    fn local(selected: &str) -> Option<Self> {
        let socket = if selected == "localhost" { None } else { Some(socket_identity(selected)?) };
        let reply = probe_output(
            alert_command(Some(selected)).args(["--timeout", "400ms", "whoami", "--json"]),
            Duration::from_millis(500),
        )?;
        let [principal, endpoint, host] =
            crate::json::Value::parse(&reply)?.fields(["principal", "endpoint", "host_id"]);
        let [kind, transport] = principal?.fields(["kind", "transport"]);
        if !kind?.is("peer") || !transport?.is("unix") {
            return None;
        }
        let endpoint = endpoint?.string(4096)?;
        let identity = socket_identity(&endpoint)?;
        if selected != "localhost" && (socket_path(&endpoint) != socket_path(selected) || socket != Some(identity)) {
            return None;
        }
        let host = host?.string(36)?;
        uuid(&host).then(|| Self { endpoint: endpoint.into_owned(), socket: identity, host: host.into_owned() })
    }

    pub(crate) fn session(&self, id: &str) -> Option<Vec<u8>> {
        exact_id(OsStr::new(id), "session:")?;
        self.is_current().then_some(())?;
        let reply = probe_output(
            alert_command(Some(&self.endpoint)).args(["--timeout", "400ms", "session", "inspect", id, "--json"]),
            Duration::from_millis(500),
        )?;
        self.is_current().then_some(reply)
    }

    pub(crate) fn is_current(&self) -> bool {
        socket_identity(&self.endpoint) == Some(self.socket)
    }
}

fn socket_path(endpoint: &str) -> Option<&Path> {
    let path = endpoint.strip_prefix("unix://").unwrap_or(endpoint);
    (path.starts_with('/') && !path.bytes().any(|byte| byte.is_ascii_control())).then(|| Path::new(path))
}

fn socket_identity(endpoint: &str) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(socket_path(endpoint)?).ok()?;
    meta.file_type().is_socket().then(|| (meta.dev(), meta.ino()))
}

fn alert_command(endpoint: Option<&str>) -> Command {
    let mut command = Command::new("rex");
    command.arg("--autostart=false");
    if let Some(endpoint) = endpoint {
        command.args(["-S", endpoint]);
        for key in ["REX_SERVER", "REX_SESSION", "REX_BLOCK", "REX_CLIENT"] {
            command.env_remove(key);
        }
    }
    command
}

/// One subscription attempt, with the same endpoint for its health probe,
/// event stream and optional verified label lookup.
pub(crate) enum AlertSource {
    Inherited,
    Local(String),
    Pinned(AlertServer),
}

impl AlertSource {
    pub(crate) fn select(label: bool) -> Self {
        let selected = std::env::var_os("REX_SERVER").unwrap_or_default();
        let local = match selected.to_str() {
            Some("" | "localhost") => Some("localhost"),
            Some(endpoint) if socket_path(endpoint).is_some() => Some(endpoint),
            _ => None,
        };
        if label && let Some(server) = local.and_then(AlertServer::local) {
            return Self::Pinned(server);
        }
        match local {
            Some(endpoint) if endpoint != "localhost" => Self::Local(endpoint.to_owned()),
            _ => Self::Inherited,
        }
    }

    pub(crate) fn origin(&self) -> Option<&AlertServer> {
        match self {
            Self::Pinned(server) => Some(server),
            _ => None,
        }
    }

    /// Another socket already owns the pinned address. Probe the successor
    /// before declaring an outage just because the old pin is stale.
    pub(crate) fn replaced(&self) -> bool {
        self.origin()
            .is_some_and(|server| socket_identity(&server.endpoint).is_some_and(|current| current != server.socket))
    }

    fn endpoint(&self) -> Option<&str> {
        match self {
            Self::Inherited => None,
            Self::Local(endpoint) => Some(endpoint),
            Self::Pinned(server) => Some(&server.endpoint),
        }
    }

    fn is_current(&self) -> bool {
        match self {
            Self::Inherited => true,
            Self::Local(endpoint) => socket_identity(endpoint).is_some(),
            Self::Pinned(server) => server.is_current(),
        }
    }

    /// A separate bounded request distinguishes an idle stream from Rex
    /// being down. This never starts Rex or uses a delivery credential.
    pub(crate) fn available(&self) -> bool {
        self.is_current()
            && finished(
                alert_command(self.endpoint())
                    .args(["--timeout=2s", "ls", "--json"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
                ALERTS_CONNECT,
            )
            && self.is_current()
    }

    /// Starts the event stream without starting Rex. An explicitly selected
    /// local socket must not fall back to another server when it disappears.
    pub(crate) fn events(&self) -> io::Result<Child> {
        if !self.is_current() {
            return Err(io::ErrorKind::NotConnected.into());
        }
        fs_ops::spawn(
            alert_command(self.endpoint())
                .args(["--timeout=2s", "events", "--all", "--json"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
    }
}

/// `events` applies this timeout to starting delivery, not to a live stream.
pub(crate) const ALERTS_CONNECT: Duration = Duration::from_millis(2500);

/// A search path on which a program started from here finds `rex`, when
/// this one's does not. The Rex server starts programs with a short system
/// path that leaves `rex` out; its own program's folder is added to it.
pub(crate) fn path_with_rex() -> Option<OsString> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&path).any(|dir| dir.join("rex").is_file()) {
        return None;
    }
    let parent = parent_program().filter(|parent| parent.file_name() == Some(OsStr::new("rex")))?;
    std::env::join_paths(std::env::split_paths(&path).chain([parent.parent()?.to_owned()])).ok()
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
pub(crate) fn json(s: &str) -> String {
    json_chars(s.chars(), s.len())
}

fn json_chars(chars: impl Iterator<Item = char>, bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 2);
    out.push('"');
    for c in chars {
        match c {
            '"' | '\\' => out.extend(['\\', c]),
            c if c.is_control() => {
                use std::fmt::Write as _;
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn inherited_id(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|id| !id.is_empty())
}

fn detached() -> bool {
    !io::stdin().is_terminal() && !io::stdout().is_terminal()
}

/// Interactive panes need Rex's terminal identity as well as inherited IDs.
/// Detached callers can carry a complete session or block ID without terminal metadata.
pub fn inside() -> bool {
    inside_with(inherited_id("REX_SESSION"), inherited_id("REX_BLOCK"), term::host(), detached())
}

fn inside_with(session: Option<OsString>, block: Option<OsString>, host: term::Host, detached: bool) -> bool {
    // Rex preview.23 and later name only the block, which then has to be enough for a target.
    let Some(session) = session else { return Target::inherited(None, block, host, detached).is_some() };
    match host {
        term::Host::Rex => block.is_some() || detached,
        term::Host::Unknown if detached => exact_id(&session, "session:").is_some(),
        _ => false,
    }
}

/// Whether Rex put this terminal's session in the environment, as releases before
/// preview.23 did. Later ones name only its block.
pub fn names_session() -> bool {
    inherited_id("REX_SESSION").is_some()
}

/// Opens a floating reader in the caller's tab and returns its block ID.
/// `file` is what the reader is given: a path, or with `commit` the name Git has for the file.
/// The old reader stays in place if the replacement cannot be created.
pub fn open(root: &Path, file: &Path, commit: Option<&str>, previous: Option<&str>) -> Result<String, &'static str> {
    let target = Target::current().ok_or("Reading a file needs a Rex pane.")?;
    let exe = std::env::current_exe().map_err(|_| "Rex did not open the file.")?;
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let origin = preview::anchor_argument().unwrap_or_default();
    float(&target, root, &name, &reader_command(&exe, file, commit), previous, Some(origin))
        .ok_or("Rex did not open the file.")
}

/// Opens a floating review of the patch in `patch` in the caller's tab.
/// `from` is shown as the name of whoever handed the patch over.
pub fn open_review(target: &Target, root: &Path, patch: &Path, from: Option<&str>) -> Result<(), &'static str> {
    const FAILED: &str = "Rex did not open the review. Run milo review PATCH without --float in a terminal.";
    let exe = std::env::current_exe().map_err(|_| FAILED)?;
    float(target, root, "Patch review", &review_command(&exe, patch, from), None, None).map(drop).ok_or(FAILED)
}

/// Runs `command`, a JSON list of words, in a new floating pane over the
/// tab of `target`, and returns the pane's block ID.
fn float(
    target: &Target,
    root: &Path,
    label: &str,
    command: &str,
    previous: Option<&str>,
    preview_origin: Option<&str>,
) -> Option<String> {
    let previous = previous.map(json).unwrap_or_else(|| "null".into());
    let mut args = format!(
        r#"{{"label":{},"cwd":{},"command":{command},"previous":{previous}"#,
        reader_label(label),
        json(&root.to_string_lossy()),
    );
    if let Some(origin) = preview_origin {
        args.push_str(r#","preview_origin":"#);
        args.push_str(&json(origin));
    }
    args.push('}');
    script(Some(target), target.selector(), include_str!("rex_reader.lua"), ["--args", &args])
        .filter(|reply| exact_block(OsStr::new(reply)).is_some())
}

/// Brings the floating reader in `block` to the front, named after the file it now shows.
pub fn focus(block: &str, file: &Path) -> bool {
    let Some(target) = Target::current() else { return false };
    let args = format!(r#"{{"block":{},"label":{}}}"#, json(block), file_label(file));
    script(Some(&target), target.selector(), include_str!("rex_reader_show.lua"), ["--args", &args]).is_some()
}

/// The label of a reader showing `file`: the file's name.
fn file_label(file: &Path) -> String {
    reader_label(&file.file_name().unwrap_or_default().to_string_lossy())
}

/// Quote a Rex label without changing the path passed to the reader.
fn reader_label(name: &str) -> String {
    const LIMIT: usize = 128;
    // Limit UTF-8 bytes without cutting a character in half.
    let mut chars = name
        .chars()
        .filter(|c| !c.is_control())
        .scan(LIMIT, |left, c| {
            *left = left.checked_sub(c.len_utf8())?;
            Some(c)
        })
        .peekable();
    if chars.peek().is_none() {
        // Let Rex use the block flavor's default label.
        return "null".into();
    }
    json_chars(chars, name.len().min(LIMIT))
}

fn reader_command(exe: &Path, file: &Path, commit: Option<&str>) -> String {
    let args = commit.map_or_else(String::new, |hash| format!(r#","--commit",{}"#, json(hash)));
    format!("[{},\"view\",{}{args}]", json(&exe.to_string_lossy()), json(&file.to_string_lossy()))
}

fn review_command(exe: &Path, patch: &Path, from: Option<&str>) -> String {
    let args = from.map_or_else(String::new, |name| format!(r#","--from",{}"#, json(name)));
    format!("[{},\"review\",{}{args}]", json(&exe.to_string_lossy()), json(&patch.to_string_lossy()))
}

/// A local Rex script watches the selected shell without periodic CLI calls.
pub struct ShellObserver {
    child: Child,
    output: ChildStdout,
    frame: Frame<ShellChange>,
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
            .arg(target.selector())
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
        self.frame.drain(&mut self.output)
    }
}

impl Drop for ShellObserver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the presence helper learns about Rex: whether a Rex app has a
/// session open and, only when asked for, what is in front.
#[derive(Debug, PartialEq, Eq)]
pub enum AppActivity {
    Unchanged,
    Active(Front),
    Idle,
    /// The script ended, as it does when the Rex server stops.
    Closed,
}

/// The one session that a Rex app on this computer is showing. Both parts
/// are unknown unless the script was asked for them and could tell.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Front {
    /// The session's label, as Rex holds it.
    pub session: Option<String>,
    /// Where the program in that session's focused pane is working.
    pub directory: Option<PathBuf>,
}

/// Which parts of `Front` the script may ask Rex about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrontParts {
    pub session: bool,
    pub directory: bool,
}

/// A local Rex script that says when the first Rex app attaches to a
/// session and when the last one leaves, without periodic CLI calls.
pub struct AppObserver {
    child: Child,
    output: ChildStdout,
    frame: Frame<AppActivity>,
}

impl AppObserver {
    /// `rex` is the program to run, since the Rex server's own path leaves it out.
    /// With no `parts`, the script asks Rex for nothing but its connected apps.
    pub fn start(rex: &OsStr, parts: FrontParts) -> Option<Self> {
        let args = format!(r#"{{"session":{},"directory":{}}}"#, parts.session, parts.directory);
        let mut child = Command::new(rex)
            .args(["--autostart=false", "do", "--all", "--fail-fast", "--args", &args, "-e"])
            .arg(include_str!("rex_presence.lua"))
            // The script hears every session, whichever pane or app started the helper.
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK")
            .env_remove("REX_CLIENT")
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

    pub fn read(&mut self) -> AppActivity {
        self.frame.drain(&mut self.output)
    }
}

impl Drop for AppObserver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The `rex` to run from a process that Rex may have started with a path
/// that leaves `rex` out: the one on the path, or else that Rex itself.
pub fn program() -> OsString {
    let on_path =
        std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("rex").is_file()));
    match parent_program() {
        Some(parent) if !on_path && parent.file_name() == Some(OsStr::new("rex")) => parent.into_os_string(),
        _ => "rex".into(),
    }
}

/// Use real block IDs, never CLI labels, positions or suffixes.
fn exact_block(id: &OsStr) -> Option<&str> {
    exact_id(id, "block:")
}

/// A whole Rex ID of the given kind, such as `window:` followed by its UUID.
pub(crate) fn exact_id<'a>(id: &'a OsStr, kind: &str) -> Option<&'a str> {
    let id = id.to_str()?;
    uuid(id.strip_prefix(kind)?).then_some(id)
}

fn uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Whether this program runs in the Rex terminal that `block` names in full.
pub(crate) fn runs_in(block: &str) -> bool {
    term::host() == term::Host::Rex && exact_block(OsStr::new(block)).is_some()
}

/// What an observer script prints: an update a line, of which the latest counts.
trait Report: Sized {
    const UNCHANGED: Self;
    const CLOSED: Self;
    fn decode(line: &[u8]) -> Option<Self>;
    fn unchanged(&self) -> bool;
}

impl Report for ShellChange {
    const UNCHANGED: Self = Self::Unchanged;
    const CLOSED: Self = Self::Closed;

    fn decode(line: &[u8]) -> Option<Self> {
        decode_change(line)
    }

    fn unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

impl Report for AppActivity {
    const UNCHANGED: Self = Self::Unchanged;
    const CLOSED: Self = Self::Closed;

    fn decode(line: &[u8]) -> Option<Self> {
        let report = line.strip_prefix(b"milo-rex ")?;
        if report == b"idle" {
            return Some(Self::Idle);
        }
        let mut words = report.split(|&b| b == b' ');
        if words.next()? != b"active" {
            return None;
        }
        // A name or folder the script left out, or wrote in a way that cannot be read, is not known.
        let mut field = || words.next().filter(|word| *word != b"-").and_then(unhex);
        let session = field().and_then(|label| String::from_utf8(label).ok());
        let directory = field().map(|path| PathBuf::from(OsString::from_vec(path))).filter(|path| path.is_absolute());
        Some(Self::Active(Front { session, directory }))
    }

    fn unchanged(&self) -> bool {
        *self == Self::Unchanged
    }
}

struct Frame<T> {
    line: Vec<u8>,
    oversized: bool,
    report: std::marker::PhantomData<T>,
}

impl<T> Default for Frame<T> {
    fn default() -> Self {
        Self { line: Vec::new(), oversized: false, report: std::marker::PhantomData }
    }
}

impl<T: Report> Frame<T> {
    fn drain(&mut self, input: &mut impl io::Read) -> T {
        let mut latest = T::UNCHANGED;
        let mut bytes = [0; 8192];
        // Coalesce queued updates without letting a busy producer starve input.
        for _ in 0..8 {
            match input.read(&mut bytes) {
                Ok(0) if latest.unchanged() => return T::CLOSED,
                Ok(0) => break,
                Ok(n) => {
                    let update = self.read(&bytes[..n]);
                    if !update.unchanged() {
                        latest = update;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => return T::CLOSED,
            }
        }
        latest
    }

    fn read(&mut self, mut bytes: &[u8]) -> T {
        const LINE_LIMIT: usize = b"milo-cwd ".len() + 2 * 16384;
        let mut latest = T::UNCHANGED;
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|&b| b == b'\n');
            let fragment = &bytes[..newline.unwrap_or(bytes.len())];
            self.oversized |= self.line.len() + fragment.len() > LINE_LIMIT;
            if !self.oversized {
                self.line.extend_from_slice(fragment);
            }
            let Some(end) = newline else { break };
            if !self.oversized
                && let Some(update) = T::decode(&self.line)
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
    if !code.starts_with(b"2f") {
        return None;
    }
    Some(ShellChange::Directory(PathBuf::from(OsString::from_vec(unhex(code)?))))
}

/// The bytes a script wrote as lowercase hex, which may hold anything but a NUL.
fn unhex(code: &[u8]) -> Option<Vec<u8>> {
    if !code.len().is_multiple_of(2) {
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
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        Frame, ShellChange, Target, exact_block, finished, inside_with, json, layout_catalog, probe_output,
        reader_command, reader_label, review_command, scripted,
    };
    use crate::term::Host;
    use std::ffi::OsStr;
    use std::io::Cursor;
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn doctor_requires_the_current_milo_action_catalog() {
        let released = r#"{"actions":[
            {"name":"milo_layout"},{"name":"milo_outline"},{"name":"milo_history"},{"name":"milo_usage"},
            {"name":"milo_close"},{"name":"milo_peek_outline"},{"name":"milo_close_peek"},
            {"name":"milo_remember_layout"},{"name":"milo_reset_layout"},{"name":"milo_name_session"},
            {"name":"an_unrelated_action"}]}"#;
        assert_eq!(layout_catalog(released.as_bytes()), Some(false));
        let current = released.replace("an_unrelated_action", "milo_open_output");
        assert_eq!(layout_catalog(current.as_bytes()), Some(true));
        let partial = current.replace("milo_close_peek", "milo_close");
        assert_eq!(layout_catalog(partial.as_bytes()), Some(false));
        assert_eq!(layout_catalog(br#"{"actions":{}}"#), None);
    }

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
    fn a_rex_that_does_not_answer_is_ended_at_the_deadline() {
        let stand_in = |script: &str| {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            command
        };
        let long = Duration::from_secs(5);
        assert_eq!(scripted(&mut stand_in("cat"), "return 1\n", long).as_deref(), Some("return 1"));
        assert!(finished(&mut stand_in("exit 0"), long) && !finished(&mut stand_in("exit 3"), long));

        let began = Instant::now();
        let short = Duration::from_millis(100);
        // More than a pipe holds, for one that never reads its script and one that never answers it.
        let script = "-".repeat(1 << 20);
        for stalled in ["exec sleep 3", "cat >/dev/null; exec sleep 3"] {
            assert!(scripted(&mut stand_in(stalled), &script, short).is_none());
        }
        assert!(!finished(&mut stand_in("exec sleep 3"), short));
        // One that prints without end is ended at a mebibyte, well before the deadline.
        assert!(scripted(&mut stand_in("exec yes"), "", long).is_none());
        assert!(began.elapsed() < Duration::from_secs(2), "waited {:?}", began.elapsed());
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
    fn queued_shell_frames_across_reads_only_apply_the_latest_directory() {
        let mut decoder = Frame::default();
        let mut bytes = b"milo-cwd 2f61\n".to_vec();
        bytes.resize(8190, b'x');
        bytes.extend_from_slice(b"\nmilo-cwd 2f62\n");
        let mut input = Cursor::new(bytes);
        assert!(matches!(decoder.drain(&mut input), ShellChange::Directory(path) if path == Path::new("/b")));
        assert!(matches!(decoder.drain(&mut input), ShellChange::Closed));
    }

    #[test]
    fn rex_block_replies_must_contain_whole_uuids() {
        let parse = |id| exact_block(OsStr::new(id)).is_some();
        assert!(parse("block:01a1f929-dc65-7cc5-9ba2-cac3108697bf"));
        assert!(!parse("block:01a1f929dc65-7-cc5-9ba2-cac3108697bf"));
        assert!(!parse("block:------------------------------------"));
    }

    const SESSION: &str = "session:01a1f929-dc65-7cc5-9ba2-cac3108697b0";
    const BLOCK: &str = "block:01a1f929-dc65-7cc5-9ba2-cac3108697bf";

    fn target(session: Option<&str>, block: Option<&str>, host: Host, detached: bool) -> Option<Target> {
        Target::inherited(session.map(Into::into), block.map(Into::into), host, detached)
    }

    #[test]
    fn a_pane_named_only_by_its_block_selects_the_session_that_holds_it() {
        // Rex preview.23 and later: an interactive pane, and an agent's shell with no terminal.
        for (host, detached) in [(Host::Rex, false), (Host::Rex, true), (Host::Unknown, true)] {
            let target = target(None, Some(BLOCK), host, detached).unwrap();
            assert!(target.session.is_none());
            assert_eq!((target.selector(), target.block.as_os_str()), (OsStr::new(BLOCK), OsStr::new(BLOCK)));
            assert!(inside_with(None, Some(BLOCK.into()), host, detached));
        }
        // A label or the end of an ID is no session selector, wherever it came from.
        for partial in ["shell", "block:97bf", "8697bf", SESSION] {
            for (host, detached) in [(Host::Rex, false), (Host::Rex, true), (Host::Unknown, true)] {
                assert!(target(None, Some(partial), host, detached).is_none(), "{partial}");
                assert!(!inside_with(None, Some(partial.into()), host, detached), "{partial}");
            }
        }
    }

    #[test]
    fn a_pane_an_older_rex_named_keeps_its_inherited_session() {
        let named = target(Some(SESSION), Some(BLOCK), Host::Rex, false).unwrap();
        assert_eq!((named.selector(), named.block.as_os_str()), (OsStr::new(SESSION), OsStr::new(BLOCK)));
        // A Rex terminal vouches for what it was given, whole IDs or not.
        assert_eq!(target(Some("test"), Some("test"), Host::Rex, false).unwrap().selector(), "test");
        assert!(inside_with(Some("test".into()), Some("test".into()), Host::Rex, false));
        assert!(inside_with(Some("test".into()), None, Host::Rex, true));
        assert!(!inside_with(Some("test".into()), None, Host::Rex, false));
        // Without a terminal the IDs must be whole.
        assert!(target(Some(SESSION), Some(BLOCK), Host::Unknown, true).is_some());
        assert!(target(Some("test"), Some(BLOCK), Host::Unknown, true).is_none());
        assert!(target(Some(SESSION), Some("shell"), Host::Unknown, true).is_none());
        assert!(inside_with(Some(SESSION.into()), None, Host::Unknown, true));
        assert!(!inside_with(Some("test".into()), Some(BLOCK.into()), Host::Unknown, true));
        // A session alone names no pane to act on.
        assert!(target(Some(SESSION), None, Host::Rex, false).is_none());
    }

    #[test]
    fn rex_ids_left_in_another_terminal_or_outside_rex_are_not_a_target() {
        for session in [None, Some(SESSION)] {
            for host in [Host::Graphics, Host::Multiplexer, Host::Other] {
                for detached in [false, true] {
                    assert!(target(session, Some(BLOCK), host, detached).is_none());
                    assert!(!inside_with(session.map(Into::into), Some(BLOCK.into()), host, detached));
                }
            }
            assert!(target(session, Some(BLOCK), Host::Unknown, false).is_none());
            assert!(!inside_with(session.map(Into::into), Some(BLOCK.into()), Host::Unknown, false));
        }
        for (host, detached) in [(Host::Rex, false), (Host::Rex, true), (Host::Unknown, true)] {
            assert!(target(None, None, host, detached).is_none());
            assert!(!inside_with(None, None, host, detached));
        }
    }

    #[test]
    fn an_app_report_names_what_is_in_front_only_in_whole_readable_fields() {
        use super::{AppActivity, Front};
        let mut decoder = Frame::<AppActivity>::default();
        let front = |session: Option<&str>, directory: Option<&str>| {
            AppActivity::Active(Front { session: session.map(str::to_owned), directory: directory.map(Into::into) })
        };
        assert_eq!(decoder.read(b"milo-rex idle\n"), AppActivity::Idle);
        assert_eq!(decoder.read(b"milo-rex active\n"), front(None, None));
        assert_eq!(decoder.read(b"milo-rex active - -\n"), front(None, None));
        // "a b" and "/p q", with the spaces inside the fields and not between them.
        assert_eq!(decoder.read(b"milo-rex active 612062 2f702071\n"), front(Some("a b"), Some("/p q")));
        assert_eq!(decoder.read(b"milo-rex active - 2f70\n"), front(None, Some("/p")));
        // A label that is not text, a folder that is not a whole path, and a broken field are all unknown.
        assert_eq!(decoder.read(b"milo-rex active ff 70\n"), front(None, None));
        assert_eq!(decoder.read(b"milo-rex active 6g 2f7\n"), front(None, None));
        // Anything else the script or Rex prints is not a report.
        assert_eq!(decoder.read(b"listening: 8 handlers\nmilo-rex busy\n"), AppActivity::Unchanged);
    }

    #[test]
    fn floating_reader_labels_remove_controls_and_fit_at_utf8_boundaries() {
        assert_eq!(reader_label("ordinary.rs"), json("ordinary.rs"));
        assert_eq!(reader_label("a\tb\n\u{85}.rs"), json("ab.rs"));
        assert_eq!(reader_label("\t\n"), "null");
        let full = "x".repeat(128);
        assert_eq!(reader_label(&format!("{full}z")), json(&full));
        let prefix = "x".repeat(127);
        assert_eq!(reader_label(&format!("{prefix}é")), json(&prefix));
        let prefix = format!("{}é", "x".repeat(126));
        assert_eq!(reader_label(&format!("{prefix}z")), json(&prefix));
    }

    #[test]
    fn a_floating_reader_passes_the_path_and_commit_as_separate_words() {
        let command = reader_command(Path::new("/bin/milo"), Path::new("-odd \"name\t\n"), Some("a123456"));
        assert_eq!(command, r#"["/bin/milo","view","-odd \"name\u0009\u000a","--commit","a123456"]"#);
    }

    #[test]
    fn a_floating_review_passes_the_patch_and_name_as_separate_words() {
        let command = review_command(Path::new("/bin/milo"), Path::new("/tmp/--float \"x\".patch"), Some("--float"));
        assert_eq!(command, r#"["/bin/milo","review","/tmp/--float \"x\".patch","--from","--float"]"#);
        let unnamed = review_command(Path::new("/bin/milo"), Path::new("/tmp/a.patch"), None);
        assert_eq!(unnamed, r#"["/bin/milo","review","/tmp/a.patch"]"#);
    }
}
