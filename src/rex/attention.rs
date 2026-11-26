//! Rex calls for the menu's Needs you section: the script that says which
//! terminals ask for the person, and the jump that shows one of them.
//!
//! Both speak only to the Rex server on this computer, never start one, and
//! never turn on a Rex setting. Block and session IDs are used whole.

use crate::sys::{AsRawFd as _, RawFd};
use std::ffi::{OsStr, OsString};
use std::io::{self, Read as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use super::{exact_id, finished, probe_output};
use crate::json::Value;

const TERMINAL: &str = "com.superlogical.terminal";
/// A status line is far shorter. A longer line is not one, and is dropped whole.
const LINE_LIMIT: usize = 1 << 16;
/// All of a jump's calls together. Each is also limited by Rex's own `--timeout`.
const JUMP_WAIT: Duration = Duration::from_secs(4);

/// The `rex` to run from the menu app, which starts Milo with a short system
/// path. Rex puts `rex` on the path of its own terminals only, so on a Mac the
/// command Rex installs for its server is used when the path has none.
fn program() -> OsString {
    let on_path =
        std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("rex").is_file()));
    let installed = || {
        let home = crate::paths::home().filter(|home| home.is_absolute() && cfg!(target_os = "macos"))?;
        let rex: PathBuf = home
            .join("Library/Application Support/com.superlogical.rex/RexServer/Versions/Current")
            .join("Rex Server.app/Contents/MacOS/rex");
        rex.is_file().then_some(rex.into_os_string())
    };
    if on_path { "rex".into() } else { installed().unwrap_or_else(|| "rex".into()) }
}

/// A `rex` that never starts a server and belongs to no session, pane or app.
fn rex(wait: &str) -> Command {
    let mut command = Command::new(program());
    command.args(["--autostart=false", "--timeout", wait]);
    for key in ["REX_SESSION", "REX_BLOCK", "REX_CLIENT"] {
        command.env_remove(key);
    }
    command
}

/// A local Rex script that reports each terminal whose program status asks for
/// the person, first all of them and then each change, without periodic calls.
pub(crate) struct AttentionWatch {
    child: Child,
    output: ChildStdout,
    line: Vec<u8>,
    oversized: bool,
}

