//! `milo ssh` in real Rex panes, over a real ssh connection to a disposable
//! sshd on the loopback address.
//!
//! The "host" is this computer, reached as the same user through an sshd of
//! the test's own: its own host key, the one client key it accepts, no
//! password, no agent or forwarding, listening only on 127.0.0.1. Sessions
//! there get a home, settings and search path of the test's own. The client's
//! ssh reads only the test's configuration file, given with -F. The
//! developer's SSH files, agent and known hosts are never used.
//!
//! One tab has a shell in local repository A beside an Outline, a History, a
//! Usage and a reader holding an unsaved draft; a second tab has its own shell
//! and Outline in A. The shell logs in, goes from B to C on the host, logs
//! out, logs in again and loses its connection, then logs in to a host
//! without Milo.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use super::{Lab, soon};

/// Keeps a pane's frame for a pull request, where a folder is named for it.
fn evidence(lab: &Lab, session: &str, block: &str, name: &str) {
    if let Some(dir) = std::env::var_os("MILO_TEST_SSH_EVIDENCE").map(PathBuf::from) {
        fs::create_dir_all(&dir).unwrap();
        let html = lab.rex(&["capture", "-s", session, "-b", block, "--format=html", "--trim"]);
        fs::write(dir.join(format!("{name}.html")), html).unwrap();
    }
}

/// An sshd of the test's own, and the files a client needs to reach it.
struct Host {
    sshd: Child,
    dir: PathBuf,
    /// The client's configuration, naming this host as `fixture`.
    config: PathBuf,
    /// Where the host's sessions find milo, emptied to take Milo away.
    bin: PathBuf,
    home: PathBuf,
}

impl Host {
    fn start(lab: &Lab) -> Self {
        let dir = lab.root.join("host");
        let (home, bin) = (dir.join("home"), dir.join("bin"));
        for folder in [&home, &bin, &home.join(".config"), &home.join(".local/state")] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        for key in ["host_key", "client_key"] {
            let made = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", "milo-test", "-f"])
                .arg(dir.join(key))
                .status()
                .unwrap();
            assert!(made.success());
        }
        fs::copy(dir.join("client_key.pub"), dir.join("authorized_keys")).unwrap();
        std::os::unix::fs::symlink(&lab.milo, bin.join("milo")).unwrap();
        fs::write(home.join(".zshenv"), "unsetopt global_rcs\nunset HISTFILE\n").unwrap();
        fs::write(home.join(".zshrc"), "PROMPT='host %1~ %# '\ncd \"$HOME\"\n").unwrap();
        fs::write(home.join(".bashrc"), "PS1='host \\W \\$ '\ncd \"$HOME\"\n").unwrap();
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let shell = super::Lab::shell();
        let env = format!(
            "HOME={0} ZDOTDIR={0} XDG_CONFIG_HOME={0}/.config XDG_STATE_HOME={0}/.local/state SHELL={shell} \
             PATH={1}:/usr/bin:/bin:/usr/sbin:/sbin GIT_CONFIG_NOSYSTEM=1",
            home.display(),
            bin.display()
        );
        let d = dir.display();
        fs::write(
            dir.join("sshd_config"),
            format!(
                "ListenAddress 127.0.0.1\nPort {port}\nHostKey {d}/host_key\nPidFile {d}/sshd.pid\n\
                 AuthorizedKeysFile {d}/authorized_keys\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
                 PubkeyAuthentication yes\nUsePAM no\nStrictModes no\nAllowAgentForwarding no\nAllowTcpForwarding no\n\
                 X11Forwarding no\nPermitUserEnvironment no\nPrintMotd no\nPrintLastLog no\nSetEnv {env}\n"
            ),
        )
        .unwrap();
        let host_key = fs::read_to_string(dir.join("host_key.pub")).unwrap();
        let host_key: Vec<&str> = host_key.split_whitespace().take(2).collect();
        fs::write(dir.join("known_hosts"), format!("[127.0.0.1]:{port} {}\n", host_key.join(" "))).unwrap();
        let user = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout).unwrap();
        let config = dir.join("ssh_config");
        fs::write(
            &config,
            format!(
                "Host fixture\n  HostName 127.0.0.1\n  Port {port}\n  User {}\n  IdentityFile {d}/client_key\n  \
                 IdentitiesOnly yes\n  IdentityAgent none\n  UserKnownHostsFile {d}/known_hosts\n  \
                 StrictHostKeyChecking yes\n  LogLevel ERROR\n",
                user.trim()
            ),
        )
        .unwrap();
        let sshd = ["/usr/sbin/sshd", "/usr/bin/sshd"].into_iter().find(|path| Path::new(path).exists()).unwrap();
        let log = fs::File::create(dir.join("sshd.log")).unwrap();
        let sshd = Command::new(sshd)
            .args(["-D", "-e", "-f"])
            .arg(dir.join("sshd_config"))
            .env_clear()
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        assert!(
            soon(|| TcpStream::connect(("127.0.0.1", port)).is_ok()),
            "{}",
            fs::read_to_string(dir.join("sshd.log")).unwrap()
        );
        Self { sshd, dir, config, bin, home }
    }

    /// Ends every session the host is serving and all they started, so that
    /// the client's connection closes under it as a dropped network would.
    fn cut(&self) {
        let mut found = vec![self.sshd.id().to_string()];
        let mut sessions = Vec::new();
        while let Some(parent) = found.pop() {
            let children = Command::new("pgrep").args(["-P", &parent]).output().unwrap();
            for child in String::from_utf8(children.stdout).unwrap().split_whitespace() {
                found.push(child.to_owned());
                sessions.push(child.to_owned());
            }
        }
        assert!(!sessions.is_empty(), "{}", fs::read_to_string(self.dir.join("sshd.log")).unwrap());
        let killed = Command::new("kill").arg("-KILL").args(&sessions).status().unwrap();
        assert!(killed.success());
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.sshd.kill();
        let _ = self.sshd.wait();
    }
}

