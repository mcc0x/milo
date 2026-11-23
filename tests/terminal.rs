//! Standalone terminal capability checks through a real PTY.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let home = Self(std::env::temp_dir().join(format!("milo-terminal-{}-{id}", std::process::id())));
        fs::create_dir(&home.0).unwrap();
        fs::write(home.0.join("page.txt"), "a standalone terminal\n").unwrap();
        home
    }

    fn milo(&self, term: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_milo"));
        command
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("TERM", term)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("COLORTERM")
            .env_remove("NO_COLOR")
            .env_remove("MILO_GRAPHICS")
            .env_remove("MILO_ICONS")
            .env_remove("TERM_PROGRAM")
            .env_remove("TMUX")
            .env_remove("STY")
            .env_remove("REX_BLOCK")
            .env_remove("REX_SESSION")
            .env_remove("REX_SERVER");
        command
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Terminal {
    child: Child,
    master: File,
    // Keep queued stdout readable even if the child exits before it is inspected.
    _slave: File,
    bytes: Vec<u8>,
}

impl Terminal {
    fn run(command: &mut Command) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: The two descriptor outputs and the size are live, and optional outputs are null.
        let result =
            unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
        assert_eq!(result, 0, "{}", io::Error::last_os_error());
        // SAFETY: A successful openpty gives the caller ownership of both distinct descriptors.
        let master = unsafe { File::from_raw_fd(master) };
        // SAFETY: This descriptor is distinct from the master and owned by this fixture.
        let slave = unsafe { File::from_raw_fd(slave) };
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            // SAFETY: Each fd is live and should not be inherited on exec, except as stdin or stdout below.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }, 0);
        }
        let child = command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self { child, master, _slave: slave, bytes: Vec::new() }
    }

    fn until(&mut self, text: &str) {
        let deadline = Instant::now() + WAIT;
        while !String::from_utf8_lossy(&self.bytes).contains(text) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "waiting for {text:?}: {:?}", String::from_utf8_lossy(&self.bytes));
            let timeout = i32::try_from(left.as_millis() + 1).unwrap();
            let mut poll = libc::pollfd { fd: self.master.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: `poll` is one live descriptor record.
            let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
            if ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            assert!(ready > 0 && poll.revents & libc::POLLIN != 0, "missing {text:?}: {:?}", self.bytes);
            let mut buffer = [0; 4096];
            let len = self.master.read(&mut buffer).unwrap();
            assert_ne!(len, 0);
            self.bytes.extend_from_slice(&buffer[..len]);
        }
    }

    fn finish(&mut self) -> ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "milo did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn dumb_terminal_is_refused_without_writing_drawing_controls() {
    let home = Home::new();
    let mut terminal = Terminal::run(home.milo("dumb").args(["view", "page.txt"]));
    assert_eq!(terminal.finish().code(), Some(1));
    let mut error = String::new();
    terminal.child.stderr.take().unwrap().read_to_string(&mut error).unwrap();
    assert!(error.contains("TERM=dumb does not support panes"), "{error:?}");
    assert!(!error.contains('\x1b'));
    let mut poll = libc::pollfd { fd: terminal.master.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: `poll` is one live descriptor record; a closed slave might report HUP but not input.
    unsafe { libc::poll(&mut poll, 1, 0) };
    assert_eq!(poll.revents & libc::POLLIN, 0);
}

#[test]
fn a_nested_terminal_with_inherited_rex_ids_uses_the_standalone_reader() {
    let home = Home::new();
    let mut command = home.milo("tmux-256color");
    command
        .arg("outline")
        .env("TERM_PROGRAM", "rex")
        .env("REX_SESSION", "session:00000000-0000-0000-0000-000000000001")
        .env("REX_BLOCK", "block:00000000-0000-0000-0000-000000000002")
        .env("REX_SERVER", home.0.join("absent.sock"))
        .env("NO_COLOR", "1");
    let mut terminal = Terminal::run(&mut command);
    terminal.until("page.txt");
    assert!(String::from_utf8_lossy(&terminal.bytes).contains('•'));
    assert!(!terminal.bytes.windows(3).any(|bytes| bytes == b"\x1b_G"));
    terminal.master.write_all(b"\r").unwrap();
    terminal.until("a standalone terminal");
    terminal.master.write_all(b"qq").unwrap();
    terminal.until("\x1b[?1049l");
    assert!(terminal.finish().success());
    let text = String::from_utf8_lossy(&terminal.bytes);
    for sgr in text.split('\x1b').filter_map(|part| part.strip_prefix('[')) {
        if let Some((parameters, _)) = sgr.split_once('m')
            && parameters.bytes().all(|byte| byte.is_ascii_digit() || byte == b';')
        {
            assert!(
                parameters.split(';').all(|value| matches!(value, "0" | "1" | "2" | "3" | "4" | "7" | "9")),
                "{sgr:?}"
            );
        }
    }
}