impl AttentionWatch {
    /// `NotFound` when there is no `rex` command to run.
    pub(crate) fn start() -> io::Result<Self> {
        let mut child = Command::new(program())
            .args(["--autostart=false", "do", "--all", "--fail-fast", "-e", include_str!("../rex_attention.lua")])
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK")
            .env_remove("REX_CLIENT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let output = child.stdout.take().expect("watch stdout was piped");
        let watch = Self { child, output, line: Vec::new(), oversized: false };
        crate::sys::set_nonblocking(watch.fd())?;
        Ok(watch)
    }

    pub(crate) fn fd(&self) -> RawFd {
        self.output.as_raw_fd()
    }

    /// Hands over each whole line that has arrived. False once the script has ended.
    pub(crate) fn read(&mut self, mut line: impl FnMut(&[u8])) -> bool {
        let mut bytes = [0; 8192];
        // A busy script must not keep the caller from its other input.
        for _ in 0..8 {
            let read = match self.output.read(&mut bytes) {
                Ok(0) => return false,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => return false,
            };
            for part in bytes[..read].split_inclusive(|&byte| byte == b'\n') {
                let (text, ended) = part
                    .split_last()
                    .filter(|(last, _)| **last == b'\n')
                    .map_or((part, false), |(_, text)| (text, true));
                self.oversized |= self.line.len() + text.len() > LINE_LIMIT;
                if !self.oversized {
                    self.line.extend_from_slice(text);
                }
                if ended {
                    if !self.oversized {
                        line(&self.line);
                    }
                    self.line.clear();
                    self.oversized = false;
                }
            }
        }
        true
    }
}

impl Drop for AttentionWatch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How an attempt to show a pane ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Jump {
    /// A Rex app on this computer now shows the pane. Its process ID lets the menu bring that app forward.
    Shown(u32),
    /// The pane is focused in its session, but no app was switched to it.
    Focused(&'static str),
    /// Nothing was changed.
    Unavailable(&'static str),
}

const NO_REX: &str = "Rex isn’t answering on this Mac, so the pane can’t be shown.";
const OTHER_SERVER: &str =
    "That pane was reported by a different Rex server than the one running now. Nothing was changed.";
const PANE_GONE: &str = "That pane has closed or is no longer in a tab.";
const NOT_FOCUSED: &str = "Rex didn’t focus that pane.";
const NO_APP: &str = "The pane is focused in its session, but no Rex app on this Mac is connected to show it. \
    Open Rex and pick the session.";
const SEVERAL_APPS: &str = "The pane is focused in its session. Several Rex apps are connected on this Mac, \
    so Milo didn’t pick one to switch: open the session in the one you want.";
const UNSURE_APP: &str = "The pane is focused in its session, but Milo couldn’t tell which Rex app on this Mac to \
    switch, so it switched none. Open the session in Rex.";
const NOT_SWITCHED: &str = "The pane is focused in its session, but Rex didn’t switch to it. Rex takes that request \
    only with Remote Control on (Settings › General › Client Info), and Milo doesn’t change that setting. \
    Open the session in Rex.";

impl Jump {
    /// One word for the menu app, then what to tell the person, if anything.
    pub(crate) fn report(&self) -> String {
        match self {
            Self::Shown(pid) => format!("shown {pid}"),
            Self::Focused(why) => format!("focused\n{why}"),
            Self::Unavailable(why) => format!("unavailable\n{why}"),
        }
    }
}

/// The session and tab that hold `block` now, if it is still a terminal that `host` runs.
fn placed(reply: &[u8], host: &str, block: &str) -> Option<(String, String)> {
    let [session, described] = Value::parse(reply)?.fields(["session_id", "block"]);
    let [id, creator, block_host, placement, window] =
        described?.fields(["block_id", "creator_name", "host_id", "placement", "window_id"]);
    if !id?.is(block) || !creator?.is(TERMINAL) || !block_host?.is(host) || !placement?.is("placed") {
        return None;
    }
    let (session, window) = (session?.string(44)?, window?.string(43)?);
    exact_id(OsStr::new(session.as_ref()), "session:")?;
    exact_id(OsStr::new(window.as_ref()), "window:")?;
    Some((session.into_owned(), window.into_owned()))
}

/// The Rex apps of this person connected over the server's local socket: each one's client ID and process ID.
///
/// Only a client that says it is an app is looked at further. Rex also lists command-line clients,
/// scripts and entries with no description at all, and what those leave out says nothing about an app.
/// An app is counted only when everything that identifies it is there and whole. `None` when an app that
/// may be this person's is listed without that: whether it is the one to switch cannot be told, so none is.
fn local_apps(reply: &[u8], uid: u64) -> Option<Vec<(String, u32)>> {
    let [clients] = Value::parse(reply)?.fields(["clients"]);
    let mut apps = Vec::new();
    for client in clients?.elements()? {
        let [id, info, principal, state] = client.fields(["client_id", "info", "principal", "connection_state"]);
        let [kind] = info.map_or([None], |info| info.fields(["kind"]));
        if !kind.is_some_and(|kind| kind.is("app")) {
            continue;
        }
        // An app whose connection dropped may come back, but it can show nothing now.
        let state = state?;
        if state.is("lost") {
            continue;
        }
        let [peer, transport, owner, pid] = principal?.fields(["kind", "transport", "uid", "pid"]);
        // An app that reached the server another way runs on another computer.
        if !peer?.is("peer") || !transport?.is("unix") {
            continue;
        }
        if owner?.unsigned()? != uid {
            continue;
        }
        if !state.is("connected") {
            return None;
        }
        let id = id?.string(43)?;
        exact_id(OsStr::new(id.as_ref()), "client:")?;
        apps.push((id.into_owned(), u32::try_from(pid?.unsigned()?).ok()?));
    }
    Some(apps)
}

/// Shows the terminal `block` in the Rex app on this computer, if `host` is still the server here
/// and the block is still one of its placed terminals. The pane is found by its whole ID wherever
/// it is now; nothing is guessed from a label, and a server on another host is never asked.
pub(crate) fn jump(host: &str, block: &str) -> Jump {
    if exact_id(OsStr::new(host), "").is_none() || exact_id(OsStr::new(block), "block:").is_none() {
        return Jump::Unavailable(PANE_GONE);
    }
    let deadline = Instant::now() + JUMP_WAIT;
    let ask = |args: &[&str]| probe_output(rex("1s").args(args), deadline.checked_duration_since(Instant::now())?);
    // SAFETY: Getting the current effective user ID touches no memory.
    #[cfg(unix)]
    let uid = u64::from(unsafe { libc::geteuid() });
    // Rex does not run on Windows, and no peer's Unix user ID can match this.
    #[cfg(windows)]
    let uid = u64::MAX;
    let Some(who) = ask(&["whoami", "--json"]) else { return Jump::Unavailable(NO_REX) };
    let [principal, server] = Value::parse(&who).map_or([None; 2], |who| who.fields(["principal", "host_id"]));
    let [kind, transport] = principal.map_or([None; 2], |principal| principal.fields(["kind", "transport"]));
    let local = kind.is_some_and(|kind| kind.is("peer")) && transport.is_some_and(|transport| transport.is("unix"));
    if !local || !server.is_some_and(|server| server.is(host)) {
        return Jump::Unavailable(OTHER_SERVER);
    }
    let found = ask(&["block", "inspect", block, "--json"]).and_then(|reply| placed(&reply, host, block));
    let Some((session, window)) = found else { return Jump::Unavailable(PANE_GONE) };
    let focus = format!(r#"{{"block_id":"{block}"}}"#);
    if ask(&["api", "call", "-s", &session, "session.focus_block", &focus]).is_none() {
        return Jump::Unavailable(NOT_FOCUSED);
    }
    let Some(apps) = ask(&["client", "ls", "--json"]).and_then(|reply| local_apps(&reply, uid)) else {
        return Jump::Focused(UNSURE_APP);
    };
    let [(client, pid)] = &apps[..] else {
        return Jump::Focused(if apps.is_empty() { NO_APP } else { SEVERAL_APPS });
    };
    let Some(left) = deadline.checked_duration_since(Instant::now()) else { return Jump::Focused(NOT_SWITCHED) };
    let (session, window) = (format!("session_id={session}"), format!("window_id={window}"));
    let mut select = rex("2s");
    select.args(["-C", client, "do", "session.select", &session, &window]);
    select.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if finished(&mut select, left) { Jump::Shown(*pid) } else { Jump::Focused(NOT_SWITCHED) }
}

#[cfg(test)]
mod tests {
    use super::{local_apps, placed};

    const HOST: &str = "27ea019d-95c6-4fba-ab3c-d19b89cbb459";
    const BLOCK: &str = "block:01a21295-d49e-7bcb-a89c-ef73e104fd7e";
    const SESSION: &str = "session:01a21295-d49e-78d9-9143-37837c6d246c";
    const WINDOW: &str = "window:01a21295-d4b1-789b-b97a-a06ba3837cbf";

    fn inspected() -> String {
        format!(
            r#"{{"session_id":"{SESSION}","block":{{"block_id":"{BLOCK}","label":"shell",
                "creator_name":"com.superlogical.terminal","host_id":"{HOST}","placement":"placed","window_id":"{WINDOW}"}}}}"#
        )
    }

