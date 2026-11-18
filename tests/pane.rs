//! Runs pane startup and input through a real terminal.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

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
            "#!/bin/sh\n[ \"$1\" = --exec-path ] && exit 1\nwhile [ -d \"$HOME\" ] && [ ! -e \"$HOME/go\" ]; do sleep 0.02; done\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(git, fs::Permissions::from_mode(0o755)).unwrap();
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
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: Both descriptor outputs and the size are live, and optional outputs are null.
        let result =
            unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: Successful openpty gives the caller ownership of both descriptors.
        let master = unsafe { File::from_raw_fd(master) };
        // SAFETY: The slave is a different descriptor from the master.
        let slave = unsafe { File::from_raw_fd(slave) };
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
        let mut fd = libc::pollfd { fd: self.master.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = i32::try_from(until.saturating_duration_since(Instant::now()).as_millis()).unwrap();
        // SAFETY: A single live pollfd points to an open descriptor.
        if unsafe { libc::poll(&mut fd, 1, ms) } <= 0 {
            return false;
        }
        let mut buf = [0; 8192];
        let len = self.master.read(&mut buf).unwrap();
        self.bytes.extend_from_slice(&buf[..len]);
        len > 0
    }

    fn until(&mut self, text: &str) {
        let until = Instant::now() + Duration::from_secs(8);
        while !self.output().contains(text) {
            assert!(Instant::now() < until && self.read(until), "expected {text:?} in {:?}", self.output());
        }
    }

    fn output(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
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
    assert_eq!(terminal.child.wait().unwrap().code(), Some(1));
    assert!(terminal.restored());
}

// The watched history waits 30 seconds even when nothing changes on disk.
// Its live age lets us tell a redraw from an ignored key without a debug hook.
#[cfg(target_os = "macos")]
#[test]
fn an_ignored_key_leaves_a_frame_alone_but_a_terminal_reply_redraws_it() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = || SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let project = Project::new();
    let git = |args: &[&str]| {
        let output = project.command("git").args(args).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Milo Test"]);
    git(&["config", "user.email", "test@example.invalid"]);
    let transition = now() + 5;
    let date = format!("{} +0000", transition - 60);
    let output = project
        .command("git")
        .env("GIT_COMMITTER_DATE", date)
        .args(["commit", "-qm", "Recent commit", "--allow-empty"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let mut terminal = Terminal::run(project.command(env!("CARGO_BIN_EXE_milo")).arg("history"));
    terminal.until("mnow");
    std::thread::sleep(Duration::from_secs(transition.saturating_sub(now())));
    let before = terminal.bytes.len();
    terminal.master.write_all(b"z").unwrap();
    let until = Instant::now() + Duration::from_millis(350);
    while terminal.read(until) {}
    assert_eq!(terminal.bytes.len(), before, "{}", terminal.output());
    terminal.master.write_all(b"\x1b]11;rgb:1111/1111/1111\x1b\\").unwrap();
    terminal.until("m1m");
}
