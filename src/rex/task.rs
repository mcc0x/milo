//! Starts a task's session on a Rex server, and runs the launcher on another host.
//!
//! A session made here is never shown, selected or focused: Rex's `new`
//! creates it on the server and asks no app to display it. Nothing here
//! starts a server, and every word of a command reaches Rex as an argument
//! of its own, so a prompt is never read by a shell.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::server::{parse_created, rex_from};
use super::{finished, probe_output, program};
use crate::json::Value;

/// A host over SSH or a tailnet can take longer to answer than the server of this terminal.
const WAIT: Duration = Duration::from_secs(12);
/// How long a new session's program is watched for an exit that means it never started.
const SETTLE: Duration = Duration::from_millis(1200);
/// How long the launcher on another host may take, most of it Git's checkout.
const CARRY_FOR: &str = "10m";
const CARRY_WAIT: Duration = Duration::from_secs(11 * 60);

fn rex(host: Option<&OsStr>) -> Command {
    rex_from(&program(), host, "10s")
}

/// Whether the Rex server of this terminal, or the one a host names, answers.
pub(crate) fn reachable(host: Option<&OsStr>) -> bool {
    probe_output(rex(host).args(["whoami", "--json"]), WAIT).is_some()
}

/// A session Rex made and the one terminal in it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Started {
    pub session: String,
    block: String,
}

/// Creates a session named `label` whose terminal runs `program` in `dir`,
/// with the environment a shell tab has: the login shell's, with its
/// interactive startup files read, so the agent finds the tools a person's
/// own shell finds. The session is created in the background.
pub(crate) fn start(label: &str, dir: &Path, program: &Path, args: &[OsString]) -> Option<Started> {
    // Rex would read a label that begins with a dash as an option.
    let label = label.trim_start_matches('-');
    let mut command = rex(None);
    command.arg("new").arg(if label.is_empty() { "task" } else { label });
    command.arg("-c").arg(dir).args(["--interactive", "--json", "--"]).arg(program).args(args);
    let (session, block) = parse_created(&probe_output(&mut command, WAIT)?)?;
    Some(Started { session, block })
}

impl Started {
    /// The exit status of the session's program if it ended within moments
    /// of starting, which an agent that opened its screen does not do.
    pub(crate) fn exited_at_once(&self) -> Option<i64> {
        let until = Instant::now() + SETTLE;
        loop {
            let mut command = rex(None);
            command.args(["block", "call", "-s", &self.session, "-b", &self.block, "process"]);
            // A terminal that is gone ended with its program. Rex keeps one that failed at once.
            let exit = probe_output(&mut command, WAIT).map_or(Some(1), |reply| last_exit(&reply));
            if exit.is_some() || Instant::now() >= until {
                return exit;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

/// The status in a terminal's `process` reply, once its program has ended.
fn last_exit(reply: &[u8]) -> Option<i64> {
    let [exit] = Value::parse(reply)?.fields(["last_exit"]);
    let [code] = exit.filter(|exit| !exit.null())?.fields(["exit_code"]);
    Some(code.and_then(Value::integer).unwrap_or(1))
}

/// What the launcher on another host left on its terminal, and whether it succeeded.
pub(crate) struct Carried {
    pub succeeded: bool,
    pub screen: String,
}

/// Runs that host's `milo` with `args` in a session of its own there, waits
/// for it to end, reads its terminal and ends the session. The host finds
/// `milo` on its own search path. `None` when the host could not be asked.
pub(crate) fn carry(host: &OsStr, args: &[OsString]) -> Option<Carried> {
    let mut command = rex(Some(host));
    command.args(["new", "milo-task", "--keep-open", "--json", "--", "milo"]).args(args);
    let (session, block) = parse_created(&probe_output(&mut command, WAIT)?)?;
    let mut wait = rex(Some(host));
    wait.args(["wait", "--for", CARRY_FOR, "-s", &session, &block]);
    let succeeded = finished(wait.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()), CARRY_WAIT);
    let mut capture = rex(Some(host));
    capture.args(["capture", "--trim", "--unwrap", "-s", &session, "-b", &block]);
    let screen = probe_output(&mut capture, WAIT).and_then(|screen| String::from_utf8(screen).ok());
    let _ = probe_output(rex(Some(host)).args(["kill", &session]), WAIT);
    Some(Carried { succeeded, screen: screen? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_running_until_rex_reports_its_exit() {
        let running = br#"{"last_exit":null,"child":{"pid":7,"name":"claude"},"home":"/home/me"}"#;
        assert_eq!(last_exit(running), None);
        assert_eq!(last_exit(br#"{"last_exit":{"exit_code":127,"error":"exit status 127"}}"#), Some(127));
        assert_eq!(last_exit(br#"{"last_exit":{"exit_code":0,"error":""}}"#), Some(0));
        assert_eq!(last_exit(br#"{"last_exit":{"error":"signal: killed"}}"#), Some(1));
        assert_eq!(last_exit(b"not json"), None);
    }
}
