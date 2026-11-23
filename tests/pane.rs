//! Runs pane startup and input through a real terminal.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const TEST_WAIT: Duration = Duration::from_secs(60);

#[test]
fn help_exits_quietly_when_its_pipe_reader_is_gone() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let output = Command::new(env!("CARGO_BIN_EXE_milo")).arg("--help").stdout(writer).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.stderr.is_empty());
}

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
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK");
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self
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
    }

    fn slow_git(&self, command: &mut Command) {
        self.stand_in_git(command, "while [ -d \"$HOME\" ] && [ ! -e \"$HOME/go\" ]; do sleep 0.02; done\nexit 1\n");
    }

    /// Puts a `git` that runs `script` first on the command's path.
    fn stand_in_git(&self, command: &mut Command, script: &str) {
        let git = self.0.join("git");
        let start = "#!/bin/sh\n[ \"$1\" = ready ] && exit 0\n[ \"$1\" = --exec-path ] && exit 1\n";
        fs::write(&git, format!("{start}{script}")).unwrap();
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

    fn click(&mut self, col: u16, row: u16, then: &str) {
        self.master.write_all(format!("\x1b[<0;{col};{row}M{then}").as_bytes()).unwrap();
    }

    fn output(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }

    /// The rows a terminal would show after everything read so far. A frame
    /// carries only the cells that changed, so the words on a row are not
    /// always next to each other in the output. Every cell is taken as one column wide.
    fn screen(&self) -> Vec<String> {
        let output = self.output();
        let mut rows: Vec<Vec<char>> = Vec::new();
        let (mut x, mut y) = (0, 0);
        let mut chars = output.chars();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                if !c.is_control() {
                    if rows.len() <= y {
                        rows.resize(y + 1, Vec::new());
                    }
                    if rows[y].len() <= x {
                        rows[y].resize(x + 1, ' ');
                    }
                    rows[y][x] = c;
                    x += 1;
                }
                continue;
            }
            match chars.next() {
                Some('[') => {
                    let mut values = String::new();
                    let last = chars.by_ref().find(|&c| {
                        values.push(c);
                        ('@'..='~').contains(&c)
                    });
                    values.pop();
                    match last {
                        Some('H') => {
                            let mut at = values.split(';').map(|n| n.parse::<usize>().unwrap_or(1) - 1);
                            (y, x) = (at.next().unwrap_or(0), at.next().unwrap_or(0));
                        }
                        Some('J') => rows.clear(),
                        _ => {}
                    }
                }
                // A string for the terminal itself ends at ESC \ or a bell.
                Some(']' | '_' | 'P') => {
                    let mut escaped = false;
                    for c in chars.by_ref() {
                        if c == '\x07' || escaped && c == '\\' {
                            break;
                        }
                        escaped = c == '\x1b';
                    }
                }
                _ => {}
            }
        }
        rows.into_iter().map(|row| row.into_iter().collect()).collect()
    }

    /// Waits until some row of the screen holds `text`.
    fn shows(&mut self, text: &str) {
        let until = Instant::now() + TEST_WAIT;
        while !self.screen().iter().any(|row| row.contains(text)) {
            assert!(self.read(until), "expected {text:?} on {:#?}", self.screen());
        }
    }

    /// Waits until no row of the screen holds `text`.
    fn hides(&mut self, text: &str) {
        let until = Instant::now() + TEST_WAIT;
        while self.screen().iter().any(|row| row.contains(text)) {
            assert!(self.read(until), "expected no {text:?} on {:#?}", self.screen());
        }
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
fn retrying_a_failed_reader_replaces_the_still_open_block() {
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
    1) echo block:11111111-1111-1111-1111-111111111111 ;;
    2) exit 1 ;;
    *) echo block:22222222-2222-2222-2222-222222222222 ;;
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
    assert!(retry.contains(r#""previous":"block:11111111-1111-1111-1111-111111111111""#), "{retry}");
}

#[test]
fn a_slow_open_reflows_its_loading_frame_without_waiting_for_git() {
    let project = Project::new();
    fs::write(project.0.join("project/page.txt"), "data").unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    project.slow_git(&mut command);
    let mut terminal = Terminal::sized(command.arg("outline"), 80, 1);
    terminal.until("Reading…");
    assert!(!terminal.output().contains("project"));
    terminal.bytes.clear();
    let size = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: The PTY descriptor and the size record are both live.
    assert_eq!(unsafe { libc::ioctl(terminal.slave.as_raw_fd(), libc::TIOCSWINSZ as _, &raw const size) }, 0);
    // SAFETY: The PTY is not the child's controlling terminal; signal only this child.
    assert_eq!(unsafe { libc::kill(i32::try_from(terminal.child.id()).unwrap(), libc::SIGWINCH) }, 0);
    terminal.until("project");
    terminal.until("Reading…");
    assert!(!terminal.output().contains("page.txt"));
    assert!(!terminal.restored());
    fs::write(project.0.join("go"), "").unwrap();
    terminal.until("page.txt");
}

#[test]
fn unavailable_git_is_distinguished_from_a_plain_folder() {
    let project = Project::new();
    let init = project
        .command("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .args(["init", "-q", "--template="])
        .output()
        .unwrap();
    assert!(init.status.success(), "{}", String::from_utf8_lossy(&init.stderr));
    fs::write(project.0.join("project/page.txt"), "local-file-contents\n").unwrap();

    let empty_path = project.0.join("bin");
    fs::create_dir(&empty_path).unwrap();
    let milo = |args: &[&str]| {
        let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
        command.env("PATH", &empty_path).args(args);
        command
    };
    let note = "Git could not be run.";
    let report = milo(&["doctor"]).output().unwrap();
    assert!(report.status.success(), "{}", String::from_utf8_lossy(&report.stderr));
    let worktree_note = format!("Worktree: {note}");
    assert!(String::from_utf8_lossy(&report.stdout).lines().any(|line| line == worktree_note));

    let at_commit = milo(&["view", "page.txt", "--commit", "abcd"]).output().unwrap();
    assert!(!at_commit.status.success());
    assert!(String::from_utf8_lossy(&at_commit.stderr).contains(note));

    Terminal::run(&mut milo(&["history"])).until(note);
    let mut outline = Terminal::run(&mut milo(&["outline"]));
    outline.until(note);
    outline.until("page.txt");
    let mut reader = Terminal::run(&mut milo(&["view", "page.txt"]));
    reader.until(note);
    reader.until("local-file-contents");
}

#[test]
fn a_git_that_cannot_give_whole_paths_still_names_a_project_by_its_folder() {
    let project = Project::new();
    project.git(&["init", "-q", "--template="]);
    fs::create_dir(project.0.join("project/deep")).unwrap();
    let real = PathBuf::from(project.git(&["--exec-path"]).trim_end()).join("git");
    // Git before 2.31 does not know `--path-format`. Like any option `rev-parse` does not know, it is
    // printed back as it was given, and the shared folder is then named from where Git was asked.
    let older = "for word do\n  shift\n  [ \"$word\" = --path-format=absolute ] && word=--unknown-to-this-git\n  \
                 set -- \"$@\" \"$word\"\ndone\nexec \"$REAL_GIT\" \"$@\"\n";
    for folder in ["project", "project/deep"] {
        let mut milo = project.command(env!("CARGO_BIN_EXE_milo"));
        project.stand_in_git(&mut milo, older);
        milo.env("REAL_GIT", &real).args(["tab-title", "name"]).arg(project.0.join(folder));
        let named = milo.output().unwrap();
        assert!(named.status.success(), "{}", String::from_utf8_lossy(&named.stderr));
        assert_eq!(String::from_utf8(named.stdout).unwrap(), "project\n", "asked in {folder}");
    }
}

#[test]
fn outline_search_distinguishes_closed_paths_and_escape_restores_the_tree() {
    let project = Project::new();
    let root = project.0.join("project");
    fs::create_dir_all(root.join("docs/chapter")).unwrap();
    fs::create_dir_all(root.join("src/reference")).unwrap();
    fs::write(root.join("docs/chapter/guide.md"), "first guide\n").unwrap();
    fs::write(root.join("src/reference/guide.md"), "second guide\n").unwrap();
    for i in 0..20 {
        fs::write(root.join(format!("note-{i:02}.txt")), "").unwrap();
    }
    fs::write(root.join("zz-target.txt"), "previous selection\n").unwrap();
    fs::write(root.join("zzz-after.txt"), "").unwrap();
    project.git(&["init", "-q", "--template="]);
    project.git(&["add", "."]);
    project.git(&["commit", "-qm", "fixture"]);

    let mut tty = Terminal::sized(project.command(env!("CARGO_BIN_EXE_milo")).arg("outline"), 80, 12);
    tty.shows("all committed");
    let visible = |tty: &Terminal| tty.screen()[1..11].iter().map(|s| s.trim_end().to_owned()).collect::<Vec<_>>();
    tty.master.write_all(b"\rGk").unwrap();
    tty.shows("zzz-after.txt");
    let before = visible(&tty);
    assert!(before.iter().any(|s| s.contains("zz-target.txt")));
    tty.master.write_all(b"/absent\r").unwrap();
    tty.shows("No matches");
    tty.master.write_all(b"\x1b").unwrap();
    tty.shows("zzz-after.txt");
    assert_eq!(visible(&tty), before);

    tty.master.write_all(b"/Guide.md").unwrap();
    tty.shows("docs/chapter/guide.md");
    tty.shows("src/reference/guide.md");
    tty.master.write_all(b"\x1b[B\r").unwrap();
    tty.shows("second guide");
    tty.master.write_all(b"q").unwrap();
    tty.shows("zzz-after.txt");
    tty.master.write_all(b"\r").unwrap();
    tty.shows("previous selection");
    tty.master.write_all(b"qq").unwrap();
    assert!(tty.wait_for_exit().success());
    assert!(tty.restored());
}

#[test]
fn outline_search_pastes_utf8_in_a_plain_folder_and_takes_touch_controls() {
    let project = Project::new();
    let root = project.0.join("project");
    fs::create_dir(root.join("buried")).unwrap();
    fs::write(root.join("buried/ω.txt"), "found it\n").unwrap();
    fs::write(root.join("buried/.DS_Store"), "").unwrap();
    let mut tty = Terminal::sized(project.command(env!("CARGO_BIN_EXE_milo")).arg("outline"), 22, 12);
    tty.shows("/ find");
    tty.click(2, 12, "");
    tty.shows("Find files");
    tty.shows("buried/ω.txt");
    assert!(!tty.screen().iter().any(|row| row.contains(".DS_Store")));
    tty.master.write_all("ω界".as_bytes()).unwrap();
    tty.shows("No matches");
    tty.master.write_all(b"\x7f").unwrap();
    tty.shows("buried/ω.txt");
    tty.master.write_all("\x15\x1b[200~BURIED/Ω.TXT\nq\x1b[201~".as_bytes()).unwrap();
    tty.shows("BURIED/Ω.TXT");
    tty.shows("buried/ω.txt");

    let size = libc::winsize { ws_row: 5, ws_col: 34, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: The slave is open; ioctl reads from the live size record.
    assert_eq!(unsafe { libc::ioctl(tty.slave.as_raw_fd(), libc::TIOCSWINSZ as _, &raw const size) }, 0);
    let pid = i32::try_from(tty.child.id()).unwrap();
    // SAFETY: The PTY is not the child's controlling terminal; signal only this child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGWINCH) }, 0);
    tty.shows("Esc back");
    tty.shows("Enter open");
    tty.click(26, 1, "");
    tty.hides("Esc back");
    tty.master.write_all(b"/").unwrap();
    tty.shows("buried/ω.txt");
    tty.shows("Enter open");
    tty.click(2, 5, "");
    tty.shows("found it");
    tty.master.write_all(b"qq").unwrap();
    assert!(tty.wait_for_exit().success());
    assert!(tty.restored());
}

#[test]
fn git_panes_wait_for_a_persistent_index_lock_without_removing_it() {
    let project = Project::new();
    let root = project.0.join("project");
    project.git(&["init"]);
    fs::write(root.join("page.txt"), "before\n").unwrap();
    project.git(&["add", "page.txt"]);
    project.git(&["commit", "-m", "Baseline"]);
    let later = project.git(&["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "UnlockedCommit"]);
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
    project.git(&["update-ref", "HEAD", later.trim()]);
    fs::remove_file(lock).unwrap();
    for (terminal, updated) in &mut panes {
        terminal.until(updated);
        terminal.master.write_all(b"q").unwrap();
        assert!(terminal.wait_for_exit().success());
    }
}

/// An outline on two changed files and one clean one, whose Git stops at
/// `status` for as long as the file `hold` is in the project's home.
fn outline_with_a_git_that_can_be_held(project: &Project) -> Terminal {
    let root = project.0.join("project");
    project.git(&["init"]);
    for name in ["clean.txt", "first.txt", "second.txt"] {
        fs::write(root.join(name), "before\n").unwrap();
    }
    project.git(&["add", "."]);
    project.git(&["commit", "-m", "Baseline"]);
    for name in ["first.txt", "second.txt"] {
        fs::write(root.join(name), "after\n").unwrap();
    }
    let real = Path::new(project.git(&["--exec-path"]).trim()).join("git");
    let script = format!(
        "case \" $* \" in *\" status \"*)\n  if [ -e \"$HOME/hold\" ]; then\n    : > \"$HOME/held\"\n    \
         while [ -e \"$HOME/hold\" ]; do sleep 0.02; done\n  fi;;\nesac\nexec '{}' \"$@\"\n",
        real.display()
    );
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    project.stand_in_git(&mut command, &script);
    let mut terminal = Terminal::sized(command.arg("outline"), 80, 12);
    terminal.shows("clean.txt");
    terminal
}

/// Changes the project and waits until the outline's Git is stopped reading it.
fn hold_git_at_a_change(project: &Project) {
    fs::write(project.0.join("hold"), "").unwrap();
    fs::write(project.0.join("project/later.txt"), "new file\n").unwrap();
    let until = Instant::now() + TEST_WAIT;
    while !project.0.join("held").exists() {
        assert!(Instant::now() < until, "the outline did not ask Git about the change");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_outline_answers_keys_while_git_is_still_reading_the_project() {
    let project = Project::new();
    let mut terminal = outline_with_a_git_that_can_be_held(&project);
    hold_git_at_a_change(&project);

    terminal.master.write_all(b"c").unwrap();
    terminal.hides("clean.txt");
    // The key was answered from what Git last said: not after Git was given up on,
    // and not from half of its next answer.
    assert!(project.0.join("hold").exists());
    let screen = terminal.screen();
    assert!(!screen.iter().any(|row| row.contains("later.txt") || row.contains("git did not answer")), "{screen:#?}");

    fs::remove_file(project.0.join("hold")).unwrap();
    terminal.shows("later.txt");
    terminal.master.write_all(b"q").unwrap();
    assert!(terminal.wait_for_exit().success());
}

#[test]
fn search_stays_responsive_during_refresh_and_opens_the_updated_file_for_editing() {
    let project = Project::new();
    let mut terminal = outline_with_a_git_that_can_be_held(&project);
    hold_git_at_a_change(&project);

    let asked = Instant::now();
    terminal.master.write_all(b"/later").unwrap();
    terminal.shows("No matches");
    terminal.master.write_all(b"\x15FIRST").unwrap();
    terminal.shows("FIRST");
    terminal.shows("first.txt");
    assert!(asked.elapsed() < Duration::from_secs(10), "waited {:?} for Git", asked.elapsed());
    assert!(!terminal.screen().iter().any(|row| row.contains("second.txt")));

    fs::remove_file(project.0.join("hold")).unwrap();
    terminal.master.write_all(b"\x1b").unwrap();
    terminal.shows("later.txt");
    terminal.master.write_all(b"/later\r").unwrap();
    terminal.shows("new file");
    terminal.master.write_all(b"e").unwrap();
    terminal.shows("^S save");
    terminal.master.write_all(b"draft ").unwrap();
    terminal.shows("draft new file");

    let file = project.0.join("project/later.txt");
    fs::write(&file, "disk version\n").unwrap();
    terminal.master.write_all(b"\x13").unwrap();
    terminal.shows("c compare");
    terminal.master.write_all(b"c").unwrap();
    terminal.shows("- Disk  + Draft");
    terminal.shows("disk version");
    terminal.shows("draft new file");
    assert_eq!(fs::read_to_string(&file).unwrap(), "disk version\n");
    terminal.master.write_all(b"\x1b").unwrap();
    terminal.shows("o overwrite");
    terminal.master.write_all(b"o").unwrap();
    terminal.shows("Saved");
    assert_eq!(fs::read_to_string(&file).unwrap(), "draft new file\n");

    terminal.master.write_all(b"\x1b").unwrap();
    terminal.shows("e edit");
    terminal.master.write_all(b"qq").unwrap();
    assert!(terminal.wait_for_exit().success());
    assert!(terminal.restored());
}

#[test]
fn the_outline_quits_without_waiting_for_a_git_that_is_still_reading() {
    let project = Project::new();
    let mut terminal = outline_with_a_git_that_can_be_held(&project);
    hold_git_at_a_change(&project);

    let asked = Instant::now();
    terminal.master.write_all(b"q").unwrap();
    assert!(terminal.wait_for_exit().success());
    // A pane that waits gives Git fifteen seconds before it reads the key.
    assert!(asked.elapsed() < Duration::from_secs(10), "waited {:?} for Git", asked.elapsed());
    assert!(terminal.restored());
    assert!(project.0.join("hold").exists());
    assert!(!project.0.join("project/.git/index.lock").exists());
}

fn history_fixture(project: &Project, subject: &str) {
    project.git(&["init", "-q", "--template="]);
    fs::write(project.0.join("project/page.txt"), "before\n").unwrap();
    project.git(&["add", "page.txt"]);
    project.git(&["commit", "-qm", subject]);
}

fn hold_history_listing(project: &Project, command: &mut Command) -> File {
    let fifo = project.0.join("listing-lifetime");
    assert!(project.command("mkfifo").arg(&fifo).status().unwrap().success());
    let lifetime = File::options().read(true).custom_flags(libc::O_NONBLOCK).open(fifo).unwrap();
    let git = Path::new(project.git(&["--exec-path"]).trim()).join("git");
    command.env("REAL_GIT", git);
    project.stand_in_git(
        command,
        r#"case " $* " in
  *" diff-tree "*" --numstat "*)
    [ "$CLOSE_LISTING_STDOUT" = yes ] && exec 1>&-
    exec 3>"$HOME/listing-lifetime"
    (while [ -d "$HOME" ]; do sleep 0.02; done) &
    exec 3>&-
    echo "$$" > "$HOME/listing-pid"
    wait
    exit 1 ;;
esac
exec "$REAL_GIT" "$@"
"#,
    );
    lifetime
}

fn open_held_history_listing(project: &Project, terminal: &mut Terminal, lifetime: &mut File) -> libc::pid_t {
    terminal.master.write_all(b"\r").unwrap();
    terminal.shows("Reading files…");
    let marker = project.0.join("listing-pid");
    let until = Instant::now() + TEST_WAIT;
    let pid = loop {
        if let Some(pid) = fs::read_to_string(&marker).ok().and_then(|s| s.strip_suffix('\n')?.parse().ok()) {
            break pid;
        }
        assert!(Instant::now() < until, "History did not start the held listing");
        std::thread::sleep(Duration::from_millis(10));
    };
    fs::remove_file(marker).unwrap();
    assert!(pid > 1);
    // Only the Git helper owns the write end by the time this marker appears.
    assert_eq!(lifetime.read(&mut [0]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    pid
}

fn assert_history_listing_stopped(pid: libc::pid_t, lifetime: &mut File) {
    // SAFETY: Signal zero checks whether this test's Git still exists without sending a signal.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "History did not reap Git");
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match lifetime.read(&mut [0]) {
            Ok(0) => break,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < until, "History left a Git helper running");
                std::thread::sleep(Duration::from_millis(10));
            }
            other => panic!("unexpected Git helper pipe result: {other:?}"),
        }
    }
}

#[test]
fn history_cancels_and_reaps_a_listing_on_quit_after_git_closes_stdout() {
    let project = Project::new();
    history_fixture(&project, "HeldCommit");
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    command.arg("history").env("CLOSE_LISTING_STDOUT", "yes");
    let mut lifetime = hold_history_listing(&project, &mut command);
    let mut terminal = Terminal::run(&mut command);
    terminal.shows("HeldCommit");
    let pid = open_held_history_listing(&project, &mut terminal, &mut lifetime);

    let asked = Instant::now();
    terminal.master.write_all(b"q").unwrap();
    assert!(terminal.wait_for_exit().success());
    assert!(asked.elapsed() < Duration::from_secs(10), "waited {:?} for Git", asked.elapsed());
    assert!(terminal.restored());
    assert_history_listing_stopped(pid, &mut lifetime);
}

#[test]
fn history_cancels_and_reaps_listings_on_repeated_project_switches() {
    use std::fmt::Write as _;

    let first = Project::new();
    let second = Project::new();
    let titles = ["FirstCommit", "SecondCommit"];
    history_fixture(&first, titles[0]);
    history_fixture(&second, titles[1]);
    let feed_path = first.0.join("cwd-feed");
    assert!(first.command("mkfifo").arg(&feed_path).status().unwrap().success());
    let rex = first.0.join("rex");
    fs::write(&rex, "#!/bin/sh\nexec cat \"$HOME/cwd-feed\"\n").unwrap();
    fs::set_permissions(rex, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = first.command(env!("CARGO_BIN_EXE_milo"));
    command
        .args(["history", "--shell", "block:11111111-1111-1111-1111-111111111111"])
        .env("REX_SESSION", "test")
        .env("REX_BLOCK", "block:22222222-2222-2222-2222-222222222222")
        .env_remove("CLOSE_LISTING_STDOUT");
    let mut lifetime = hold_history_listing(&first, &mut command);
    let mut terminal = Terminal::run(&mut command);
    terminal.shows(titles[0]);
    let until = Instant::now() + TEST_WAIT;
    let mut feed = loop {
        match File::options().write(true).custom_flags(libc::O_NONBLOCK).open(&feed_path) {
            Ok(writer) => break writer,
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                assert!(Instant::now() < until, "History did not start the shell observer");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("cannot open shell observer feed: {e}"),
        }
    };

    for next in [1, 0, 1] {
        let pid = open_held_history_listing(&first, &mut terminal, &mut lifetime);
        let root = [&first, &second][next].0.join("project");
        let mut report = String::from("milo-cwd ");
        for byte in root.as_os_str().as_encoded_bytes() {
            write!(report, "{byte:02x}").unwrap();
        }
        let asked = Instant::now();
        writeln!(feed, "{report}").unwrap();
        terminal.shows(titles[next]);
        assert!(asked.elapsed() < Duration::from_secs(10), "waited {:?} for Git", asked.elapsed());
        assert_history_listing_stopped(pid, &mut lifetime);
    }
    terminal.master.write_all(b"q").unwrap();
    assert!(terminal.wait_for_exit().success());
    assert!(terminal.restored());
}

#[test]
fn both_panes_promote_a_plain_folder_while_idle_and_load_worktree_choices() {
    let project = Project::new();
    let root = project.0.join("project");
    let milo = env!("CARGO_BIN_EXE_milo");
    fs::create_dir(root.join("saved")).unwrap();
    fs::write(root.join("saved/kept.txt"), "keep\n").unwrap();
    fs::write(root.join("page.txt"), "before\n").unwrap();
    project.git(&["init", "-q", "--template="]);
    project.git(&["add", "."]);
    project.git(&["commit", "-qm", "FirstGitCommit"]);

    let config = project.0.join("config/milo");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("preferences.conf"), "changes_only = true\n").unwrap();
    let mut seed = Terminal::run(project.command(milo).arg("outline"));
    seed.until("c all files");
    seed.master.write_all(b"c").unwrap();
    seed.until("changed only");
    seed.master.write_all(b"q").unwrap();
    assert!(seed.wait_for_exit().success());

    // The preference created by the pane identifies its matching fold store.
    let prefs = fs::read_dir(config.join("worktrees")).unwrap().next().unwrap().unwrap().path();
    let folds = project.0.join("state/milo/worktrees").join(prefs.with_extension("folds").file_name().unwrap());
    fs::create_dir_all(folds.parent().unwrap()).unwrap();
    fs::write(folds, "+73617665642f\n").unwrap(); // Open "saved/".

    // Make the folder plain again without losing the saved settings or tracked files.
    let metadata = project.0.join("metadata");
    fs::rename(root.join(".git"), &metadata).unwrap();
    fs::remove_file(root.join("page.txt")).unwrap();
    let mut outline = Terminal::run(project.command(milo).arg("outline"));
    let mut history = Terminal::run(project.command(milo).arg("history"));
    outline.until("saved");
    assert!(!outline.output().contains("kept.txt"));
    history.until("Not a Git project.");

    fs::rename(metadata, root.join(".git")).unwrap();
    outline.until("to commit");
    outline.until("changed only");
    outline.until("kept.txt");
    history.until("FirstGitCommit");
    for terminal in [&mut outline, &mut history] {
        terminal.master.write_all(b"q").unwrap();
        assert!(terminal.wait_for_exit().success());
    }
}

#[test]
fn follow_shell_promotion_applies_worktree_preferences_before_a_cwd_update() {
    let project = Project::new();
    let root = project.0.join("project");
    let observer_pid = project.0.join("observer");
    let rex = project.0.join("rex");
    fs::write(
        &rex,
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$HOME/observer\"\nprintf 'milo-cwd -\\n'\n\
         while [ -d \"$HOME\" ]; do sleep 0.02; done\n",
    )
    .unwrap();
    fs::set_permissions(rex, fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![project.0.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let path = std::env::join_paths(paths).unwrap();

    let milo = env!("CARGO_BIN_EXE_milo");
    project.git(&["init", "-q", "--template="]);
    fs::write(root.join("page.txt"), "content\n").unwrap();
    project.git(&["add", "page.txt"]);
    project.git(&["commit", "-qm", "FirstGitCommit"]);

    // Let Outline determine the worktree's preferences path.
    let mut seed = Terminal::run(project.command(milo).arg("outline"));
    seed.until("changed only");
    seed.master.write_all(b"cq").unwrap();
    assert!(seed.wait_for_exit().success());
    let worktrees = project.0.join("config/milo/worktrees");
    let prefs = fs::read_dir(worktrees).unwrap().next().unwrap().unwrap().path();

    let metadata = project.0.join("metadata");
    for (pane, prefs_text, promoted, observer_running) in [
        ("outline", "project_mode = pin\n", "all committed", false),
        ("history", "project_mode = broken\n", "worktree prefs", true),
    ] {
        fs::write(&prefs, prefs_text).unwrap();
        fs::rename(root.join(".git"), &metadata).unwrap();
        let mut command = project.command(milo);
        let anchor = "block:11111111-1111-1111-1111-111111111111";
        command
            .args([pane, "--shell", anchor])
            .env("PATH", &path)
            .env("REX_SESSION", "test")
            .env("REX_BLOCK", "block:22222222-2222-2222-2222-222222222222");
        let mut terminal = Terminal::run(&mut command);
        terminal.until("Waiting for shell");
        let pid: libc::pid_t = fs::read_to_string(&observer_pid).unwrap().trim().parse().unwrap();
        fs::rename(&metadata, root.join(".git")).unwrap();
        terminal.until(promoted);
        // SAFETY: Signal zero checks whether this test's observer exists without sending a signal.
        assert_eq!(unsafe { libc::kill(pid, 0) } == 0, observer_running, "{pane}");
        if observer_running {
            // SAFETY: pid is the test observer whose identity was checked above.
            assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
            terminal.until("tracking unavailable");
        }
        terminal.master.write_all(b"q").unwrap();
        assert!(terminal.wait_for_exit().success());
        fs::remove_file(&observer_pid).unwrap();
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

fn wall_reader(project: &Project, cols: u16, rows: u16) -> Terminal {
    let wall = std::str::from_utf8(&[b'X'; 600]).unwrap();
    let mut file = String::with_capacity(400 * 601);
    for row in 0..400 {
        file.push_str(match row {
            6 => "PASTE-READY",
            201 => "ONE-LINE-UP",
            _ => wall,
        });
        file.push('\n');
    }
    fs::write(project.0.join("project/wall.txt"), file).unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    project.slow_git(&mut command);
    fs::write(project.0.join("go"), "").unwrap();
    let mut terminal = Terminal::sized(command.args(["view", "wall.txt"]), cols, rows);
    // Wait for the file frame, not the small frame drawn while it opens.
    terminal.until("XXXXXXXXXXXXXXXX");
    terminal
}

#[cfg(target_os = "macos")]
#[test]
fn preview_error_exits_when_its_terminal_cannot_display_the_notice() {
    let project = Project::new();
    let mut command = project.command("/bin/sh");
    command.args(["-c", "while [ ! -f go ]; do sleep .05; done; exec \"$@\"", "anchor", env!("CARGO_BIN_EXE_milo"),
        "preview-anchor", "http://localhost:3000", "block:11111111-1111-4111-8111-111111111111",
        "window:22222222-2222-4222-8222-222222222222", "client:33333333-3333-4333-8333-333333333333", "100", "42", "auto"]);
    let mut terminal = Terminal::run(&mut command);
    let fd = terminal.slave.as_raw_fd();
    // SAFETY: This is an open private PTY. The child waits on the go file
    // while the test fills its output queue, and inherits blocking stderr.
    assert_eq!(unsafe { libc::tcflow(fd, libc::TCOOFF) }, 0);
    // SAFETY: Read and temporarily change file status flags on the open slave.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert_ne!(flags, -1);
    // SAFETY: The descriptor remains live.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) }, 0);
    let blocked = (0..1024).any(|_| terminal.slave.write(&[0; 1024]).is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock));
    assert!(blocked, "private terminal output never saturated");
    // SAFETY: Restore the original flags before launching the preview process.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }, 0);
    fs::write(project.0.join("project/go"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while terminal.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "error reporting blocked on a stopped terminal");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(terminal.child.wait().unwrap().code(), Some(1));
    assert!(terminal.restored());
}

#[test]
fn sigterm_restores_the_terminal_even_when_its_output_queue_is_full() {
    let project = Project::new();
    let mut terminal = wall_reader(&project, 600, 200);
    let pid = i32::try_from(terminal.child.id()).unwrap();
    // SAFETY: This is our own, still-owned child, not a process group.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    assert_eq!(terminal.wait_for_exit().code(), Some(1));
    assert!(terminal.restored());
    terminal.until("\x1b[?1049l");
}

#[test]
fn keyboard_ctrl_c_restores_the_terminal_without_draining_a_blocked_frame() {
    let project = Project::new();
    let mut terminal = wall_reader(&project, 600, 200);
    assert!(!terminal.restored());
    terminal.master.write_all(b"\x03").unwrap();
    assert!(terminal.wait_for_exit().success());
    assert!(terminal.restored());
    terminal.until("\x1b[?1049l");
}

#[test]
fn a_paste_spanning_a_blocked_frame_cannot_quit_or_reorder_keys() {
    let project = Project::new();
    let mut terminal = wall_reader(&project, 80, 8);
    // Scroll one line and start a paste while the terminal is responsive.
    terminal.master.write_all(b"j\x1b[200~").unwrap();
    terminal.until("PASTE-READY");
    terminal.bytes.clear();

    // Enlarge the viewport so it paints more than can fit in the PTY queue.
    let size = libc::winsize { ws_row: 200, ws_col: 600, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: The slave is open; ioctl reads from the live size record.
    assert_eq!(unsafe { libc::ioctl(terminal.slave.as_raw_fd(), libc::TIOCSWINSZ as _, &raw const size) }, 0);
    let pid = i32::try_from(terminal.child.id()).unwrap();
    // SAFETY: The PTY is not the child's controlling terminal; signal only this child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGWINCH) }, 0);
    terminal.until("XXXXXXXXXXXXXXXX");
    terminal.master.write_all(b"\x03q\x1b[201~Gk").unwrap();

    // Only G then k, in order, reaches the line just above the last page.
    terminal.until("ONE-LINE-UP");
    terminal.master.write_all(b"q").unwrap();
    terminal.until("\x1b[?1049l");
    assert!(terminal.wait_for_exit().success());
    assert!(terminal.restored());
}

#[test]
fn the_reader_renders_invisible_git_edits_in_working_files_and_commits() {
    let project = Project::new();
    project.git(&["init", "-q"]);
    project.git(&["config", "core.autocrlf", "false"]);
    let path = project.0.join("project/notes.txt");
    fs::write(&path, "\tindent\nsame a\nline ending\r\nsame b\ncafe!\nsame c\nhidden!\nsame d\ncode=1\n").unwrap();
    project.git(&["add", "."]);
    project.git(&["commit", "-qm", "before"]);
    fs::write(
        &path,
        "    indent\nsame a\nline ending\nsame b\ncafe\u{301}!\nsame c\nhidden\u{200b}!\nsame d\ncode=2\n",
    )
    .unwrap();

    let check = |args: &[&str]| {
        let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
        let mut terminal = Terminal::run(command.args(args));
        terminal.until("\x1b[0;7;32m2");
        let out = terminal.output();
        for text in [
            " [tab]",
            " [4 spaces]",
            " [CRLF]",
            " [LF]",
            "\x1b[0;7;31me\x1b",
            "\x1b[0;7;32me\u{301}\x1b",
            " [combining acute U+0301]",
            "\x1b[0;7;32mn\x1b",
            " [zero-width space U+200B]",
        ] {
            assert!(out.contains(text), "{text:?} missing from {out:?}");
        }
        terminal.master.write_all(b"q").unwrap();
        assert!(terminal.wait_for_exit().success());
    };
    check(&["view", "notes.txt"]);
    project.git(&["add", "."]);
    project.git(&["commit", "-qm", "after"]);
    let commit = project.git(&["rev-parse", "HEAD"]);
    check(&["view", "notes.txt", "--commit", commit.trim()]);
}

#[test]
fn a_picture_returns_after_resize_without_further_input() {
    const PLACE: &str = "\x1b_Ga=p,";
    const UPLOAD: &str = "\x1b_Ga=t,";
    let project = Project::new();
    fs::write(project.0.join("project/resize.png"), include_bytes!("../src/usage/marks/codex.png")).unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    let mut terminal = Terminal::run(command.args(["view", "resize.png"]));
    terminal.until("\x1b_Gi=31,s=1,v=1,a=q");
    terminal.master.write_all(b"\x1b_Gi=31;OK\x1b\\").unwrap();
    terminal.until(PLACE);
    // Let the initial 120 ms watcher recheck finish before exercising the idle deadline.
    std::thread::sleep(Duration::from_millis(200));
    terminal.bytes.clear();

    let size = libc::winsize { ws_row: 24, ws_col: 72, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: The descriptor and the size record both remain valid for ioctl.
    assert_eq!(unsafe { libc::ioctl(terminal.slave.as_raw_fd(), libc::TIOCSWINSZ as _, &raw const size) }, 0);
    let pid = i32::try_from(terminal.child.id()).unwrap();
    // This PTY is not the child's controlling terminal, so notify it explicitly.
    // SAFETY: This is the test's still-running child, not a process group.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGWINCH) }, 0);
    // The fallback file check is no sooner than every two seconds.
    let until = Instant::now() + Duration::from_millis(1500);
    while !terminal.output().contains(PLACE) {
        assert!(terminal.read(until), "picture did not return while idle: {:?}", terminal.output());
    }
    let output = terminal.output();
    let (_, resized) = output.split_once("\x1b[2J").expect("resize must erase the display");
    let (text, settled) = resized.split_once("\x1b[?2026l").expect("resize must flush the text frame");
    assert!(text.contains("resize.png") && !text.contains(UPLOAD) && !text.contains(PLACE), "{text:?}");
    assert_eq!(settled.matches(UPLOAD).count(), 1);
    assert!(settled.contains(PLACE));
    terminal.master.write_all(b"q").unwrap();
    assert!(terminal.wait_for_exit().success());
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
        (&["usage", "--help"][..], "Usage: milo usage [--menu-stream]"),
        (&["setup", "--help"][..], "Usage: milo setup"),
        (&["view", "--help"][..], "Usage: milo view FILE [--commit ID | --edit]"),
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
        (&["view"][..], "milo: missing file; use milo view FILE [--commit ID | --edit]\n"),
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), message);
    }
}

#[test]
fn a_pane_started_without_a_terminal_says_what_works_without_one() {
    let project = Project::new();
    fs::write(project.0.join("project/page.txt"), "text\n").unwrap();
    const NEEDS: &str = "milo: cannot use this terminal: standard output is not a terminal; \
                         a pane needs an interactive terminal. milo doctor and milo COMMAND --help print without one";
    for args in [&["outline"][..], &["history"], &["usage"], &["view", "page.txt"], &["."]] {
        let output = project.command(env!("CARGO_BIN_EXE_milo")).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), format!("{NEEDS}\n"), "{args:?}");
    }
    // Inside Rex there is a way to open the pane from a shell that has no terminal.
    let output =
        project.command(env!("CARGO_BIN_EXE_milo")).env("REX_SESSION", "test").arg("outline").output().unwrap();
    let in_rex = format!("{NEEDS}, and rex run -s \"$REX_SESSION\" -- milo COMMAND opens a pane in a Rex tab\n");
    assert_eq!((output.status.code(), String::from_utf8(output.stderr).unwrap()), (Some(1), in_rex));
    // A mistake in the command is still reported as that mistake.
    let output = project.command(env!("CARGO_BIN_EXE_milo")).args(["outline", "missing"]).output().unwrap();
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "milo: missing is not a directory\n");
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
fn uninstall_keeps_the_executable_when_the_owned_claude_status_line_cannot_be_removed() {
    let project = Project::new();
    let bin = project.0.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let milo = bin.join("milo");
    fs::copy(env!("CARGO_BIN_EXE_milo"), &milo).unwrap();
    let command = || {
        let mut command = project.command(&milo);
        command.env_remove("REX_CONFIG");
        command
    };
    // A sibling test's new process can hold the copy open for writing for a moment.
    let until = Instant::now() + TEST_WAIT;
    let skipped = loop {
        match command().args(["setup", "--skip-onboarding"]).status() {
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(2));
            }
            status => break status.unwrap(),
        }
    };
    assert!(skipped.success());
    let settings = project.0.join(".claude/settings.json");
    fs::create_dir(settings.parent().unwrap()).unwrap();
    let invalid = br#"{"statusLine":{"type":"command","command":"milo claude-status"}"#;
    fs::write(&settings, invalid).unwrap();
    let note = project.0.join("state/milo/claude-status-added");
    fs::write(&note, "created\n").unwrap();

    let result = command().args(["setup", "--remove"]).output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("Could not remove Milo's Claude Code status line"));

    let mut home = Terminal::run(&mut command());
    home.until("Uninstall");
    home.master.write_all(b"\x1b[F\r").unwrap();
    home.until("Cancel");
    home.master.write_all(b"\x1b[B\r").unwrap();
    // The incremental renderer may put cursor moves between words in the report.
    home.until("executable");
    home.until("removed.");
    assert!(milo.is_file());
    assert!(note.is_file());
    assert_eq!(fs::read(&settings).unwrap(), invalid);
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
    fs::write(config.join("preferences.conf"), "changes_only = true\nusage_codex = always\n").unwrap();

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

#[test]
fn repeat_clicks_do_not_cross_between_home_and_settings() {
    // The renderer positions text using CUP and changes its style using SGR.
    // Every glyph on these menu and control lines occupies one cell.
    fn painted<'a>(output: &'a str, label: &'a str) -> impl Iterator<Item = (u16, std::ops::Range<u16>)> + 'a {
        let (mut row, mut col) = (1, 1);
        output.split("\x1b[").skip(1).filter_map(move |run| {
            let at = run.find(|ch| ('@'..='~').contains(&ch))?;
            let (args, tail) = run.split_at(at);
            let (command, text) = tail.split_at(1);
            match command {
                "H" => {
                    let (y, x) = args.split_once(';')?;
                    row = y.parse().ok()?;
                    col = x.parse().ok()?;
                }
                "m" => {}
                _ => return None,
            }
            let text = text.split('\x1b').next().unwrap();
            let start = col;
            col += u16::try_from(text.chars().count()).unwrap();
            let offset = text.find(label)?;
            let left = start + u16::try_from(text[..offset].chars().count()).unwrap();
            Some((row, left..left + u16::try_from(label.len()).unwrap()))
        })
    }

    fn overlap(a: &(u16, std::ops::Range<u16>), b: &(u16, std::ops::Range<u16>)) -> Option<u16> {
        let left = a.1.start.max(b.1.start);
        (a.0 == b.0 && left < a.1.end.min(b.1.end)).then_some(left)
    }

    let project = Project::new();
    let milo = env!("CARGO_BIN_EXE_milo");
    assert!(project.command(milo).args(["setup", "--skip-onboarding"]).status().unwrap().success());
    let version = concat!("v", env!("CARGO_PKG_VERSION"));

    for (cols, rows) in [(40, 16), (28, 12)] {
        let mut tty = Terminal::sized(&mut project.command(milo), cols, rows);
        tty.until("q quit");
        let (settings, home_quit) = {
            let home = tty.output();
            (painted(&home, "Terminal").next().unwrap(), painted(&home, "q quit").next().unwrap())
        };

        // A fresh Settings frame shows which cells would really change a draft
        // if the repeat were delivered instead of suppressed.
        let mut probe = Terminal::sized(project.command(milo).arg("setup"), cols, rows);
        probe.until("q quit");
        let (col, setup_quit) = {
            let setup = probe.output();
            // Plain spaces can separate two cursor writes, so match words.
            let col = ["Project", "Follow", "Track", "edits", "Outline", "New", "commits", "Tab", "titles", "On", "Off"]
                .into_iter()
                .flat_map(|word| painted(&setup, word))
                .find_map(|target| overlap(&settings, &target))
                .unwrap_or_else(|| panic!("no actionable Settings overlap with {settings:?} at {cols}×{rows}: {setup:?}"));
            (col, painted(&setup, "q quit").next().unwrap())
        };
        let row = settings.0;
        probe.click(col, row, "");
        probe.until("Unsaved changes");
        probe.master.write_all(b"\x03").unwrap();
        assert!(probe.wait_for_exit().success());

        let quit_overlap = overlap(&setup_quit, &home_quit);
        if cols == 28 {
            assert!(quit_overlap.is_some(), "the repeat must hit both quit buttons in the compact pane");
        }
        let quit_col = quit_overlap.unwrap_or(setup_quit.1.start);
        let quit_row = setup_quit.0;
        tty.bytes.clear();
        tty.click(col, row, "");
        tty.until("Workflow");
        tty.bytes.clear();
        tty.click(col, row, "2");
        tty.until("Pulse");
        assert!(!tty.output().contains("Unsaved"), "the repeat must not change the draft");

        // The quit labels overlap at 28 columns. A double-click must return
        // to Home without quitting it, and the help key must still work.
        tty.bytes.clear();
        tty.click(quit_col, quit_row, "");
        tty.until(version);
        tty.bytes.clear();
        tty.click(quit_col, quit_row, "?");
        tty.until("Milo home");

        if cols == 40 {
            tty.bytes.clear();
            tty.master.write_all(b"q").unwrap();
            tty.until("Terminal");
            let (row, span) = painted(&tty.output(), "Terminal").next().unwrap();
            tty.bytes.clear();
            tty.click(span.start, row, "");
            tty.until("Workflow");
        }
        tty.master.write_all(b"qq").unwrap();
        assert!(tty.wait_for_exit().success());
    }
}

#[test]
fn repeat_clicks_do_not_cross_the_onboarding_handoff() {
    fn spot(tty: &Terminal, label: &str) -> (u16, u16) {
        let screen = tty.screen();
        screen
            .iter()
            .enumerate()
            .find_map(|(row, line)| {
                let start = line.find(label)?;
                Some((u16::try_from(line[..start].chars().count() + 1).unwrap(), u16::try_from(row + 1).unwrap()))
            })
            .unwrap_or_else(|| panic!("missing {label:?} on {screen:#?}"))
    }

    let milo = env!("CARGO_BIN_EXE_milo");
    for (cols, rows) in [(40, 16), (28, 12)] {
        let project = Project::new();
        let mut tty = Terminal::sized(&mut project.command(milo), cols, rows);
        tty.until("q skip");
        let (skip_col, footer) = spot(&tty, "q skip");
        tty.click(skip_col, footer, "");
        tty.until("Uninstall");
        tty.shows("q quit");
        // The same cell must be able to quit Home if the repeat is not suppressed.
        let (quit_col, quit_row) = spot(&tty, "q quit");
        assert_eq!(quit_row, footer);
        assert!((quit_col..quit_col + 6).contains(&skip_col));
        tty.click(skip_col, footer, "?");
        tty.until("Milo home");
        tty.master.write_all(b"qq").unwrap();
        assert!(tty.wait_for_exit().success());

        let project = Project::new();
        let mut tty = Terminal::sized(project.command(milo).arg("setup"), cols, rows);
        tty.until("q skip");
        let (skip_col, skip_row) = spot(&tty, "Skip setup");
        tty.click(skip_col, skip_row, "");
        tty.shows("Outside Rex");
        // The repeat must neither open the scope picker nor change the draft.
        tty.click(skip_col, skip_row, "2");
        tty.shows("Choose Nerd Font");
        assert!(!tty.screen()[0].contains("Unsaved changes"));

        // Verify that a separate click can still use Appearance's save action at its drawn position.
        let save = if cols == 28 { "Save prefs" } else { "Save pane preferences for" };
        if !tty.screen().iter().any(|row| row.contains(save)) {
            tty.master.write_all(b"\x1b[F").unwrap(); // End
            tty.shows(save);
        }
        let (save_col, save_row) = spot(&tty, save);
        tty.click(save_col, save_row, "");
        tty.shows("Save preferences for");
        tty.master.write_all(b"qq").unwrap();
        assert!(tty.wait_for_exit().success());

        // Without the onboarding barrier, the same click either opens the scope
        // picker or dirties a Workflow preference, depending on the geometry.
        let mut probe = Terminal::sized(project.command(milo).arg("setup"), cols, rows);
        probe.shows("Outside Rex");
        probe.click(skip_col, skip_row, "2");
        let until = Instant::now() + TEST_WAIT;
        while !probe.screen().iter().any(|row| row.contains("Save preferences for") || row.contains("Unsaved changes")) {
            assert!(probe.read(until), "the onboarding click must overlap an actionable setting: {:#?}", probe.screen());
        }
        probe.master.write_all(b"\x03").unwrap();
        assert!(probe.wait_for_exit().success());
    }
}

#[test]
fn usage_and_doctor_read_only_the_selected_provider_directories() {
    let project = Project::new();
    let cwd = project.0.join("project").canonicalize().unwrap();
    let milo = env!("CARGO_BIN_EXE_milo");
    let command = || {
        let mut command = project.command(milo);
        command
            .env_remove("REX_CONFIG")
            .env_remove("PI_CODING_AGENT_DIR")
            .env_remove("OPENCODE_DB")
            .env("XDG_DATA_HOME", project.0.join("data"))
            .env("CODEX_HOME", "codex-override")
            .env("CLAUDE_CONFIG_DIR", "claude-override");
        command
    };
    let doctor = |command: &mut Command| {
        let output = command.arg("doctor").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    };
    let save = |command: &mut Command, used: u32| {
        use std::process::Stdio;
        let mut child = command.args(["claude-status"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        write!(input, r#"{{"rate_limits":{{"seven_day":{{"used_percentage":{used}}}}}}}"#).unwrap();
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), format!("week {}% left\n", 100 - used));
    };

    let session = "sessions/2026/01/01/rollout-test.jsonl";
    for (path, used) in [(project.0.join(".codex"), 85), (cwd.join("codex-real"), 25)] {
        let file = path.join(session);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let row = format!(
            r#"{{"timestamp":"2026-01-01T00:00:00Z","type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"primary":{{"used_percent":{used},"window_minutes":10080}}}}}}}}"#
        );
        fs::write(file, format!("{row}\n")).unwrap();
    }
    std::os::unix::fs::symlink("codex-real", cwd.join("codex-override")).unwrap();
    save(command().env_remove("CLAUDE_CONFIG_DIR"), 82);
    let default_save = project.0.join("state/milo/claude-limits.json");
    let baseline = fs::read(&default_save).unwrap();

    let claude = cwd.join("claude-override");
    let transcript = claude.join("projects/test/session.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(transcript, "{}\n").unwrap();
    let report = doctor(&mut command());
    assert!(report.contains("Codex limits: Codex session log"), "{report}");
    assert!(report.contains("Claude Code limits: no local report"), "{report}");
    let report = doctor(command().env("CODEX_HOME", "missing-override"));
    assert!(!report.contains("Codex limits:"), "{report}");

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() - 60_000;
    fs::write(
        claude.join(".claude.json"),
        format!(
            r#"{{"oauthAccount":{{"accountUuid":"00000000-0000-4000-8000-000000000001"}},
            "cachedUsageUtilization":{{"accountUuid":"00000000-0000-4000-8000-000000000001","fetchedAtMs":{now},
            "utilization":{{"seven_day":{{"utilization":35}}}}}}}}"#
        ),
    )
    .unwrap();
    let report = doctor(&mut command());
    assert!(report.contains("Claude Code limits: Claude Code usage cache"), "{report}");

    save(command().env("CLAUDE_CONFIG_DIR", &claude), 61);
    assert_eq!(fs::read(&default_save).unwrap(), baseline);
    let report = doctor(&mut command());
    assert!(report.contains("Claude Code limits: Milo Claude status line"), "{report}");
    let report = doctor(command().env("CLAUDE_CONFIG_DIR", "other-claude"));
    assert!(!report.contains("Claude Code limits:"), "{report}");

    assert!(command().args(["setup", "--skip-onboarding"]).status().unwrap().success());
    let mut pane = Terminal::sized(command().arg("usage"), 80, 2);
    pane.until("75%");
    pane.until("39%");
    pane.master.write_all(b"q").unwrap();
    assert!(pane.wait_for_exit().success());
}

#[test]
fn claude_status_starts_fresh_when_the_account_changes_in_the_same_profile() {
    let project = Project::new();
    let milo = env!("CARGO_BIN_EXE_milo");
    let command = || {
        let mut command = project.command(milo);
        command.env_remove("CODEX_HOME").env_remove("PI_CODING_AGENT_DIR").env_remove("OPENCODE_DB");
        command.env("XDG_DATA_HOME", project.0.join("data"));
        command
    };
    let profile = |id| {
        fs::write(project.0.join(".claude.json"), format!(r#"{{"oauthAccount":{{"accountUuid":"{id}"}}}}"#)).unwrap();
    };
    let save = |limits: &str, expected: &str| {
        use std::process::Stdio;
        let mut child = command().arg("claude-status").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        write!(child.stdin.take().unwrap(), r#"{{"session_id":"same-session","rate_limits":{limits}}}"#).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    };
    let doctor = || {
        let output = command().arg("doctor").output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let week = now + 172_800;
    let old_week = week + 86_400;
    profile("00000000-0000-4000-8000-000000000001");
    save(
        &format!(
            r#"{{"five_hour":{{"used_percentage":77,"resets_at":{}}},"seven_day":{{"used_percentage":85,"resets_at":{old_week}}}}}"#,
            now + 3600
        ),
        "5h 23% · week 15% left\n",
    );
    assert!(doctor().contains("Claude Code limits: Milo Claude status line"));
    assert!(command().args(["setup", "--skip-onboarding"]).status().unwrap().success());
    let mut pane = Terminal::sized(command().arg("usage"), 80, 2);
    pane.shows("15%");

    profile("00000000-0000-4000-8000-000000000002");
    assert!(!doctor().contains("Claude Code limits: Milo Claude status line"));
    pane.master.write_all(b"r").unwrap();
    pane.hides("15%");
    pane.hides("23%");
    save(&format!(r#"{{"seven_day":{{"used_percentage":30,"resets_at":{week}}}}}"#), "week 70% left\n");

    let path = project.0.join("state/milo/claude-limits.json");
    let saved = fs::read_to_string(&path).unwrap();
    assert!(saved.contains(r#""five_hour":null"#) && saved.contains(r#""used_percentage":30"#), "{saved}");
    assert!(doctor().contains("Claude Code limits: Milo Claude status line"));
    pane.master.write_all(b"r").unwrap();
    pane.shows("70%");
    assert!(pane.screen().iter().all(|row| !row.contains("23%")));

    save(&format!(r#"{{"seven_day":{{"used_percentage":29,"resets_at":{week}}}}}"#), "week 71% left\n");
    assert_eq!(fs::read_to_string(path).unwrap(), saved);
    pane.master.write_all(b"q").unwrap();
    assert!(pane.wait_for_exit().success());
}

#[test]
fn the_reader_edits_a_file_and_asks_before_ctrl_c_can_lose_the_draft() {
    let project = Project::new();
    let file = project.0.join("project/page.txt");
    fs::write(&file, "old\r\n").unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    let mut tty = Terminal::run(command.args(["view", "page.txt"]));
    tty.shows("e edit");
    tty.master.write_all(b"e").unwrap();
    tty.shows("^S save · ^Z undo · Esc done · ^G help");

    // Keys that close or explain a reader are text now, and a paste is text too.
    tty.master.write_all(b"q? \x1b[200~pasted\nlines \x1b[201~").unwrap();
    tty.shows("unsaved");
    tty.shows("q? pasted");
    tty.shows("lines old");
    tty.master.write_all(b"\x03").unwrap();
    tty.shows("Unsaved edits: s save · d discard · Esc back");
    assert_eq!(fs::read(&file).unwrap(), b"old\r\n");
    tty.master.write_all(b"\x1b").unwrap();
    tty.shows("^S save");

    tty.master.write_all(b"\x13").unwrap();
    tty.shows("Saved");
    assert_eq!(fs::read(&file).unwrap(), b"q? pasted\r\nlines old\r\n");

    // Saved, so leaving does not ask, and the reader is a reader again.
    tty.master.write_all(b"\x1b").unwrap();
    tty.shows("e edit");
    tty.master.write_all(b"q").unwrap();
    assert!(tty.wait_for_exit().success());
    assert!(tty.restored());
    assert!(!project.0.join("state/milo/drafts").exists());
}

#[test]
fn a_reader_ended_from_outside_keeps_its_unsaved_edits_and_offers_them_again() {
    let project = Project::new();
    let file = project.0.join("project/page.txt");
    fs::write(&file, "old\n").unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    let mut tty = Terminal::run(command.args(["view", "page.txt", "--edit"]));
    tty.shows("^S save");
    tty.master.write_all(b"draft ").unwrap();
    tty.shows("unsaved");
    let pid = i32::try_from(tty.child.id()).unwrap();
    // SAFETY: This is our own, still-owned child, not a process group.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    tty.wait_for_exit();
    assert!(tty.restored());
    // The line about the draft comes after the terminal is handed back.
    tty.until("\x1b[?1049l");
    tty.until("were kept");
    assert_eq!(fs::read_to_string(&file).unwrap(), "old\n");
    let drafts: Vec<_> =
        fs::read_dir(project.0.join("state/milo/drafts")).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(drafts.len(), 1);
    assert_eq!(fs::read_to_string(&drafts[0]).unwrap(), "draft old\n");

    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    let mut tty = Terminal::run(command.args(["view", "page.txt", "--edit"]));
    tty.shows("Kept draft found: r restore · d delete · Esc later");
    tty.master.write_all(b"r").unwrap();
    tty.shows("Draft restored");
    tty.master.write_all(b"\x13").unwrap();
    tty.shows("Saved");
    assert_eq!(fs::read_to_string(&file).unwrap(), "draft old\n");
    assert!(!drafts[0].exists());
}

#[test]
fn view_edit_opens_a_read_only_file_for_reading_and_says_why() {
    let project = Project::new();
    fs::write(project.0.join("project/data.bin"), b"a\0b").unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    let mut tty = Terminal::run(command.args(["view", "data.bin", "--edit"]));
    tty.shows("Read-only: not text");
    tty.master.write_all(b"q").unwrap();
    assert!(tty.wait_for_exit().success());

    let both = project.command(env!("CARGO_BIN_EXE_milo")).args(["view", "x", "--edit", "--commit"]).output().unwrap();
    assert!(!both.status.success());
    assert!(String::from_utf8_lossy(&both.stderr).contains("[--commit ID | --edit]"));
}

#[test]
fn the_outline_edits_a_file_in_its_own_pane_and_returns_to_the_same_file() {
    let project = Project::new();
    let root = project.0.join("project");
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(root.join(name), format!("{name} text\n")).unwrap();
    }
    // A setting milo cannot read puts a notice under the outline, which is then drawn by a
    // pane around it. Keys and pasted text must still reach the editor through that pane.
    fs::create_dir_all(project.0.join("config/milo")).unwrap();
    fs::write(project.0.join("config/milo/preferences.conf"), "garbage line\n").unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    let mut tty = Terminal::run(command.arg("outline"));
    tty.shows("c.txt");
    tty.master.write_all(b"j\r").unwrap();
    tty.shows("b.txt text");
    tty.master.write_all(b"e").unwrap();
    tty.shows("^S save");
    tty.master.write_all(b"q? \x1b[200~edited \x1b[201~\x03").unwrap();
    tty.shows("Unsaved edits: s save");
    tty.shows("q? edited b.txt text");
    tty.master.write_all(b"\x1b").unwrap();
    tty.shows("^S save");
    tty.master.write_all(b"\x13").unwrap();
    tty.shows("Saved");
    assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "q? edited b.txt text\n");

    // Esc stops editing, q closes the reader, and the outline still has that file selected.
    tty.master.write_all(b"\x1b").unwrap();
    tty.shows("e edit");
    tty.master.write_all(b"q").unwrap();
    tty.shows("c.txt");
    tty.hides("edited b.txt text");
    tty.master.write_all(b"\r").unwrap();
    tty.shows("edited b.txt text");
    tty.master.write_all(b"qq").unwrap();
    assert!(tty.wait_for_exit().success());
    assert!(tty.restored());
}

#[test]
fn the_outline_adds_a_tab_to_its_floating_reader_and_ctrl_c_there_asks_about_a_draft_in_another_tab() {
    let project = Project::new();
    fs::write(project.0.join("project/draft.txt"), "data\n").unwrap();
    fs::write(project.0.join("project/page.txt"), "more\n").unwrap();
    let block = "block:11111111-1111-1111-1111-111111111111";
    let rex = project.0.join("rex");
    // Every request is noted by its arguments, which say whether a reader is opened or only shown.
    let script = format!(
        "#!/bin/sh\n[ \"$1\" = ready ] && exit 0\ncat >/dev/null\nfor last do :; done\n\
         echo \"$last\" >> \"$HOME/requests\"\necho {block}\n"
    );
    fs::write(&rex, script).unwrap();
    fs::set_permissions(&rex, fs::Permissions::from_mode(0o700)).unwrap();
    // A sibling test's new process can hold the script open for writing for a moment.
    let until = Instant::now() + TEST_WAIT;
    while let Err(error) = project.command(&rex).arg("ready").status() {
        assert!(
            error.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < until,
            "cannot run stand-in rex: {error}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut paths = vec![project.0.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let in_rex = |args: &[&str], block: &str| {
        let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
        command
            .env("PATH", std::env::join_paths(&paths).unwrap())
            .env("REX_SESSION", "test")
            .env("REX_BLOCK", block)
            .args(args);
        command
    };
    let requests = |count: usize| {
        let until = Instant::now() + TEST_WAIT;
        loop {
            let log = fs::read_to_string(project.0.join("requests")).unwrap_or_default();
            if log.lines().count() == count {
                return log.lines().last().unwrap_or_default().to_owned();
            }
            assert!(Instant::now() < until, "expected {count} requests to Rex, not:\n{log}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    // The outline opens the file in a floating reader, which Rex runs in the block it names.
    let mut outline = Terminal::run(&mut in_rex(&["outline"], "block:22222222-2222-2222-2222-222222222222"));
    outline.shows("page.txt");
    outline.master.write_all(b"\r").unwrap();
    assert!(requests(1).contains(r#""view""#));
    let mut reader = Terminal::run(&mut in_rex(&["view", "draft.txt", "--edit"], block));
    reader.shows("^S save");
    reader.master.write_all(b"x").unwrap();
    reader.shows("unsaved");

    // A second file goes to that reader as a tab: Rex is asked to show the block, not to run another.
    outline.master.write_all(b"j\r").unwrap();
    reader.shows("*draft.txt  page.txt ×");
    reader.shows("more");
    let shown = requests(2);
    assert!(shown.contains(&format!(r#""block":"{block}""#)) && !shown.contains("view"), "{shown}");
    assert!(shown.contains(r#""label":"page.txt""#), "{shown}");
    // Asking for it again makes no second tab.
    outline.master.write_all(b"\r").unwrap();
    requests(3);
    assert_eq!(reader.screen()[0].matches("page.txt").count(), 1, "{:#?}", reader.screen());

    // Ctrl-C would end the reader with the other tab's draft, so that tab comes forward and asks.
    reader.master.write_all(b"\x03").unwrap();
    reader.shows("Unsaved edits: s save · d discard · Esc back");
    reader.shows("*draft.txt ×  page.txt");
    reader.master.write_all(b"s").unwrap();
    reader.shows("Saved");
    assert_eq!(fs::read_to_string(project.0.join("project/draft.txt")).unwrap(), "xdata\n");
    reader.master.write_all(b"\x03").unwrap();
    assert!(reader.wait_for_exit().success());
    assert!(!project.0.join("state/milo/drafts").exists());

    // With that reader gone there is nothing to add a tab to, and the outline opens another.
    outline.master.write_all(b"\r").unwrap();
    assert!(requests(4).contains(r#""view""#));
}

#[test]
fn a_reader_ended_from_outside_says_which_tabs_drafts_it_kept() {
    let project = Project::new();
    for name in ["a.txt", "b.txt"] {
        fs::write(project.0.join("project").join(name), "old\n").unwrap();
    }
    let block = "block:11111111-1111-1111-1111-111111111111";
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    command.env("REX_SESSION", "test").env("REX_BLOCK", block);
    let mut tty = Terminal::run(command.args(["view", "a.txt", "--edit"]));
    tty.shows("^S save");
    tty.master.write_all(b"one ").unwrap();
    tty.shows("unsaved");

    // What a pane writes to ask for a second file: its path, and no commit.
    let pipe = fs::read_dir(project.0.join("state/milo/readers")).unwrap().next().unwrap().unwrap().path();
    let request = format!("{}\0\0", project.0.join("project/b.txt").display());
    fs::OpenOptions::new().write(true).open(pipe).unwrap().write_all(request.as_bytes()).unwrap();
    tty.shows("*a.txt  b.txt ×");
    tty.master.write_all(b"etwo ").unwrap();
    tty.shows("*a.txt  *b.txt ×");

    let pid = i32::try_from(tty.child.id()).unwrap();
    // SAFETY: This is our own, still-owned child, not a process group.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    tty.wait_for_exit();
    assert!(tty.restored());
    tty.until("\x1b[?1049l");
    for name in ["a.txt", "b.txt"] {
        tty.until(&format!("{name} were kept"));
    }
    let mut drafts: Vec<String> = fs::read_dir(project.0.join("state/milo/drafts"))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    drafts.sort();
    assert_eq!(drafts, ["one old\n", "two old\n"]);
    assert!(fs::read_dir(project.0.join("state/milo/readers")).unwrap().next().is_none());
}

#[test]
fn formatting_runs_an_installed_formatter_on_the_draft_and_saves_only_when_asked() {
    let project = Project::new();
    let file = project.0.join("project/a.go");
    fs::write(&file, "package main\n").unwrap();
    // A program that already exists stands in for the formatter: `rev` turns each line
    // around, and a link to it is not a file some other test's new process could hold open.
    let rev = ["/usr/bin/rev", "/bin/rev"].into_iter().find(|rev| Path::new(rev).is_file()).expect("no rev");
    let tools = project.0.join("tools");
    fs::create_dir(&tools).unwrap();
    std::os::unix::fs::symlink(rev, tools.join("gofmt")).unwrap();
    let mut paths = vec![tools.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));

    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    command.env("PATH", std::env::join_paths(paths).unwrap());
    let mut tty = Terminal::run(command.args(["view", "a.go", "--edit"]));
    tty.shows("^F format");
    tty.master.write_all(b"\x06").unwrap();
    tty.shows("Formatted with gofmt");
    tty.shows("niam egakcap");
    tty.shows("unsaved");
    assert_eq!(fs::read_to_string(&file).unwrap(), "package main\n");
    tty.master.write_all(b"\x1a").unwrap();
    tty.shows("package main");
    tty.hides("unsaved");
    tty.master.write_all(b"\x06").unwrap();
    tty.shows("niam egakcap");
    tty.master.write_all(b"\x13").unwrap();
    tty.shows("Saved");
    assert_eq!(fs::read_to_string(&file).unwrap(), "niam egakcap\n");
    // Esc and a letter sent together would read as one key, so Ctrl-Q stops editing here.
    tty.master.write_all(b"\x11q").unwrap();
    assert!(tty.wait_for_exit().success());

    // With no formatter to be found, the pane says which to install and starts nothing.
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    command.env("PATH", project.0.join("empty"));
    let mut tty = Terminal::run(command.args(["view", "a.go", "--edit"]));
    tty.shows("^F format");
    tty.master.write_all(b"\x06").unwrap();
    tty.shows("Not installed: gofmt.");
    tty.master.write_all(b"\x11q").unwrap();
    assert!(tty.wait_for_exit().success());
    assert_eq!(fs::read_to_string(&file).unwrap(), "niam egakcap\n");
}

#[test]
fn rustfmt_formats_a_rust_draft_where_it_is_installed() {
    let project = Project::new();
    // The project's stand-in home has no Rust toolchains, so say where the real ones are.
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let toolchains = |command: &mut Command| {
        for (name, folder) in [("RUSTUP_HOME", ".rustup"), ("CARGO_HOME", ".cargo")] {
            command.env(name, std::env::var_os(name).map_or_else(|| home.join(folder), PathBuf::from));
        }
    };
    let mut rustfmt = project.command("rustfmt");
    toolchains(&mut rustfmt);
    if !rustfmt.arg("--version").output().is_ok_and(|output| output.status.success()) {
        // The formatter is optional; the stand-in above covers the pane without it.
        return;
    }
    let file = project.0.join("project/main.rs");
    fs::write(&file, "fn  main( ){let x=1;}\n").unwrap();
    let mut command = project.command(env!("CARGO_BIN_EXE_milo"));
    toolchains(&mut command);
    let mut tty = Terminal::run(command.args(["view", "main.rs", "--edit"]));
    tty.shows("^F format");
    tty.master.write_all(b"\x06").unwrap();
    tty.shows("Formatted with rustfmt");
    tty.master.write_all(b"\x13").unwrap();
    tty.shows("Saved");
    assert_eq!(fs::read_to_string(&file).unwrap(), "fn main() {\n    let x = 1;\n}\n");
}

/// The installed binary has no way to send a key anywhere but its provider:
/// the loopback stand-in that unit tests use is not compiled into it, and the
/// variable that once selected it does nothing. Saved answers stand in for
/// the providers here, so this test sends no request at all.
#[test]
fn an_account_reaches_the_pane_doctor_and_menu_stream_and_no_setting_redirects_its_key() {
    use std::hash::{DefaultHasher, Hash as _, Hasher as _};
    use std::io::{BufRead as _, BufReader};
    use std::process::Stdio;

    const DEEPSEEK_KEY: &str = "sk-not-a-real-key";
    const OPENROUTER_KEY: &str = "sk-or-not-a-real-key";

    let milo = env!("CARGO_BIN_EXE_milo");
    let binary = fs::read(milo).unwrap();
    let holds = |text: &str| binary.windows(text.len()).any(|window| window == text.as_bytes());
    assert!(holds("MILO_DEEPSEEK_API_KEY") && holds("proto = \"=https\""));
    assert!(!holds("MILO_TEST_USAGE_PORT"), "the binary must not know a transport override");
    assert!(!holds("proto = \"=http\""), "the binary must not be able to ask over plain HTTP");

    // A receiver that would see any request the old override could have redirected.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = std::sync::Arc::new(AtomicU32::new(0));
    let counted = received.clone();
    std::thread::spawn(move || {
        for _ in listener.incoming() {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });
    let project = Project::new();
    let command = || {
        let mut command = project.command(milo);
        command
            .env_remove("REX_CONFIG")
            .env_remove("PI_CODING_AGENT_DIR")
            .env_remove("OPENCODE_DB")
            .env_remove("CODEX_HOME")
            .env("XDG_DATA_HOME", project.0.join("data"))
            .env("MILO_TEST_USAGE_PORT", port.to_string())
            // Should a request be made after all, curl's proxy sends it to the counter, not to a provider.
            .env("HTTPS_PROXY", format!("http://127.0.0.1:{port}"))
            .env("https_proxy", format!("http://127.0.0.1:{port}"))
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env_remove("MILO_DEEPSEEK_API_KEY")
            .env("MILO_OPENROUTER_API_KEY", OPENROUTER_KEY);
        command
    };
    let config = project.0.join("config/milo");
    fs::create_dir_all(&config).unwrap();
    let keys = config.join("usage-accounts.conf");
    fs::write(&keys, format!("MILO_DEEPSEEK_API_KEY = {DEEPSEEK_KEY}\n")).unwrap();
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(command().args(["setup", "--skip-onboarding"]).status().unwrap().success());

    // Keys alone turn nothing on: the pane and doctor say nothing of the accounts.
    let mut pane = Terminal::sized(command().arg("usage"), 80, 4);
    // The pane moves the cursor across spaces, so only single words arrive whole.
    pane.until("show");
    pane.master.write_all(b"rq").unwrap();
    assert!(pane.wait_for_exit().success());
    let report = command().arg("doctor").output().unwrap();
    assert!(!String::from_utf8_lossy(&report.stdout).contains("DeepSeek"));
    assert!(!project.0.join("state/milo/accounts").exists());

    // Answers saved moments ago, as another pane would have left them: none is due a request.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let later = now + 3600;
    let saved = project.0.join("state/milo/accounts");
    fs::create_dir_all(&saved).unwrap();
    // The v2 cache uses the provider, key and optional scope in its file name.
    let cache = |provider: &str, key: &str| {
        let [first, second] = [0, 1].map(|domain: u8| {
            let mut hash = DefaultHasher::new();
            (domain, provider, key, None::<&str>).hash(&mut hash);
            hash.finish()
        });
        saved.join(format!("{provider}-{first:016x}{second:016x}.json"))
    };
    let deepseek = cache("deepseek", DEEPSEEK_KEY);
    let openrouter = cache("openrouter", OPENROUTER_KEY);
    let reading = format!(
        r#"{{"observed":{now},"figure":{{"kind":"balance","unit":"cny","value":110}},"details":[{{"kind":"granted","unit":"cny","value":10}}],"depleted":false}}"#
    );
    fs::write(&deepseek, format!(r#"{{"version":2,"asked":{now},"next":{later},"reading":{reading}}}"#)).unwrap();
    fs::write(&openrouter, format!(r#"{{"version":2,"asked":{now},"next":{later},"failure":"limited"}}"#)).unwrap();
    fs::write(config.join("preferences.conf"), "usage_deepseek = auto\nusage_openrouter = always\n").unwrap();

    let mut pane = Terminal::sized(command().arg("usage"), 80, 4);
    pane.until("granted");
    pane.until("limited");
    assert!(!pane.output().contains('%'), "{}", pane.output());
    pane.master.write_all(b"q").unwrap();
    assert!(pane.wait_for_exit().success());

    let report = command().arg("doctor").output().unwrap();
    let report = String::from_utf8(report.stdout).unwrap();
    assert!(
        report.contains("Usage accounts: DeepSeek shown once it has a figure; OpenRouter always shown"),
        "{report}"
    );
    assert!(report.contains("DeepSeek balance: asks api.deepseek.com with MILO_DEEPSEEK_API_KEY from "), "{report}");
    assert!(report.contains("  last answer: ¥110.00 balance, "), "{report}");
    assert!(report.contains("  last attempt: the provider rate limited the request"), "{report}");
    assert!(!report.contains("not-a-real-key"), "{report}");

    let mut stream =
        command().args(["usage", "--menu-stream"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut lines = BufReader::new(stream.stdout.take().unwrap()).lines();
    // Each account is looked at on a thread of its own, even when a saved
    // answer spares the request, and the stream does not hold a line back for
    // one: an account still being looked at reads "Asking the provider" until
    // a later line, a second apart. Both looks only read a file here, so the
    // line with both must come within a few.
    let settled = |line: &String| line.contains("deepseek") && !line.contains("Asking the provider");
    let line = lines
        .by_ref()
        .take(10)
        .map(Result::unwrap)
        .find(settled)
        .expect("a line with both accounts' saved answers");
    assert!(
        line.contains(
            r#"{"id":"deepseek","name":"DeepSeek","windows":[],"detail":"¥110.00 balance · ¥10.00 granted"}"#
        ),
        "{line}"
    );
    assert!(line.contains(r#"{"id":"openrouter","name":"OpenRouter","windows":[],"detail":"Rate limited"}"#), "{line}");
    drop(stream.stdin.take());
    assert!(stream.wait().unwrap().success());

    // Nothing was sent to the loopback receiver, with or without accounts turned on.
    assert_eq!(received.load(Ordering::SeqCst), 0);
    for path in [deepseek, openrouter] {
        assert!(!path.file_name().unwrap().to_str().unwrap().contains("not-a-real-key"));
        assert!(!fs::read_to_string(path).unwrap().contains("not-a-real-key"));
    }
}
