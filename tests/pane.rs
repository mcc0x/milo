//! Runs pane startup and input through a real terminal.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const TEST_WAIT: Duration = Duration::from_secs(60);

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("milo-pane-{}-{id}", std::process::id()));
        fs::create_dir_all(root.join("project")).unwrap();
        Self(root)
    }

    fn command(&self, program: impl AsRef<Path>) -> Command {
        let mut command = Command::new(program.as_ref());
        command
            .current_dir(self.0.join("project"))
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK");
        command
    }

    fn slow_git(&self, command: &mut Command) {
        let git = self.0.join("git");
        fs::write(
            &git,
            "#!/bin/sh\n[ \"$1\" = ready ] && exit 0\n[ \"$1\" = --exec-path ] && exit 1\nwhile [ -d \"$HOME\" ] && [ ! -e \"$HOME/go\" ]; do sleep 0.02; done\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
        // A sibling's fork can inherit the writer until it execs. Probe the script
        // itself before starting a pane that assumes it can execute immediately.
        let until = Instant::now() + TEST_WAIT;
        loop {
            match self.command(&git).arg("ready").status() {
                Ok(status) => {
                    assert!(status.success());
                    break;
                }
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    assert!(Instant::now() < until, "stand-in git is still open for writing");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => panic!("cannot run stand-in git: {e}"),
            }
        }
        let mut paths = vec![self.0.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Terminal {
    child: Child,
    master: File,
    slave: File,
    modes: libc::termios,
    bytes: Vec<u8>,
}

impl Terminal {
    fn run(command: &mut Command) -> Self {
        Self::sized(command, 80, 24)
    }

    fn sized(command: &mut Command, cols: u16, rows: u16) -> Self {
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: Both descriptor outputs and the size are live, and optional outputs are null.
        let result =
            unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: Successful openpty gives the caller ownership of both descriptors.
        let master = unsafe { File::from_raw_fd(master) };
        // SAFETY: The slave is a different descriptor from the master.
        let slave = unsafe { File::from_raw_fd(slave) };
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            // SAFETY: The descriptor is open; the child gets only the slave on its standard streams.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }, 0);
        }
        let modes = modes(&slave);
        let child = command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap())
            .spawn()
            .unwrap();
        Self { child, master, slave, modes, bytes: Vec::new() }
    }

    fn read(&mut self, until: Instant) -> bool {
        let mut buf = [0; 8192];
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let mut fd = libc::pollfd { fd: self.master.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let ms = i32::try_from(left.as_millis() + 1).unwrap();
            // SAFETY: A single live pollfd points to an open descriptor.
            match unsafe { libc::poll(&mut fd, 1, ms) } {
                0 => return false,
                n if n < 0 => {
                    let error = io::Error::last_os_error();
                    assert_eq!(error.kind(), io::ErrorKind::Interrupted, "{error}");
                    continue;
                }
                _ => {}
            }
            match self.master.read(&mut buf) {
                Ok(len) => {
                    self.bytes.extend_from_slice(&buf[..len]);
                    return len > 0;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => panic!("cannot read test terminal: {e}"),
            }
        }
    }

    fn until(&mut self, text: &str) {
        let until = Instant::now() + TEST_WAIT;
        while !self.output().contains(text) {
            assert!(self.read(until), "expected {text:?} in {:?}", self.output());
        }
    }

    fn output(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }

    fn wait_for_exit(&mut self) -> ExitStatus {
        let until = Instant::now() + TEST_WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < until, "milo didn't exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn restored(&self) -> bool {
        let new = modes(&self.slave);
        let old = &self.modes;
        (new.c_iflag, new.c_oflag, new.c_cflag, new.c_lflag, new.c_cc)
            == (old.c_iflag, old.c_oflag, old.c_cflag, old.c_lflag, old.c_cc)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn modes(slave: &File) -> libc::termios {
    // SAFETY: A zeroed termios is valid, and tcgetattr can write to it below.
    let mut modes = unsafe { std::mem::zeroed() };
    // SAFETY: The descriptor is open and the output is writable.
    assert_eq!(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut modes) }, 0);
    modes
}

#[test]
fn retrying_a_failed_reader_replaces_the_still_open_layer() {
    let project = Project::new();
    fs::write(project.0.join("project/page.txt"), "data").unwrap();
    let rex = project.0.join("rex");
    fs::write(
        &rex,
        r#"#!/bin/sh
cat >/dev/null
for last do :; done
echo "$last" >> "$HOME/opens"
case $(wc -l < "$HOME/opens" | tr -d ' ') in
    1) echo layer:first ;;
    2) exit 1 ;;
    *) echo layer:next ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(rex, fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![project.0.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    command.env("PATH", std::env::join_paths(paths).unwrap()).env("REX_SESSION", "test").env("REX_BLOCK", "test");
    let mut tty = Terminal::run(command.arg("outline"));
    tty.until("page.txt");
    tty.master.write_all(b"\r\r").unwrap();
    tty.until("Rex did not open the file");
    tty.master.write_all(b"\rq").unwrap();
    assert!(tty.wait_for_exit().success());
    let requests = fs::read_to_string(project.0.join("opens")).unwrap();
    let retry = requests.lines().nth(2).expect("the third open reached Rex");
    assert!(retry.contains(r#""previous":"layer:first""#), "{retry}");
}

#[test]
fn a_slow_open_draws_the_project_name_before_it_has_the_files() {
    let project = Project::new();
    fs::write(project.0.join("project/page.txt"), "data").unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    project.slow_git(&mut command);
    let mut terminal = Terminal::run(command.arg("outline"));
    terminal.until("Reading…");
    assert!(terminal.output().contains("project"));
    assert!(!terminal.output().contains("page.txt"));
    assert!(!terminal.restored());
    fs::write(project.0.join("go"), "").unwrap();
    terminal.until("page.txt");
}

#[test]
fn git_panes_wait_for_a_persistent_index_lock_without_removing_it() {
    let project = Project::new();
    let git = |args: &[&str]| {
        let output = project
            .command("git")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .args(["-c", "user.name=Milo Test", "-c", "user.email=test@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    };
    let root = project.0.join("project");
    git(&["init"]);
    fs::write(root.join("page.txt"), "before\n").unwrap();
    git(&["add", "page.txt"]);
    git(&["commit", "-m", "Baseline"]);
    let later = git(&["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "UnlockedCommit"]);
    let lock = root.join(".git/index.lock");
    fs::write(&lock, "held by Git").unwrap();

    let milo = env!("CARGO_BIN_EXE_milo");
    let mut panes = [
        (Terminal::sized(project.command(milo).arg("outline"), 80, 8), "later.txt"),
        (Terminal::sized(project.command(milo).arg("history"), 80, 8), "UnlockedCommit"),
        (Terminal::sized(project.command(milo).args(["view", "page.txt"]), 80, 8), "updated"),
    ];
    for (terminal, _) in &mut panes {
        terminal.until("Waiting for Git");
    }
    assert_eq!(fs::read_to_string(&lock).unwrap(), "held by Git");

    fs::write(root.join("later.txt"), "new file\n").unwrap();
    fs::write(root.join("page.txt"), "updated\n").unwrap();
    git(&["update-ref", "HEAD", later.trim()]);
    fs::remove_file(lock).unwrap();
    for (terminal, updated) in &mut panes {
        terminal.until(updated);
        terminal.master.write_all(b"q").unwrap();
        assert!(terminal.wait_for_exit().success());
    }
}

#[test]
fn ctrl_c_during_a_slow_open_restores_the_terminal_without_waiting_for_git() {
    let project = Project::new();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    project.slow_git(&mut command);
    let mut terminal = Terminal::run(command.arg("outline"));
    terminal.until("Reading…");
    assert!(!terminal.restored());
    terminal.master.write_all(b"\x03").unwrap();
    terminal.until("\x1b[?1049l");
    assert!(terminal.restored());
    assert_eq!(terminal.wait_for_exit().code(), Some(1));
    assert!(!project.0.join("go").exists());
}

#[test]
fn a_closed_pty_stops_a_slow_open_without_waiting_for_git() {
    let project = Project::new();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    project.slow_git(&mut command);
    let mut terminal = Terminal::run(command.arg("outline"));
    terminal.until("Reading…");
    // A normal close after Git has timed out would exit successfully instead.
    terminal.master = File::open("/dev/null").unwrap();
    assert_eq!(terminal.wait_for_exit().code(), Some(1));
    #[cfg(target_os = "macos")]
    assert!(terminal.restored());
}

#[test]
fn sigterm_restores_the_terminal_even_when_its_output_queue_is_full() {
    let project = Project::new();
    fs::write(project.0.join("project/wall.txt"), format!("{}\n", "X".repeat(600)).repeat(200)).unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    project.slow_git(&mut command);
    fs::write(project.0.join("go"), "").unwrap();
    let mut terminal = Terminal::sized(command.args(["view", "wall.txt"]), 600, 200);
    // Wait for the large file frame, not the small frame drawn while it opens.
    terminal.until("XXXXXXXXXXXXXXXX");
    let pid = i32::try_from(terminal.child.id()).unwrap();
    // SAFETY: This is our own, still-owned child, not a process group.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    assert_eq!(terminal.wait_for_exit().code(), Some(1));
    assert!(terminal.restored());
    terminal.until("\x1b[?1049l");
}

#[test]
fn a_failed_open_restores_the_terminal_before_printing_the_error() {
    let project = Project::new();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    project.slow_git(&mut command);
    let mut terminal = Terminal::run(command.args(["outline", "missing"]));
    terminal.until("Reading…");
    assert!(!terminal.restored());
    fs::write(project.0.join("go"), "").unwrap();
    let error = "milo: missing is not a directory";
    terminal.until(error);
    let output = terminal.output();
    assert!(output.find("\x1b[?1049l").unwrap() < output.find(error).unwrap());
    assert_eq!(terminal.wait_for_exit().code(), Some(1));
    assert!(terminal.restored());
}

#[test]
fn help_and_version_write_to_stdout_and_argument_errors_write_to_stderr() {
    let project = Project::new();
    let run = |args: &[&str]| {
        let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
        if args[0] == "not-a-milo-command" {
            project.slow_git(&mut command);
            fs::write(project.0.join("go"), "").unwrap();
        }
        command.args(args).output().unwrap()
    };
    for (args, first_line) in [
        (&["--help"][..], "milo — file, history, usage, and reader panes for your terminal"),
        (&["--version"][..], concat!("milo ", env!("CARGO_PKG_VERSION"))),
        (&["outline", "--help"][..], "Usage: milo outline [PATH]"),
        (&["history", "--help"][..], "Usage: milo history [PATH]"),
        (&["usage", "--help"][..], "Usage: milo usage"),
        (&["setup", "--help"][..], "Usage: milo setup"),
        (&["view", "--help"][..], "Usage: milo view FILE [--commit ID]"),
        (&["listen", "--help"][..], "Usage: milo listen [--stop]"),
        (&["claude-status", "--help"][..], "Usage: milo claude-status"),
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
        assert_eq!(String::from_utf8(output.stdout).unwrap().lines().next(), Some(first_line), "{args:?}");
    }
    for (args, message) in [
        (&["not-a-milo-command"][..], "milo: not-a-milo-command is not a directory; see milo --help for commands\n"),
        (&["view"][..], "milo: missing file; use milo view FILE [--commit ID]\n"),
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), message);
    }
}

#[test]
fn doctor_and_removal_do_not_install_anything_on_a_fresh_account() {
    let project = Project::new();
    let settings = project.0.join(".claude/settings.json");
    fs::create_dir(settings.parent().unwrap()).unwrap();
    let original = br#"{"statusLine":{"type":"command","command":"mine"}}"#;
    fs::write(&settings, original).unwrap();
    for args in [&["doctor"][..], &["setup", "--remove"]] {
        let out = project
            .command(env!("CARGO_BIN_EXE_milo"))
            .env_remove("REX_CONFIG")
            .env("PI_CODING_AGENT_DIR", project.0.join("agent"))
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(!project.0.join("config").exists() && !project.0.join("state").exists());
        assert_eq!(fs::read(&settings).unwrap(), original);
    }
}

#[test]
fn panes_keep_global_preferences_when_a_worktree_override_is_invalid() {
    let project = Project::new();
    let initialized = project
        .command("git")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .args(["init", "-q"])
        .status()
        .unwrap();
    assert!(initialized.success());
    fs::write(project.0.join("project/page.txt"), "project text\n").unwrap();
    let command = || {
        let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
        command.env_remove("REX_CONFIG").env("CLAUDE_CONFIG_DIR", project.0.join("claude"));
        command
    };

    // Let Outline pick the worktree's settings file instead of duplicating its path hash.
    let mut outline = Terminal::run(command().arg("outline"));
    outline.until("c changed only");
    outline.master.write_all(b"c").unwrap();
    outline.until("all files");
    outline.master.write_all(b"q").unwrap();
    assert!(outline.wait_for_exit().success());

    let config = project.0.join("config/milo");
    let file = fs::read_dir(config.join("worktrees")).unwrap().next().unwrap().unwrap().path();
    let invalid = "changes_only = false\nhistory_follow_new = broken\n";
    fs::write(&file, invalid).unwrap();
    fs::write(config.join("preferences.conf"), "changes_only = true\n").unwrap();

    for (args, content) in [
        (&["outline"][..], "c all files"),
        (&["history"][..], "No commits yet."),
        (&["view", "page.txt"][..], "project text"),
        (&["usage"][..], "Codex"),
    ] {
        let mut tty = Terminal::run(command().args(args));
        tty.until("milo doctor: worktree prefs ignored");
        tty.until(content);
        if args[0] == "outline" {
            tty.master.write_all(b"c").unwrap();
            tty.until("Could not save changes-only preference");
        }
        tty.master.write_all(b"q").unwrap();
        assert!(tty.wait_for_exit().success());
    }

    let skipped = command().args(["setup", "--skip-onboarding"]).output().unwrap();
    assert!(skipped.status.success());
    let mut setup = Terminal::run(command().arg("setup"));
    setup.until("Could not read Milo's pane preferences.");
    setup.master.write_all(b"q").unwrap();
    assert!(setup.wait_for_exit().success());

    let doctor = command().arg("doctor").output().unwrap();
    assert!(doctor.status.success());
    let report = String::from_utf8(doctor.stdout).unwrap();
    assert!(report.contains("Preferences: unreadable"), "{report}");
    assert!(report.contains(file.file_name().unwrap().to_str().unwrap()), "{report}");
    assert!(report.contains("invalid preference history_follow_new"), "{report}");
    assert_eq!(fs::read_to_string(&file).unwrap(), invalid);
}

#[test]
fn unattended_skip_is_shared_by_home_and_settings_without_applying_preferences() {
    let project = Project::new();
    let milo = env!("CARGO_BIN_EXE_milo");
    let command = || {
        let mut c = project.command(milo);
        c.env_remove("REX_CONFIG").env("CLAUDE_CONFIG_DIR", project.0.join("claude"));
        c
    };
    let skipped = command().args(["setup", "--skip-onboarding"]).output().unwrap();
    assert!(skipped.status.success(), "{}", String::from_utf8_lossy(&skipped.stderr));
    for (args, expected) in [(&[][..], "Uninstall"), (&["setup"][..], "Workflow")] {
        let mut tty = Terminal::run(command().args(args));
        tty.until(expected);
        tty.master.write_all(b"q").unwrap();
        assert!(tty.wait_for_exit().success());
        assert!(tty.restored());
    }
    assert!(!project.0.join("config/milo/preferences.conf").exists());
    assert!(!project.0.join("config/rex/init.lua").exists());
}
