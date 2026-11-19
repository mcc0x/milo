//! Runs the real `milo listen` with a stand-in for `rex`.
//!
//! The lock that keeps one listener alive belongs to a process, so only
//! real processes can show that it works: one listener at a time, stopped
//! on request, and never confused with a program the listener left behind.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// A stand-in that says it has started and then stays, as `rex` does.
const STAYS: &str = "echo started > \"$HOME/started\"\nexec sleep 120\n";
/// A stand-in that starts another program, notes its number, and ends.
const LEAVES_ONE_BEHIND: &str = "sleep 120 &\necho $! > \"$HOME/left-behind\"\n";
static NEXT: AtomicU32 = AtomicU32::new(0);

/// A home directory of this test's own, with its own `rex` first on the path.
struct Home {
    root: PathBuf,
}

impl Home {
    fn new(rex: &str) -> Self {
        let name = format!("milo-listener-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let home = Self { root: std::env::temp_dir().join(name) };
        fs::create_dir_all(home.root.join("bin")).unwrap();
        let config = home.root.join(".config/milo");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("listener.lua"), "").unwrap();
        home.set_rex(rex);
        home
    }

    fn set_rex(&self, script: &str) {
        let path = self.root.join("bin/rex");
        fs::write(&path, format!("#!/bin/sh\n[ \"$1\" = ready ] && exit 0\n{script}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        // A test thread's fork can briefly inherit the file open for writing.
        assert!(soon(|| Command::new(&path).arg("ready").status().is_ok_and(|status| status.success())));
    }

    fn milo(&self, args: &[&str]) -> Command {
        let path = std::env::var("PATH").unwrap_or_default();
        let mut milo = Command::new(env!("CARGO_BIN_EXE_milo"));
        milo.args(args)
            .env("HOME", &self.root)
            .env("PATH", format!("{}:{path}", self.root.join("bin").display()))
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        milo
    }

    fn started(&self) -> bool {
        self.root.join("started").exists()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Wait for the process or file, with a generous limit for a busy test machine.
fn soon(mut happens: impl FnMut() -> bool) -> bool {
    let began = Instant::now();
    loop {
        if happens() {
            return true;
        }
        if began.elapsed() >= Duration::from_secs(30) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn signal(how: libc::c_int, process: u32) -> bool {
    let pid = libc::pid_t::try_from(process).expect("not a process ID");
    assert!(pid > 1);
    // SAFETY: A positive ID addresses only the test process.
    unsafe { libc::kill(pid, how) == 0 }
}

#[test]
fn only_one_listener_runs_and_stopping_ends_it() {
    let home = Home::new(STAYS);
    let mut listener = home.milo(&["listen"]).spawn().unwrap();
    assert!(soon(|| home.started()));

    // A second one finds the place taken and leaves at once, starting nothing.
    fs::remove_file(home.root.join("started")).unwrap();
    assert!(home.milo(&["listen"]).status().unwrap().success());
    assert!(!home.started());

    assert!(home.milo(&["listen", "--stop"]).status().unwrap().success());
    assert!(soon(|| listener.try_wait().unwrap().is_some()));

    // With it gone, the next one can start.
    let mut next = home.milo(&["listen"]).spawn().unwrap();
    assert!(soon(|| home.started()));
    assert!(home.milo(&["listen", "--stop"]).status().unwrap().success());
    assert!(soon(|| next.try_wait().unwrap().is_some()));
}

#[test]
fn a_program_the_listener_left_behind_is_not_taken_for_it() {
    let home = Home::new(LEAVES_ONE_BEHIND);
    assert!(home.milo(&["listen"]).status().unwrap().success());
    let noted = fs::read_to_string(home.root.join("left-behind")).unwrap();
    let left_behind: u32 = noted.trim().parse().unwrap();
    assert!(signal(0, left_behind));

    // The listener itself has ended, so there is nothing to stop, and what
    // it started is left alone.
    assert!(home.milo(&["listen", "--stop"]).status().unwrap().success());
    assert!(signal(0, left_behind));

    // Nor does what it left behind keep a new listener from starting.
    home.set_rex(STAYS);
    let mut listener = home.milo(&["listen"]).spawn().unwrap();
    assert!(soon(|| home.started()));

    assert!(home.milo(&["listen", "--stop"]).status().unwrap().success());
    assert!(soon(|| listener.try_wait().unwrap().is_some()));
    assert!(signal(0, left_behind));
    signal(libc::SIGTERM, left_behind);
}

#[test]
fn stopping_with_no_listener_does_nothing() {
    let home = Home::new(STAYS);
    assert!(home.milo(&["listen", "--stop"]).status().unwrap().success());
    assert!(!home.started());
}

#[test]
fn starting_without_a_saved_listener_explains_how_to_enable_it() {
    let home = Home::new("echo started > \"$HOME/started\"\n");
    fs::remove_file(home.root.join(".config/milo/listener.lua")).unwrap();
    let output = home.milo(&["listen"]).stderr(Stdio::piped()).output().unwrap();
    assert!(!output.status.success());
    assert!(!home.started());
    assert!(String::from_utf8(output.stderr).unwrap().contains("new shell tabs are on in milo setup"));
}

#[test]
fn listening_leaves_a_linked_milo_folder_alone() {
    let home = Home::new(STAYS);
    let config = home.root.join(".config/milo");
    let elsewhere = home.root.join("dotfiles");
    fs::rename(&config, &elsewhere).unwrap();
    symlink(&elsewhere, &config).unwrap();
    let output = home.milo(&["listen"]).stderr(Stdio::piped()).output().unwrap();
    assert!(!output.status.success());
    assert!(!home.started());
    assert!(String::from_utf8(output.stderr).unwrap().contains("must be a regular folder"));
    assert_eq!(fs::read_link(config).unwrap(), elsewhere);
}

#[test]
fn listening_does_not_create_a_lock_in_a_linked_state_folder() {
    let home = Home::new("echo started > \"$HOME/started\"\n");
    let state = home.root.join("state");
    let elsewhere = home.root.join("dotfiles");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, state.join("milo")).unwrap();
    assert!(!home.milo(&["listen"]).env("XDG_STATE_HOME", &state).status().unwrap().success());
    assert!(!home.milo(&["listen", "--stop"]).env("XDG_STATE_HOME", &state).status().unwrap().success());
    assert!(!home.started());
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}
