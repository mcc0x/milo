//! Asks a Rex server who it is and who is looking at it, and carries the
//! settings editor of another host to this computer through a Rex forward.
//!
//! A *host* is another Rex server, named as `rex -S` names it: by the label
//! `rex hosts` gave it or by an address. Whatever a server answers is
//! untrusted text from another machine, so only whole IDs, numbers and plain
//! version words are kept from it.

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::{PROBE_WAIT, exact_id, probe_output};
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

/// A `rex` that never starts a server. With a host, the session and pane of
/// this terminal are left out: they belong to another server.
fn rex(host: Option<&OsStr>, wait: &str) -> Command {
    let mut command = Command::new("rex");
    command.args(["--autostart=false", "--timeout", wait]).env_remove("REX_CLIENT");
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
    let (wait, deadline) = if host.is_some() { ("10s", HOST_WAIT) } else { ("3s", PROBE_WAIT) };
    parse_server(&probe_output(rex(host, wait).args(["whoami", "--json"]), deadline)?)
}

fn parse_command_version(output: &[u8]) -> Option<CommandVersion> {
    let [version, api] = Value::parse(output)?.fields(["rex_version", "api_revision"]);
    Some(CommandVersion { version: version_word(version?)?, api: api?.unsigned()? })
}

/// The `rex` command on the search path. It asks no server.
pub(crate) fn command_version() -> Option<CommandVersion> {
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
    parse_viewers(&probe_output(rex(None, "3s").args(["client", "ls", "--json"]), PROBE_WAIT)?)
}

/// The session a host runs its settings editor in, and that editor's terminal.
pub(crate) struct RemoteEditor {
    host: OsString,
    session: String,
    block: String,
}

fn parse_created(output: &[u8]) -> Option<(String, String)> {
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
    use super::{CommandVersion, Server, Viewers, parse_command_version, parse_created, parse_server, parse_viewers};

    const HOST: &str = "d4744373-81e8-4cdb-a5be-9a5be80c2c4a";

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
    fn a_new_session_gives_whole_ids_or_nothing() {
        let session = "session:01a1fe07-6593-7bcb-a091-bfac0d945bd5";
        let block = "block:01a1fe07-6593-7d94-b45e-a15c13d6737c";
        let reply = format!(r#"{{"session_id":"{session}","initial_windows":[{{"block_ids":["{block}"]}}]}}"#);
        assert_eq!(parse_created(reply.as_bytes()), Some((session.into(), block.into())));
        assert_eq!(parse_created(reply.replace(block, "block:short").as_bytes()), None);
        assert_eq!(parse_created(reply.replace(session, "--help").as_bytes()), None);
    }
}
