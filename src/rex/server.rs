//! Asks a Rex server who it is and who is looking at it, and carries the
//! settings editor of another host to this computer through a Rex forward.
//!
//! A *host* is another Rex server, named as `rex -S` names it: by the label
//! `rex hosts` gave it or by an address. Whatever a server answers is
//! untrusted text from another machine, so only whole IDs, numbers and plain
//! version words are kept from it.

use std::ffi::{OsStr, OsString};
use std::io;
#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::{ASKS_REX, PROBE_WAIT, exact_id, probe_output};
use crate::json::Value;

/// A host over SSH or a tailnet can take longer to answer than the server of this terminal.
const HOST_WAIT: Duration = Duration::from_secs(12);

/// What a Rex server says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Server {
    pub version: String,
    /// The revision of the control API the server speaks.
    pub api: u64,
    /// Rex's own name for the machine and account the server keeps its files on.
    pub host_id: String,
}

/// The installed `rex` command, which can be older or newer than the server it talks to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandVersion {
    pub version: String,
    pub api: u64,
}

/// Where the Rex apps showing this server run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Viewers {
    /// At least one app runs on this computer.
    Here,
    /// Apps are connected, all of them from another computer or a phone.
    Elsewhere,
    /// No app is connected.
    Nobody,
}

fn rex_command(exe: &OsStr, wait: &str) -> Command {
    let mut command = Command::new(exe);
    command.args(["--autostart=false", "--timeout", wait]).env_remove("REX_CLIENT");
    command
}

/// A `rex` that never starts a server. With a host, the session and pane of
/// this terminal are left out: they belong to another server.
fn rex(host: Option<&OsStr>, wait: &str) -> Command {
    rex_from(OsStr::new("rex"), host, wait)
}

/// As `rex`, for a caller that has found which `rex` program to run.
pub(super) fn rex_from(exe: &OsStr, host: Option<&OsStr>, wait: &str) -> Command {
    let mut command = rex_command(exe, wait);
    if let Some(host) = host {
        command.arg("-S").arg(host).env_remove("REX_SESSION").env_remove("REX_BLOCK").env_remove("REX_SERVER");
    }
    command
}

/// A version as Rex writes one, such as `0.1.0-preview.21`, and nothing a terminal would act on.
fn version_word(value: Value<'_>) -> Option<String> {
    let word = value.string(64)?;
    let plain = !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'));
    plain.then(|| word.into_owned())
}

fn parse_server(output: &[u8]) -> Option<Server> {
    let [version, api, host_id] = Value::parse(output)?.fields(["rex_version", "api_revision", "host_id"]);
    let host_id = host_id?.string(36)?;
    let whole = host_id.len() == 36 && host_id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    Some(Server {
        version: version_word(version?)?,
        api: api?.unsigned()?,
        host_id: whole.then(|| host_id.into_owned())?,
    })
}

/// The server of this terminal, or the one a host names. `None` when it does not answer.
pub(crate) fn server(host: Option<&OsStr>) -> Option<Server> {
    if !ASKS_REX {
        return None;
    }
    let (wait, deadline) = if host.is_some() { ("10s", HOST_WAIT) } else { ("3s", PROBE_WAIT) };
    parse_server(&probe_output(rex(host, wait).args(["whoami", "--json"]), deadline)?)
}

fn parse_command_version(output: &[u8]) -> Option<CommandVersion> {
    let [version, api] = Value::parse(output)?.fields(["rex_version", "api_revision"]);
    Some(CommandVersion { version: version_word(version?)?, api: api?.unsigned()? })
}

/// The `rex` command on the search path. It asks no server.
pub(crate) fn command_version() -> Option<CommandVersion> {
    if !ASKS_REX {
        return None;
    }
    parse_command_version(&probe_output(Command::new("rex").args(["version", "--json"]), PROBE_WAIT)?)
}

