//! Follows one shell onto another computer over SSH, when the person asks for
//! it with `milo ssh HOST`.
//!
//! `milo ssh` runs the person's own `ssh`, with their configuration, keys and
//! prompts, and keeps that one connection open for the tab: the shell's
//! session and the panes' sessions share it, so nothing logs in twice and no
//! key or password leaves this computer. Agent forwarding is turned off for
//! it. Milo installs nothing on the host and changes no SSH or shell setting
//! on either computer.
//!
//! On the host, Milo itself is the helper. The shell's session starts it with
//! a token naming the connection; it notes its own process under that token in
//! its state folder and becomes the person's login shell. The Outline and
//! History beside the shell give their terminal to Milo on the host for as long
//! as the shell is there, and it follows that shell's folder as the host's
//! system reports it. Usage stays on this computer unless the person chooses
//! the host's. When the shell logs out or the connection is lost, the panes take
//! their terminal back, still holding what they held before.
//!
//! When Milo is not on the host, the shell still connects as plain ssh would
//! and the panes say that they cannot follow it.

pub mod helper;
pub mod link;
pub mod process;
pub mod usage;
pub mod watch;

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::{fs_ops, paths, rex, term};
use link::Link;
use process::Process;

pub const HELP: &str = "Usage: milo ssh [-F CONFIG] HOST

Log in to HOST with your own ssh, and let the Outline and History beside this
Rex shell follow it there. They show HOST's files and commits, with HOST's name
on them, as you change folders on HOST, and come back to this computer's when
you log out or the connection is lost. Usage keeps showing this computer's;
press Tab in it to see HOST's. Other tabs are not affected.

HOST needs milo on its search path, in ~/.local/bin or in ~/.cargo/bin. Without
it you are still logged in and the panes say they cannot follow you. Milo
installs nothing on HOST and changes no SSH or shell setting. Panes that follow
HOST only read there: the reader does not edit HOST's files.

-F CONFIG uses that ssh configuration file instead of your own, as ssh -F does.
Agent forwarding is off for this connection. One hop only: a shell that goes
on from HOST to another computer is not followed.";

/// The helper protocol both computers must speak.
pub const PROTOCOL: &str = "1";
const OUTSIDE_REX: &str = "milo ssh follows a shell in a Rex tab; outside Rex, use ssh";
const PROBE_WAIT: Duration = Duration::from_secs(20);
const STOP_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub struct Request<'a> {
    host: &'a str,
    config: Option<&'a OsStr>,
}

impl<'a> Request<'a> {
    pub fn parse(first: Option<&'a OsStr>, rest: &'a [OsString]) -> Result<Self, String> {
        const EXPECTED: &str = "expected milo ssh [-F CONFIG] HOST";
        let words: Vec<&OsStr> = first.into_iter().chain(rest.iter().map(OsString::as_os_str)).collect();
        let (config, host) = match words.as_slice() {
            [flag, config, host] if *flag == "-F" => (Some(*config), *host),
            [host] => (None, *host),
            _ => return Err(EXPECTED.into()),
        };
        let host = host.to_str().filter(|host| link::is_host(host)).ok_or(
            "HOST must be a name or address ssh takes, such as dev@build-box, with no spaces or shell characters",
        )?;
        Ok(Self { host, config })
    }
}

/// How to reach the host again over the connection `milo ssh` holds.
struct Ssh<'a> {
    host: &'a str,
    config: Option<&'a Path>,
    control: &'a Path,
}