struct Tab<'a> {
    lab: &'a Lab,
    session: String,
    window: String,
    shell: String,
    dir: PathBuf,
}

impl<'a> Tab<'a> {
    fn pane(&self, label: &str, share: &str, args: &[&str]) -> String {
        let place = ["split", "-s", &self.session, "-b", &self.shell, "-c", self.dir.to_str().unwrap()];
        let pane =
            ["--split=left", "--ratio", share, "--shell=none", "--label", label, "--", self.lab.milo.to_str().unwrap()];
        self.lab.rex(&[place.as_slice(), &pane, args].concat());
        let find = || {
            self.lab
                .view()
                .tab(&self.window)
                .panes
                .iter()
                .find(|pane| pane.label == label)
                .map(|pane| pane.block.clone())
        };
        assert!(soon(|| find().is_some()), "no pane {label}");
        find().unwrap()
    }

    fn screen(&self, block: &str) -> String {
        self.lab.rex(&["capture", "-s", &self.session, "-b", block, "--trim"])
    }

    fn shows(&self, block: &str, text: &str) {
        assert!(soon(|| self.screen(block).contains(text)), "expected {text:?} on:\n{}", self.screen(block));
    }

    fn lacks(&self, block: &str, text: &str) {
        assert!(soon(|| !self.screen(block).contains(text)), "unexpected {text:?} on:\n{}", self.screen(block));
    }

    fn keys(&self, block: &str, keys: &[&str]) {
        self.lab.rex(&[["send-key", "-s", self.session.as_str(), "-b", block].as_slice(), keys].concat());
    }

    /// Types a line into the shell and presses Return.
    fn run(&self, line: &str) {
        self.lab.rex(&["send", "-s", &self.session, "-b", &self.shell, line]);
        self.keys(&self.shell, &["enter"]);
    }
}

fn repository(lab: &Lab, dir: &Path, name: &str) {
    fs::create_dir_all(dir).unwrap();
    lab.git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join(format!("{name}-committed.txt")), format!("{name}\n")).unwrap();
    lab.git(dir, &["add", "-A"]);
    lab.git(dir, &["commit", "-q", "-m", &format!("Commit in {name}")]);
    fs::write(dir.join(format!("{name}-changed.txt")), format!("{name} changed\n")).unwrap();
}