fn parse_viewers(output: &[u8]) -> Option<Viewers> {
    let [clients] = Value::parse(output)?.fields(["clients"]);
    let mut viewers = Viewers::Nobody;
    for client in clients?.elements()? {
        let [info, principal, connection] = client.fields(["info", "principal", "connection_state"]);
        let [kind] = info?.fields(["kind"]);
        if !kind.is_some_and(|kind| kind.is("app")) || connection.is_some_and(|state| state.is("lost")) {
            continue;
        }
        let [transport] = principal?.fields(["transport"]);
        if transport.is_some_and(|transport| transport.is("unix")) {
            return Some(Viewers::Here);
        }
        viewers = Viewers::Elsewhere;
    }
    Some(viewers)
}

/// Whether a Rex app on this computer is connected to the server of this terminal.
pub(crate) fn viewers() -> Option<Viewers> {
    if !ASKS_REX {
        return None;
    }
    parse_viewers(&probe_output(rex(None, "3s").args(["client", "ls", "--json"]), PROBE_WAIT)?)
}

#[cfg(any(target_os = "macos", test))]
const NATIVE_UNKNOWN: &str = "Could not confirm a local Rex app for this pane";
#[cfg(any(target_os = "macos", test))]
const NATIVE_REMOTE: &str = "Native opening is unavailable from remote Rex";

#[cfg(any(target_os = "macos", test))]
enum NativeConnection {
    Here,
    Elsewhere,
}

#[cfg(any(target_os = "macos", test))]
fn native_connection(principal: Value<'_>, uid: u64) -> Option<NativeConnection> {
    let [kind, transport, uid_field] = principal.fields(["kind", "transport", "uid"]);
    let (kind, transport) = (kind?, transport?);
    if kind.is("peer") && transport.is("unix") {
        (uid_field?.unsigned()? == uid).then_some(NativeConnection::Here)
    } else if (kind.is("anonymous") || kind.is("tailscale")) && (transport.is("tcp") || transport.is("tailscale"))
        || kind.is("windows_peer") && transport.is("named_pipe")
    {
        Some(NativeConnection::Elsewhere)
    } else {
        None
    }
}

/// The session the local server says holds the caller's block. A session ID the
/// terminal inherited from an older Rex must be that one.
#[cfg(any(target_os = "macos", test))]
fn native_identity(output: &[u8], session: Option<&str>, block: &str, uid: u64) -> Result<String, &'static str> {
    let [principal, endpoint, session_id, block_id] =
        Value::parse(output).ok_or(NATIVE_UNKNOWN)?.fields(["principal", "endpoint", "session_id", "block_id"]);
    match native_connection(principal.ok_or(NATIVE_UNKNOWN)?, uid).ok_or(NATIVE_UNKNOWN)? {
        NativeConnection::Elsewhere => return Err(NATIVE_REMOTE),
        NativeConnection::Here => {}
    }
    let endpoint = endpoint.and_then(|value| value.string(4096)).ok_or(NATIVE_UNKNOWN)?;
    let unix_socket = endpoint
        .strip_prefix("unix:///")
        .is_some_and(|path| !path.is_empty() && !path.bytes().any(|b| b.is_ascii_control()));
    let found = session_id.and_then(|id| id.string(44)).ok_or(NATIVE_UNKNOWN)?;
    let whole = exact_id(OsStr::new(&*found), "session:").is_some();
    if unix_socket && whole && session.is_none_or(|id| id == found) && block_id.is_some_and(|id| id.is(block)) {
        Ok(found.into_owned())
    } else {
        Err(NATIVE_UNKNOWN)
    }
}

/// The scope in the reply must agree with the session we sent on the command line.
#[cfg(any(target_os = "macos", test))]
fn native_attachment(sessions: Value<'_>, session: &str) -> Option<bool> {
    let mut attached = false;
    for id in sessions.elements()? {
        if id.is(session) {
            attached = true;
        } else {
            let id = id.string(44)?;
            exact_id(OsStr::new(&*id), "session:")?;
        }
    }
    Some(attached)
}