impl Ssh<'_> {
    fn base(&self) -> Command {
        let mut command = Command::new("ssh");
        if let Some(config) = self.config {
            command.arg("-F").arg(config);
        }
        command.arg("-S").arg(self.control);
        command
    }

    /// Logs in, asking on this terminal for whatever ssh needs, then holds the
    /// connection with no session of its own.
    fn master(&self) -> Command {
        let mut command = self.base();
        command.args(["-M", "-N", "-n", "-o", "ControlPersist=no", "-o", "ForwardAgent=no", "--", self.host]);
        command.stdin(Stdio::null());
        command
    }

    /// A session over the held connection. Never a second login: with no
    /// connection to share, ssh may not ask anything.
    fn session(&self, terminal: bool, batch: bool, remote: &[&str]) -> Command {
        let mut command = self.base();
        command.args(["-o", "ControlMaster=no"]);
        if batch {
            command.args(["-o", "BatchMode=yes", "-q"]);
        }
        command.arg(if terminal { "-t" } else { "-T" }).args(["--", self.host]).args(remote);
        command
    }

    /// Waits for the login to finish. The master writes its own errors.
    fn ready(&self, master: &mut Child) -> Option<ExitStatus> {
        loop {
            if let Ok(Some(status)) = master.try_wait() {
                return Some(status);
            }
            if term::TERMINATED.load(Ordering::Relaxed) {
                let _ = master.kill();
                return master.wait().ok();
            }
            if fs::symlink_metadata(self.control).is_ok() {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Asks the host where Milo is, in a session with no terminal so that the
    /// answer is plain output and not a screen.
    fn probe(&self) -> Helper {
        const FIND: &str = "sh -c 'for m in \"$(command -v milo 2>/dev/null)\" \"$HOME/.local/bin/milo\" \"$HOME/.cargo/bin/milo\"; do if [ -n \"$m\" ] && [ -x \"$m\" ]; then exec \"$m\" ssh-helper probe; fi; done; echo milo-ssh-helper none'";
        let mut command = self.session(false, true, &[FIND]);
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let Ok(mut child) = fs_ops::spawn(&mut command) else { return Helper::Unknown };
        let until = Instant::now() + PROBE_WAIT;
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(25));
        }
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
            return Helper::Unknown;
        }
        let mut answer = String::new();
        let _ = child.stdout.take().map(|out| out.take(64 << 10).read_to_string(&mut answer));
        Helper::from_answer(&answer)
    }

    fn stop(&self, master: &mut Child) {
        let mut exit = self.base();
        exit.args(["-O", "exit", "--", self.host]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let _ = fs_ops::spawn(&mut exit).and_then(|mut child| child.wait());
        let until = Instant::now() + STOP_WAIT;
        while matches!(master.try_wait(), Ok(None)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = master.kill();
        let _ = master.wait();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Helper {
    Found(String),
    Missing,
    /// Milo answered, but not in a way this version can use.
    Different,
    /// The host did not answer in time.
    Unknown,
}

impl Helper {
    fn from_answer(answer: &str) -> Self {
        let Some(words) = answer.lines().rev().find_map(|line| line.strip_prefix("milo-ssh-helper ")) else {
            return Self::Different;
        };
        match words.split_once(' ') {
            Some((version, path)) if version == PROTOCOL && link::is_helper(path) => Self::Found(path.to_owned()),
            _ if words == "none" => Self::Missing,
            _ => Self::Different,
        }
    }

    fn note(&self, host: &str) -> String {
        match self {
            Self::Found(_) => format!(
                "milo: following this shell on {host}. Outline and History in this tab show {host} until you log out."
            ),
            Self::Missing => format!(
                "milo: Milo isn't on {host}'s search path, in ~/.local/bin or in ~/.cargo/bin, so this tab's panes can't follow you there."
            ),
            Self::Different => format!(
                "milo: Milo on {host} is a different version and can't be followed from here, so this tab's panes stay on this computer."
            ),
            Self::Unknown => {
                format!("milo: {host} did not say whether Milo is there, so this tab's panes stay on this computer.")
            }
        }
    }
}

/// Runs `milo ssh` and returns the exit status of the shell's session.
pub fn connect(request: &Request<'_>) -> Result<i32, String> {
    let target = rex::Target::current().ok_or(OUTSIDE_REX)?;
    let block = rex::exact_id(&target.block, "block:").ok_or(OUTSIDE_REX)?.to_owned();
    let state = paths::state_dir().ok_or("Milo's state folder is unavailable")?;
    let owner = Process::current().ok_or("milo ssh cannot name its own process on this system")?;
    let token = token().map_err(|e| format!("cannot make a connection name: {e}"))?;
    let control =
        Control::create(&token).map_err(|e| format!("cannot make a private folder for the connection: {e}"))?;
    // The person's keys go to the shell and ssh; Milo only notes them and ends cleanly.
    let _ = term::termination();
    let ssh = Ssh { host: request.host, config: request.config.map(Path::new), control: &control.socket };
    let mut master = fs_ops::spawn(&mut ssh.master()).map_err(|e| format!("cannot start ssh: {e}"))?;
    if let Some(status) = ssh.ready(&mut master) {
        return Ok(code(status));
    }
    let helper = ssh.probe();
    eprintln!("{}", helper.note(request.host));
    let helper_path = if let Helper::Found(path) = &helper { Some(path.clone()) } else { None };
    let link = Link {
        owner,
        token: token.clone(),
        host: request.host.to_owned(),
        control: control.socket.clone(),
        config: request.config.map(PathBuf::from),
        helper: helper_path.clone(),
    };
    if link.write(&state, &block).is_err() {
        eprintln!("milo: could not tell this tab's panes about the connection, so they stay on this computer.");
    }
    let remote = helper_path.as_deref().map(|path| ["exec", path, "ssh-helper", "shell", &token]);
    let mut session = ssh.session(true, false, remote.as_ref().map_or(&[], |words| &words[..]));
    let status = fs_ops::spawn(&mut session).and_then(|mut child| child.wait());
    link.remove(&state, &block);
    let lost = matches!(master.try_wait(), Ok(Some(_)));
    ssh.stop(&mut master);
    let status = status.map_err(|e| format!("cannot start ssh: {e}"))?;
    if lost && status.code() == Some(255) {
        eprintln!("milo: lost the connection to {}. This tab's panes are back on this computer.", request.host);
    }
    Ok(code(status))
}

/// What a pane runs to show its counterpart on the host.
pub fn pane_command(link: &Link, role: &str) -> Option<Command> {
    let helper = link.helper.as_deref()?;
    let ssh = Ssh { host: &link.host, config: link.config.as_deref(), control: &link.control };
    Some(ssh.session(true, true, &["exec", helper, "ssh-helper", "pane", role, &link.token, &link.host]))
}

fn code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status.code().or_else(|| status.signal().map(|signal| 128 + signal)).unwrap_or(1)
}

fn token() -> io::Result<String> {
    let mut bytes = [0; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(link::hex(&bytes))
}

/// A private folder for the connection's control socket, removed with it.
/// It lives in the temporary folder because a socket's path must be short.
struct Control {
    dir: PathBuf,
    socket: PathBuf,
}

impl Control {
    fn create(token: &str) -> io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("milo-ssh-{}", &token[..16]));
        // A folder already there is someone else's: never reuse it.
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let socket = dir.join("c");
        Ok(Self { dir, socket })
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_dir(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_request_names_one_host_and_at_most_a_configuration_file() {
        let rest = words(&[]);
        assert_eq!(Request::parse(Some(OsStr::new("dev@ub-1")), &rest), Ok(Request { host: "dev@ub-1", config: None }));
        let rest = words(&["/tmp/ssh config", "ub-1"]);
        assert_eq!(
            Request::parse(Some(OsStr::new("-F")), &rest),
            Ok(Request { host: "ub-1", config: Some(OsStr::new("/tmp/ssh config")) })
        );
        for args in [&[][..], &["-A", "ub-1"], &["ub-1", "ls"], &["-oProxyCommand=sh"], &["ub-1;sh"]] {
            let (first, rest) =
                args.split_first().map_or((None, words(&[])), |(first, rest)| (Some(*first), words(rest)));
            assert!(Request::parse(first.map(OsStr::new), &rest).is_err(), "{args:?}");
        }
    }

    #[test]
    fn the_host_says_where_milo_is_in_one_line_of_its_own() {
        assert_eq!(
            Helper::from_answer("Welcome to Ubuntu\nmilo-ssh-helper 1 /home/dev/.local/bin/milo\n"),
            Helper::Found("/home/dev/.local/bin/milo".into())
        );
        assert_eq!(Helper::from_answer("milo-ssh-helper none\n"), Helper::Missing);
        assert_eq!(Helper::from_answer("milo-ssh-helper 2 /usr/bin/milo\n"), Helper::Different);
        assert_eq!(Helper::from_answer("milo-ssh-helper 1 /home/my files/milo\n"), Helper::Different);
        assert_eq!(Helper::from_answer("error: unknown command\n"), Helper::Different);
    }

    #[test]
    fn every_session_shares_the_held_connection_and_none_forwards_the_agent() {
        let control = Path::new("/tmp/milo-ssh-x/c");
        let ssh = Ssh { host: "ub-1", config: Some(Path::new("/cfg")), control };
        let args = |command: &Command| command.get_args().map(|a| a.to_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(
            args(&ssh.master()),
            [
                "-F", "/cfg", "-S", "/tmp/milo-ssh-x/c", "-M", "-N", "-n", "-o", "ControlPersist=no", "-o",
                "ForwardAgent=no", "--", "ub-1"
            ]
        );
        let link = Link {
            owner: Process::current().unwrap(),
            token: "0".repeat(32),
            host: "ub-1".into(),
            control: control.into(),
            config: None,
            helper: Some("/opt/bin/milo".into()),
        };
        let pane = pane_command(&link, "outline").unwrap();
        assert_eq!(
            args(&pane),
            [
                "-S",
                "/tmp/milo-ssh-x/c",
                "-o",
                "ControlMaster=no",
                "-o",
                "BatchMode=yes",
                "-q",
                "-t",
                "--",
                "ub-1",
                "exec",
                "/opt/bin/milo",
                "ssh-helper",
                "pane",
                "outline",
                &"0".repeat(32),
                "ub-1"
            ]
        );
        assert!(pane_command(&Link { helper: None, ..link }, "outline").is_none());
    }
}