#[test]
#[ignore = "needs an installed Rex and sshd; starts a disposable server and an sshd on 127.0.0.1"]
fn panes_follow_a_shell_over_ssh_and_come_back() {
    let lab = Lab::start();
    let local = lab.root.join("A");
    repository(&lab, &local, "alpha");
    fs::write(local.join("notes.txt"), "saved notes\n").unwrap();
    let host = Host::start(&lab);
    let (remote_b, remote_c) = (host.home.join("B"), host.home.join("C"));
    repository(&lab, &remote_b, "bravo");
    repository(&lab, &remote_c, "charlie");

    let [session, tab] = lab.new_session("ssh", "work", &local);
    let shell = lab.view().tab(&tab).panes[0].block.clone();
    assert!(shell.starts_with("block:"), "{shell}");
    let work =
        Tab { lab: &lab, session: session.clone(), window: tab.clone(), shell: shell.clone(), dir: local.clone() };
    let reader = work.pane("draft", "70", &["view", local.join("notes.txt").to_str().unwrap(), "--edit"]);
    work.shows(&reader, "saved notes");
    lab.rex(&["send", "-s", &session, "-b", &reader, "DRAFT-KEPT "]);
    work.shows(&reader, "DRAFT-KEPT");
    let usage = work.pane("usage", "40", &["usage", "--shell", &shell]);
    let history = work.pane("history", "40", &["history", "--shell", &shell]);
    let outline = work.pane("outline", "50", &["outline", "--shell", &shell]);
    work.shows(&outline, "alpha-changed.txt");
    work.shows(&history, "Commit in alpha");

    let other_tab = lab.new_tab(&session, &local);
    let other_shell = lab.view().tab(&other_tab).panes[0].block.clone();
    let other = Tab {
        lab: &lab,
        session: session.clone(),
        window: other_tab.clone(),
        shell: other_shell.clone(),
        dir: local.clone(),
    };
    let other_outline = other.pane("outline", "50", &["outline", "--shell", &other_shell]);
    other.shows(&other_outline, "alpha-changed.txt");

    let milo = lab.milo.to_str().unwrap();
    let login = format!("'{milo}' ssh -F '{}' fixture", host.config.display());
    work.run(&login);
    work.shows(&shell, "following this shell on fixture");
    work.run(&format!("cd '{}'", remote_b.display()));
    work.shows(&outline, "bravo-changed.txt");
    work.shows(&outline, "fixture · showing B");
    work.shows(&history, "Commit in bravo");
    work.shows(&history, "fixture · showing B");
    evidence(&lab, &session, &outline, "outline-on-host-B");
    work.run(&format!("cd '{}'", remote_c.display()));
    work.shows(&outline, "charlie-changed.txt");
    work.shows(&outline, "fixture · showing C");
    work.shows(&history, "Commit in charlie");
    evidence(&lab, &session, &outline, "outline-on-host-C");
    evidence(&lab, &session, &history, "history-on-host-C");
    other.shows(&other_outline, "alpha-changed.txt");
    other.lacks(&other_outline, "fixture");

    // The host's reader only reads.
    work.keys(&outline, &["n"]);
    work.keys(&outline, &["enter"]);
    work.shows(&outline, "charlie changed");
    work.keys(&outline, &["e"]);
    work.shows(&outline, "follows a shell over SSH");
    work.keys(&outline, &["q"]);
    work.lacks(&outline, "follows a shell over SSH");

    // Usage stays this computer's until the person asks for the host's, and comes back.
    work.shows(&usage, "This computer's usage · Tab: fixture");
    evidence(&lab, &session, &usage, "usage-here-while-on-host");
    work.keys(&usage, &["tab"]);
    work.shows(&usage, "fixture's usage · Tab: back to the other computer's");
    evidence(&lab, &session, &usage, "usage-of-host");
    work.keys(&usage, &["tab"]);
    work.shows(&usage, "This computer's usage · Tab: fixture");

    // Logging out brings the panes back as they were.
    work.run("exit");
    work.shows(&outline, "alpha-changed.txt");
    work.lacks(&outline, "fixture");
    work.lacks(&usage, "fixture");
    work.shows(&history, "Commit in alpha");
    work.shows(&reader, "DRAFT-KEPT");
    assert_eq!(fs::read_to_string(local.join("notes.txt")).unwrap(), "saved notes\n");
    evidence(&lab, &session, &outline, "outline-back-after-logout");

    // A lost connection does too.
    work.run(&login);
    work.shows(&shell, "following this shell on fixture");
    work.run(&format!("cd '{}'", remote_b.display()));
    work.shows(&outline, "fixture · showing B");
    host.cut();
    work.shows(&shell, "lost the connection to fixture");
    work.shows(&outline, "alpha-changed.txt");
    work.lacks(&outline, "fixture");
    work.shows(&reader, "DRAFT-KEPT");

    // Without Milo on the host, the shell still works and the panes say why they stay.
    fs::remove_file(host.bin.join("milo")).unwrap();
    work.run(&login);
    work.shows(&shell, "Milo isn't on fixture's search path");
    work.shows(&outline, "fixture · Can't follow there · showing A");
    work.shows(&usage, "fixture has no Milo");
    evidence(&lab, &session, &outline, "outline-without-milo-on-host");
    work.run(&format!("cd '{}' && echo \"remote-shell-$(basename \"$PWD\")\"", remote_c.display()));
    work.shows(&shell, "remote-shell-C");
    work.shows(&outline, "alpha-changed.txt");
    work.run("exit");
    work.lacks(&outline, "fixture");
    work.lacks(&usage, "fixture");
    work.shows(&reader, "DRAFT-KEPT");
    other.lacks(&other_outline, "fixture");
    assert!(
        !host.home.join(".local/state/milo/ssh/shells").read_dir().is_ok_and(|mut d| d.any(|e| {
            let path = e.unwrap().path();
            fs::read_to_string(path).is_ok_and(|note| {
                note.split(' ').next().and_then(|pid| pid.parse::<i32>().ok()).is_some_and(|pid| {
                    // SAFETY: kill with signal 0 only asks whether the process exists.
                    unsafe { libc::kill(pid, 0) == 0 }
                })
            })
        })),
        "a shell outlived its session"
    );
    assert!(
        !lab.root.join("home/.local/state/milo/ssh/tabs").read_dir().is_ok_and(|mut d| d.next().is_some()),
        "a link was left behind"
    );
}