/// A complete answer is required even when the first client is local.
#[cfg(any(target_os = "macos", test))]
fn native_clients(output: &[u8], session: &str, uid: u64) -> Option<Result<(), &'static str>> {
    let [clients] = Value::parse(output)?.fields(["clients"]);
    let (mut here, mut elsewhere) = (0, false);
    for client in clients?.elements()? {
        let [info, principal, state, sessions] =
            client.fields(["info", "principal", "connection_state", "session_ids"]);
        let state = state?;
        if state.is("lost") {
            continue;
        }
        if !state.is("connected") {
            return None;
        }
        let [kind] = info?.fields(["kind"]);
        let kind = kind?;
        if kind.is("cli") || kind.is("agent") || kind.is("other") {
            continue;
        }
        if !kind.is("app") || !native_attachment(sessions?, session)? {
            return None;
        }
        match native_connection(principal?, uid)? {
            NativeConnection::Here => here += 1,
            NativeConnection::Elsewhere => elsewhere = true,
        }
    }
    Some(match (here, elsewhere) {
        (1, false) => Ok(()),
        (0, false) => Err("No Rex app is connected to this session"),
        (0, true) => Err(NATIVE_REMOTE),
        _ => Err("Native opening needs exactly one local Rex app connected to this session"),
    })
}

/// Prefer the executable the kernel reports for a direct Rex parent. Otherwise
/// use an absolute PATH entry, never a launcher or executable in the project.
#[cfg(any(target_os = "macos", test))]
fn native_program(root: &Path, cwd: &Path, parent: Option<&Path>, path: &OsStr) -> Option<PathBuf> {
    crate::native::trusted::find("rex", Some(root), cwd, parent, path)
}

/// Check the same session and pane the local CLI sees, then count the apps
/// attached to that session. The web editor's server-wide viewer hint is not an authorization check.
#[cfg(target_os = "macos")]
pub(crate) fn native_app(root: &Path, session: Option<&OsStr>, block: &OsStr) -> Result<(), &'static str> {
    use std::time::Instant;

    let session = session.map(|id| exact_id(id, "session:").ok_or(NATIVE_UNKNOWN)).transpose()?;
    let block = exact_id(block, "block:").ok_or(NATIVE_UNKNOWN)?;
    let parent = super::parent_program();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let exe = native_program(root, Path::new("."), parent.as_deref(), &path).ok_or(NATIVE_UNKNOWN)?;
    // Both probes run in the input loop, so they share one short deadline.
    let deadline = Instant::now() + Duration::from_millis(600);
    let probe = |args: &[&str]| {
        let left = deadline.checked_duration_since(Instant::now())?;
        probe_output(rex_command(exe.as_os_str(), "500ms").args(args), left)
    };
    // SAFETY: Getting the current effective user ID touches no memory.
    let uid = u64::from(unsafe { libc::geteuid() });
    let who = probe(&["whoami", "--json"]).ok_or(NATIVE_UNKNOWN)?;
    let session = native_identity(&who, session, block, uid)?;
    let clients = probe(&["client", "ls", "-s", &session, "--json"]).ok_or(NATIVE_UNKNOWN)?;
    native_clients(&clients, &session, uid).ok_or(NATIVE_UNKNOWN)?
}

/// The session a host runs its settings editor in, and that editor's terminal.
pub(crate) struct RemoteEditor {
    host: OsString,
    session: String,
    block: String,
}

pub(super) fn parse_created(output: &[u8]) -> Option<(String, String)> {
    let [session, windows] = Value::parse(output)?.fields(["session_id", "initial_windows"]);
    let [blocks] = windows?.elements()?.next()?.fields(["block_ids"]);
    let session = session?.string(64)?;
    let block = blocks?.elements()?.next()?.string(64)?;
    exact_id(OsStr::new(&*session), "session:")?;
    exact_id(OsStr::new(&*block), "block:")?;
    Some((session.into_owned(), block.into_owned()))
}

impl RemoteEditor {
    /// Starts `milo web` on the host in a session of its own, told which
    /// address on this computer its page will be opened at. `project` is a
    /// folder on the host. The host finds `milo` on its own search path.
    pub(crate) fn start(host: &OsStr, project: Option<&OsStr>, forwarded_port: u16) -> Option<Self> {
        if !ASKS_REX {
            return None;
        }
        let mut command = rex(Some(host), "10s");
        command.args(["new", "milo-settings", "--json"]);
        if let Some(project) = project {
            command.arg("-c").arg(project);
        }
        command.args(["--", "milo", "web", "--forwarded-port"]).arg(forwarded_port.to_string());
        let (session, block) = parse_created(&probe_output(&mut command, HOST_WAIT)?)?;
        Some(Self { host: host.to_owned(), session, block })
    }