    #[test]
    fn a_jump_needs_the_same_terminal_on_the_same_host_still_in_a_tab() {
        let current = inspected();
        assert_eq!(placed(current.as_bytes(), HOST, BLOCK), Some((SESSION.into(), WINDOW.into())));
        for changed in [
            current.replace(HOST, "27ea019d-95c6-4fba-ab3c-d19b89cbb450"),
            current.replace(BLOCK, "block:01a21295-d49e-7bcb-a89c-ef73e104fd70"),
            current.replace("com.superlogical.terminal", "com.example.plugin"),
            current.replace(r#""placed""#, r#""detached""#),
            current.replace(SESSION, "agent-one"),
            current.replace(WINDOW, "2"),
            "{}".into(),
        ] {
            assert_eq!(placed(changed.as_bytes(), HOST, BLOCK), None, "{changed}");
        }
    }

    fn client(id: u8, info: &str, principal: &str, state: &str) -> String {
        format!(
            r#"{{"client_id":"client:01a21295-d097-70de-b037-f56cd986d6{id:02}",{info}
                "principal":{principal},"connection_state":"{state}"}}"#
        )
    }

    const UNIX: &str = r#"{"kind":"peer","transport":"unix","uid":501,"pid":4242}"#;
    const APP: &str = r#""info":{"kind":"app","platform":"macos","name":"Rex"},"#;

    fn listed(clients: &[String]) -> Option<Vec<(String, u32)>> {
        local_apps(format!(r#"{{"clients":[{}]}}"#, clients.join(",")).as_bytes(), 501)
    }

    fn app(id: u8) -> (String, u32) {
        (format!("client:01a21295-d097-70de-b037-f56cd986d6{id:02}"), 4242)
    }

    #[test]
    fn only_this_persons_connected_apps_on_the_local_socket_can_be_switched() {
        let found = listed(&[
            client(1, r#""info":{"kind":"cli"},"#, UNIX, "connected"),
            client(2, APP, r#"{"kind":"tailscale","transport":"tailscale"}"#, "connected"),
            client(3, APP, r#"{"kind":"peer","transport":"unix","uid":0,"pid":7}"#, "connected"),
            client(4, APP, UNIX, "lost"),
            client(5, APP, UNIX, "connected"),
        ]);
        assert_eq!(found, Some(vec![app(5)]));
        assert_eq!(listed(&[client(5, APP, UNIX, "connected"), client(6, APP, UNIX, "connected")]).unwrap().len(), 2);
        assert_eq!(local_apps(br#"{"clients":[]}"#, 501), Some(Vec::new()));
        assert_eq!(local_apps(b"not json", 501), None);
    }

    #[test]
    fn a_client_that_does_not_say_it_is_an_app_cannot_hide_the_app_listed_after_it() {
        // As a running server listed them: an earlier local client of the same person with no
        // description at all, then Rex for Mac.
        let found = listed(&[client(1, "", UNIX, "connected"), client(5, APP, UNIX, "connected")]);
        assert_eq!(found, Some(vec![app(5)]));
        // Whatever else such an entry leaves out or garbles, it is still not an app.
        let bare = r#"{"client_id":7}"#.to_owned();
        let no_kind = client(2, r#""info":{"name":"rex"},"#, "null", "odd");
        let odd_kind = client(3, r#""info":{"kind":["app"]},"#, UNIX, "connected");
        assert_eq!(listed(&[bare, no_kind, odd_kind, client(5, APP, UNIX, "connected")]), Some(vec![app(5)]));
        // And it is never counted as an app itself, however local it is.
        assert_eq!(listed(&[client(1, "", UNIX, "connected")]), Some(Vec::new()));
    }

    #[test]
    fn an_app_that_cannot_be_told_apart_is_not_passed_over_for_another() {
        let whole = client(5, APP, UNIX, "connected");
        let no_pid = r#"{"kind":"peer","transport":"unix","uid":501}"#;
        let no_uid = r#"{"kind":"peer","transport":"unix","pid":4242}"#;
        let no_transport = r#"{"kind":"peer","uid":501,"pid":4242}"#;
        for unsure in [
            client(6, APP, UNIX, "reconnecting"),
            client(6, APP, no_pid, "connected"),
            client(6, APP, no_uid, "connected"),
            client(6, APP, no_transport, "connected"),
            client(6, APP, "null", "connected"),
            client(6, APP, UNIX, "connected").replace("client:01a21295", "01a21295"),
            client(6, APP, UNIX, "connected").replace(r#","connection_state":"connected""#, ""),
        ] {
            assert_eq!(listed(&[whole.clone(), unsure.clone()]), None, "{unsure}");
            assert_eq!(listed(&[unsure.clone(), whole.clone()]), None, "{unsure}");
        }
    }
}
