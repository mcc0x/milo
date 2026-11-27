//! Milo's side on the host a shell reached with `milo ssh`.
//!
//! `probe` says where this Milo is. `shell` notes its own process under the
//! connection's token and then becomes the person's login shell, so the noted
//! process is the shell itself. `pane` shows the Outline, History or Usage
//! of this host in the terminal a pane on the other computer lent it, and
//! follows the noted shell's folder as this system reports it.

use std::fs;
use std::io::{self, Write as _};
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

use super::link::{is_helper, is_host, is_token};
use super::process::Process;
use super::{PROTOCOL, usage};
use crate::git::Repo;
use crate::pane::Pane;
use crate::preferences::Preferences;
use crate::{fs_ops, paths, workflow};

/// How long a pane waits for the shell of its connection to start.
const SHELL_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// What a pane on this host is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Outline,
    History,
    Usage,
}

impl Role {
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "outline" => Self::Outline,
            "history" => Self::History,
            "usage" => Self::Usage,
            _ => return None,
        })
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Outline => "outline",
            Self::History => "history",
            Self::Usage => "usage",
        }
    }
}

/// The internal command a session on this host runs.
#[derive(Debug, PartialEq, Eq)]
pub enum Request<'a> {
    Probe,
    Shell(&'a str),
    Pane { role: Role, token: &'a str, host: &'a str },
}

impl<'a> Request<'a> {
    pub fn parse(words: &[&'a str]) -> Option<Self> {
        Some(match *words {
            ["probe"] => Self::Probe,
            ["shell", token] if is_token(token) => Self::Shell(token),
            ["pane", role, token, host] if is_token(token) && is_host(host) => {
                Self::Pane { role: Role::parse(role)?, token, host }
            }
            _ => return None,
        })
    }
}

pub fn probe() -> i32 {
    let path = std::env::current_exe().and_then(fs::canonicalize).ok();
    let path = path.as_deref().and_then(Path::to_str).filter(|path| is_helper(path)).unwrap_or("-");
    let line = format!("milo-ssh-helper {PROTOCOL} {path}\n");
    i32::from(io::stdout().lock().write_all(line.as_bytes()).is_err())
}

/// Notes this process under `token`, then runs the login shell in its place.
/// A note that cannot be made costs following, never the shell.
pub fn shell(token: &str) -> i32 {
    if let (Some(state), Some(me)) = (paths::state_dir(), Process::current()) {
        let _ = note(&state, token, me);
    }
    let shell = login_shell();
    #[cfg(unix)]
    let error = {
        let name = shell.file_name().map_or_else(|| "sh".into(), |name| name.to_string_lossy().into_owned());
        Command::new(&shell).arg0(format!("-{name}")).exec()
    };
    #[cfg(windows)]
    let error = io::Error::from(io::ErrorKind::Unsupported);
    eprintln!("milo: cannot start {}: {error}", shell.display());
    127
}

fn login_shell() -> PathBuf {
    let named = std::env::var_os("SHELL").map(PathBuf::from).filter(|shell| shell.is_absolute());
    named.unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn shells(state: &Path) -> PathBuf {
    state.join("ssh").join("shells")
}

fn note(state: &Path, token: &str, me: Process) -> io::Result<()> {
    let dir = shells(state);
    #[cfg(unix)]
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    #[cfg(windows)]
    fs::create_dir_all(&dir)?;
    fs_ops::open_dir(&dir)?;
    // Notes of shells that have ended are only clutter.
    for entry in fs::read_dir(&dir)?.flatten() {
        if read_note(&entry.path()).is_none_or(|shell| !shell.alive()) {
            let _ = fs::remove_file(entry.path());
        }
    }
    let (temp, mut file) = fs_ops::Temp::create(dir.join(format!(".{token}")))?;
    file.write_all(format!("{} {}\n", me.pid, me.start).as_bytes())?;
    temp.replace(&dir.join(token))
}

fn read_note(path: &Path) -> Option<Process> {
    use std::io::Read as _;
    let mut file = fs_ops::open_read(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: geteuid has no preconditions.
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
            return None;
        }
    }
    let mut text = String::new();
    (&mut file).take(64).read_to_string(&mut text).ok()?;
    let (pid, start) = text.trim_end().split_once(' ')?;
    Some(Process { pid: pid.parse().ok()?, start: start.parse().ok()? })
}

/// The shell noted under `token`, if it is still running.
pub fn noted_shell(state: &Path, token: &str) -> Option<Process> {
    read_note(&shells(state).join(token)).filter(Process::alive)
}

/// Opens this host's pane for `role`. Nothing it shows can be edited here.
pub fn pane(
    role: Role,
    token: &str,
    host: &str,
    make: fn(Repo, &Preferences) -> Box<dyn Pane>,
) -> Result<Box<dyn Pane>, String> {
    crate::reader::forbid_edits("this pane follows a shell over SSH");
    if role == Role::Usage {
        return Ok(Box::new(usage::Labeled::new(Box::new(crate::usage::Usage::open()), host)));
    }
    let state = paths::state_dir().ok_or("Milo's state folder is unavailable on this host")?;
    // The pane can be asked for while the shell's session is still starting.
    let until = std::time::Instant::now() + SHELL_WAIT;
    let shell = loop {
        match noted_shell(&state, token) {
            Some(shell) => break shell,
            None if std::time::Instant::now() < until => std::thread::sleep(std::time::Duration::from_millis(100)),
            None => return Err("the shell that milo ssh started here has ended".into()),
        }
    };
    let dir = shell.cwd().ok_or("cannot read the shell's folder on this host")?;
    let repo = Repo::open_to_read(&dir).ok_or_else(|| format!("cannot open {}", dir.display()))?;
    Ok(workflow::remote(repo, shell, host.to_owned(), make))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn only_whole_tokens_hosts_and_roles_reach_the_helper() {
        let token = "0123456789abcdef0123456789abcdef";
        assert_eq!(Request::parse(&["probe"]), Some(Request::Probe));
        assert_eq!(Request::parse(&["shell", token]), Some(Request::Shell(token)));
        assert_eq!(
            Request::parse(&["pane", "history", token, "ub-1"]),
            Some(Request::Pane { role: Role::History, token, host: "ub-1" })
        );
        for words in
            [&["shell", "../x"][..], &["pane", "reader", token, "ub-1"], &["pane", "outline", token, "a b"], &[]]
        {
            assert_eq!(Request::parse(words), None, "{words:?}");
        }
    }

    #[test]
    fn a_noted_shell_is_found_while_it_runs_and_swept_once_it_ends() {
        let state = Fixture::folder();
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let running = Process::of(i32::try_from(child.id()).unwrap()).unwrap();
        let [first, second] = ["a".repeat(32), "b".repeat(32)];
        note(&state.root, &first, running).unwrap();
        assert_eq!(noted_shell(&state.root, &first), Some(running));
        assert_eq!(noted_shell(&state.root, &second), None);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(noted_shell(&state.root, &first), None);
        note(&state.root, &second, Process::current().unwrap()).unwrap();
        assert!(!shells(&state.root).join(&first).exists());
        assert!(shells(&state.root).join(&second).exists());
    }
}