    fn rex(&self, wait: &str) -> Command {
        rex(Some(&self.host), wait)
    }

    /// What the editor's terminal shows. The editor prints its private link
    /// there, as `milo web` does in any terminal.
    pub(crate) fn screen(&self) -> Option<String> {
        let mut command = self.rex("10s");
        command.args(["capture", "--trim", "--unwrap", "-s", &self.session, "-b", &self.block]);
        String::from_utf8(probe_output(&mut command, HOST_WAIT)?).ok()
    }

    /// Listens on this computer's loopback address only and carries each
    /// connection to the editor's port on the host's loopback address. The
    /// forward lives in the editor's session, so ending that session ends it.
    pub(crate) fn forward(&self, local_port: u16, host_port: u16) -> io::Result<Child> {
        self.rex("10s")
            .args(["forward", "open", "--no-open", "--bind", "127.0.0.1", "-s", &self.session, "-p"])
            .arg(local_port.to_string())
            .arg(format!("127.0.0.1:{host_port}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    }

    /// A process that ends when the editor on the host does.
    pub(crate) fn watch(&self) -> io::Result<Child> {
        self.rex("10s")
            .args(["wait", "-s", &self.session, &self.block])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    /// Ends the editor's session on the host: the editor, and the forward kept in it.
    pub(crate) fn end(&self) -> bool {
        let mut command = self.rex("10s");
        command.args(["kill", &self.session]);
        probe_output(&mut command, HOST_WAIT).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommandVersion, NATIVE_REMOTE, NATIVE_UNKNOWN, Server, Viewers, native_clients, native_identity,
        parse_command_version, parse_created, parse_server, parse_viewers,
    };
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    #[cfg(unix)]
    use std::path::Path;

    #[cfg(unix)]
    use super::native_program;
    #[cfg(unix)]
    use crate::fixture::Fixture;

    const HOST: &str = "d4744373-81e8-4cdb-a5be-9a5be80c2c4a";
    const SESSION: &str = "session:01a1fe07-6593-7bcb-a091-bfac0d945bd5";
    const BLOCK: &str = "block:01a1fe07-6593-7d94-b45e-a15c13d6737c";
    const PEER: &str = r#"{"kind":"peer","transport":"unix","uid":501}"#;
    const REMOTE: &str = r#"{"kind":"tailscale","transport":"tailscale"}"#;

    #[cfg(unix)]
    #[test]
    fn native_probe_skips_checkout_paths_links_and_non_executables() {
        let fixture = Fixture::folder();
        let root = &fixture.root;
        let installed = Fixture::folder();
        let binary = |name: &str, mode| {
            installed.write(name, "");
            let binary = installed.root.join(name);
            fs::set_permissions(&binary, fs::Permissions::from_mode(mode)).unwrap();
            binary
        };
        let non_executable = binary("non-executable/rex", 0o644);
        let executable = binary("installed/rex", 0o755);
        let parent = binary("parent/rex", 0o755);
        fixture.write("bin/rex", "");
        let checkout = root.join("bin/rex");
        fs::set_permissions(&checkout, fs::Permissions::from_mode(0o755)).unwrap();
        let inside_link = root.join("link");
        fs::create_dir(&inside_link).unwrap();
        symlink(&executable, inside_link.join("rex")).unwrap();
        let outside_link = installed.root.join("checkout-link");
        fs::create_dir(&outside_link).unwrap();
        symlink(&checkout, outside_link.join("rex")).unwrap();
        let alias = installed.root.join("checkout-dir");
        symlink(&inside_link, &alias).unwrap();
        let cwd = installed.root.join("invocation-directory");
        let cwd_bin = binary("invocation-directory/bin/rex", 0o755);
        let cwd_alias = installed.root.join("cwd-alias");
        symlink(&cwd, &cwd_alias).unwrap();
        let cwd_link = installed.root.join("cwd-link");
        fs::create_dir(&cwd_link).unwrap();
        symlink(&cwd_bin, cwd_link.join("rex")).unwrap();
        let case_alias = root.with_file_name("PROJECT").join("bin");
        let bad_path = [
            Path::new("."),
            checkout.parent().unwrap(),
            &inside_link,
            &outside_link,
            &alias,
            cwd_bin.parent().unwrap(),
            &cwd_alias.join("bin"),
            &cwd_link,
            &case_alias,
            non_executable.parent().unwrap(),
        ];
        let excluded = std::env::join_paths(bad_path).unwrap();
        assert_eq!(native_program(root, &cwd, Some(&checkout), &excluded), None);
        assert_eq!(native_program(root, &cwd, Some(&cwd_bin), &excluded), None);
        let path = std::env::join_paths(bad_path.into_iter().chain(executable.parent())).unwrap();
        assert_eq!(native_program(root, &cwd, None, &path), Some(executable));
        assert_eq!(native_program(root, &cwd, Some(&parent), &path), Some(parent));
    }

    #[test]
    fn a_server_is_known_by_its_whole_host_id_and_a_plain_version() {
        let reply = format!(r#"{{"host_id":"{HOST}","rex_version":"0.1.0-preview.21","api_revision":18}}"#);
        let server = Server { version: "0.1.0-preview.21".into(), api: 18, host_id: HOST.into() };
        assert_eq!(parse_server(reply.as_bytes()), Some(server));
        for broken in [
            reply.replace("0.1.0-preview.21", "0.1\\u001b[31m"),
            reply.replace(HOST, "d4744373"),
            reply.replace("18", "\"18\""),
            reply.replace("\"host_id\"", "\"other\""),
        ] {
            assert_eq!(parse_server(broken.as_bytes()), None, "{broken}");
        }
    }

    #[test]
    fn the_command_reports_its_own_version_and_api() {
        let reply = br#"{"rex_version":"0.1.0-preview.21","platform":"darwin_arm64","api_revision":18}"#;
        assert_eq!(parse_command_version(reply), Some(CommandVersion { version: "0.1.0-preview.21".into(), api: 18 }));
        assert_eq!(parse_command_version(br#"{"rex_version":"0.1.0"}"#), None);
    }

    #[test]
    fn only_a_connected_app_on_this_computer_counts_as_here() {
        let client = |kind: &str, transport: &str, state: &str| {
            format!(
                r#"{{"info":{{"kind":"{kind}"}},"principal":{{"transport":"{transport}"}},"connection_state":"{state}"}}"#
            )
        };
        let list = |clients: &[String]| format!(r#"{{"clients":[{}]}}"#, clients.join(","));
        let viewers = |clients: &[String]| parse_viewers(list(clients).as_bytes());
        assert_eq!(viewers(&[]), Some(Viewers::Nobody));
        assert_eq!(viewers(&[client("cli", "unix", "connected")]), Some(Viewers::Nobody));
        assert_eq!(viewers(&[client("app", "tailscale", "connected")]), Some(Viewers::Elsewhere));
        assert_eq!(viewers(&[client("app", "unix", "lost")]), Some(Viewers::Nobody));
        let both = [client("app", "ssh", "connected"), client("app", "unix", "connected")];
        assert_eq!(viewers(&both), Some(Viewers::Here));
        assert_eq!(parse_viewers(br#"{"clients":{}}"#), None);
    }

    #[test]
    fn native_identity_requires_a_local_peer_and_the_exact_pane() {
        let reply = format!(
            r#"{{"principal":{PEER},"endpoint":"unix:///tmp/rex.sock","session_id":"{SESSION}","block_id":"{BLOCK}"}}"#
        );
        let identity = |reply: &str| native_identity(reply.as_bytes(), Some(SESSION), BLOCK, 501);
        assert_eq!(identity(&reply).as_deref(), Ok(SESSION));
        // A pane named by its block alone learns its session from the same reply.
        assert_eq!(native_identity(reply.as_bytes(), None, BLOCK, 501).as_deref(), Ok(SESSION));
        for partial in ["session:5bd5", "milo", BLOCK] {
            let reply = reply.replace(SESSION, partial);
            assert_eq!(native_identity(reply.as_bytes(), None, BLOCK, 501), Err(NATIVE_UNKNOWN), "{partial}");
        }
        let other = reply.replace(SESSION, "session:01a1fe07-6593-7bcb-a091-bfac0d945bd6");
        assert_eq!(identity(&other), Err(NATIVE_UNKNOWN));
        let remote = reply.replace(PEER, REMOTE);
        assert_eq!(identity(&remote), Err(NATIVE_REMOTE));
        for bad in [
            reply.replace(SESSION, "session:5bd5"),
            reply.replace(BLOCK, "block:737c"),
            reply.replace("block_id", "window_id"),
            reply.replace("unix:///tmp/rex.sock", "tcp://127.0.0.1:2411"),
            reply.replace("501", "502"),
            reply.replace("501", "\"501\""),
            reply.replace("\"peer\"", "null"),
        ] {
            assert_eq!(identity(&bad), Err(NATIVE_UNKNOWN), "{bad}");
        }
    }

    fn app(principal: &str, state: &str, session: &str) -> String {
        format!(
            r#"{{"info":{{"kind":"app"}},"principal":{principal},"connection_state":"{state}","session_ids":["{session}"]}}"#
        )
    }

    fn clients(clients: &[&str]) -> Option<Result<(), &'static str>> {
        native_clients(format!(r#"{{"clients":[{}]}}"#, clients.join(",")).as_bytes(), SESSION, 501)
    }

    #[test]
    fn native_actions_need_one_same_user_app_in_this_session() {
        let local = app(PEER, "connected", SESSION);
        let remote = app(REMOTE, "connected", SESSION);
        let lost = app(REMOTE, "lost", SESSION);
        let cli = local.replace(r#""app""#, r#""cli""#);
        let multiple = Some(Err("Native opening needs exactly one local Rex app connected to this session"));
        let nobody = Some(Err("No Rex app is connected to this session"));
        assert_eq!(clients(&[]), nobody);
        assert_eq!(clients(&[&lost, &cli]), nobody);
        assert_eq!(clients(&[&local, &lost, &cli]), Some(Ok(())));
        let other_session = "session:01a1fe07-6593-7bcb-a091-bfac0d945bd6";
        let attached_twice = local.replace(SESSION, &format!(r#"{SESSION}","{other_session}"#));
        assert_eq!(clients(&[&attached_twice]), Some(Ok(())));
        assert_eq!(clients(&[&remote]), Some(Err(NATIVE_REMOTE)));
        assert_eq!(clients(&[&local, &local]), multiple);
        assert_eq!(clients(&[&local, &remote]), multiple);
        assert_eq!(clients(&[&remote, &local]), multiple);
    }

    #[test]
    fn a_malformed_native_client_reply_cannot_authorize_even_with_a_good_local_app() {
        let local = app(PEER, "connected", SESSION);
        let wrong_session = "session:01a1fe07-6593-7bcb-a091-bfac0d945bd6";
        for bad in [
            app(PEER, "connected", wrong_session),
            app(PEER, "unknown", SESSION),
            app(&PEER.replace("501", "502"), "connected", SESSION),
            app(r#"{"kind":"peer","transport":"unix"}"#, "connected", SESSION),
            app(r#"{"kind":"future","transport":"future"}"#, "connected", SESSION),
            local.replace(r#""connection_state":"connected","#, ""),
            local.replace("[\"", "[42,\""),
            "null".into(),
        ] {
            assert_eq!(clients(&[&local, &bad]), None, "{bad}");
        }
        assert_eq!(native_clients(br#"{"clients":{}}"#, SESSION, 501), None);
    }

    #[test]
    fn a_new_session_gives_whole_ids_or_nothing() {
        let session = "session:01a1fe07-6593-7bcb-a091-bfac0d945bd5";
        let block = "block:01a1fe07-6593-7d94-b45e-a15c13d6737c";
        let reply = format!(r#"{{"session_id":"{session}","initial_windows":[{{"block_ids":["{block}"]}}]}}"#);
        assert_eq!(parse_created(reply.as_bytes()), Some((session.into(), block.into())));
        assert_eq!(parse_created(reply.replace(block, "block:short").as_bytes()), None);
        assert_eq!(parse_created(reply.replace(session, "--help").as_bytes()), None);
    }
}
