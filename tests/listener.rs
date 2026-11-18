//! Runs the real `milo listen` with a stand-in for `rex`.
//!
//! The lock that keeps one listener alive belongs to a process, so only
//! real processes can show that it works: one listener at a time, stopped
//! on request, and never confused with a program the listener left behind.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// A stand-in that says it has started and then stays, as `rex` does.
const STAYS: &str = "#!/bin/sh\necho started > \"$HOME/started\"\nexec sleep 20\n";
/// A stand-in that starts another program, notes its number, and ends.
const LEAVES_ONE_BEHIND: &str = "#!/bin/sh\nsleep 20 &\necho $! > \"$HOME/left-behind\"\n";
/// A listener that needs a second request to stop.
const STOPS_ON_RETRY: &str = "\
#!/bin/sh
trap 'if test -f \"$HOME/term\"; then exit; fi; touch \"$HOME/term\"' TERM
echo started > \"$HOME/started\"
while :; do sleep 0.02; done
";

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
        home.set_rex(rex);
        home
    }

    fn set_rex(&self, script: &str) {
        let path = self.root.join("bin/rex");
        // Written beside it and moved into place: a script still open for
        // writing cannot be run.
        let draft = self.root.join("bin/rex.draft");
        fs::write(&draft, script).unwrap();
        fs::set_permissions(&draft, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(draft, path).unwrap();
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

/// Whether `happens` comes true within a few seconds.
fn soon(mut happens: impl FnMut() -> bool) -> bool {
    let began = Instant::now();
    loop {
        if happens() {
            return true;
        }
        if began.elapsed() > Duration::from_secs(5) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn signal(how: &str, process: u32) -> bool {
    Command::new("kill").args([how, &process.to_string()]).stderr(Stdio::null()).status().unwrap().success()
}

fn is_running(process: u32) -> bool {
    signal("-0", process)
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
    assert!(is_running(left_behind));

    // The listener itself has ended, so there is nothing to stop, and what
    // it started is left alone.
    assert!(home.milo(&["listen", "--stop"]).status().unwrap().success());
    assert!(is_running(left_behind));

    // Nor does what it left behind keep a new listener from starting.
    home.set_rex(STAYS);
    let mut listener = home.milo(&["listen"]).spawn().unwrap();
    assert!(soon(|| home.started()));

    assert!(home.milo(&["listen", "--stop"]).status().unwrap().success());
    assert!(soon(|| listener.try_wait().unwrap().is_some()));
    assert!(is_running(left_behind));
    signal("-TERM", left_behind);
}

#[test]
fn stopping_with_no_listener_does_nothing() {
    let home = Home::new(STAYS);
    assert!(home.milo(&["listen", "--stop"]).status().unwrap().success());
    assert!(!home.started());
}

#[test]
fn stopping_retries_when_the_listener_holds_the_lock_after_the_wait() {
    let home = Home::new(STOPS_ON_RETRY);
    let mut listener = home.milo(&["listen"]).spawn().unwrap();
    assert!(soon(|| home.started()));
    let stopped = home.milo(&["listen", "--stop"]).status().unwrap().success();
    if !stopped {
        let _ = listener.kill();
    }
    assert!(soon(|| listener.try_wait().unwrap().is_some()));
    assert!(stopped);
}
