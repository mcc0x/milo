//! Times how long `milo web` of a release build takes to hand over its
//! settings editor, in a throwaway home with made-up settings.
//!
//! Each run starts `milo web --no-open --port 0` and times what a person
//! waits for: the private link on standard output, and the first answer to
//! an authenticated request for the editor's state. Only the command line and
//! the loopback server are used, so any build can be timed.
//!
//! ```sh
//! cargo test --release --locked --test web_bench -- --ignored --nocapture
//! ```
//!
//! `tests/bench/report.rs` lists the variables that choose the builds, the
//! runs and the report. The noise allowed before a ratio fails the test is
//! two milliseconds: each sample starts a process.

#[path = "bench/report.rs"]
mod report;

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use report::{Bench, Samples};

const WAIT: Duration = Duration::from_secs(30);
/// A median must exceed the base's by this much, as well as by the ratio,
/// before it fails the test.
const NOISE: Duration = Duration::from_millis(2);
/// How long the stand-in for a browser takes to ask for the page once it has
/// the link. A real browser takes longer; this only shows whether the editor
/// uses that time.
const BROWSER: Duration = Duration::from_millis(100);
/// How long the slow stand-in for Rex takes to answer one question.
const SLOW_REX: &str = "0.15";

/// Where `milo web` is started from.
struct Scenario {
    name: &'static str,
    /// Seconds the stand-in for Rex waits before each answer, or `None`
    /// to start outside Rex.
    rex: Option<&'static str>,
}

const SCENARIOS: &[Scenario] = &[
    Scenario { name: "outside-rex", rex: None },
    Scenario { name: "rex-pane", rex: Some("0") },
    Scenario { name: "rex-pane-slow", rex: Some(SLOW_REX) },
];

/// A throwaway home, a Git project in it, and a stand-in for Rex.
struct Home {
    root: PathBuf,
}

impl Home {
    fn new(scenario: &Scenario) -> Self {
        let root = std::env::temp_dir().join(format!("milo-web-bench-{}-{}", std::process::id(), scenario.name));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("project")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        let home = Self { root };
        let git = home.command("git").args(["init", "-q", "--template=", "-b", "main"]).output().unwrap();
        assert!(git.status.success(), "git init: {}", String::from_utf8_lossy(&git.stderr));
        if let Some(delay) = scenario.rex {
            // It answers the one question the editor asks as it opens: which tab it is in.
            let script = format!(
                "#!/bin/sh\n/bin/cat >/dev/null\necho asked >> \"$HOME/rex.log\"\n\
                 for arg do case \"$arg\" in\n\
                 milo_operation=context) /bin/sleep {delay}; printf 'window:test\\nok - 120 40\\n'; exit;;\n\
                 esac; done\nexit 1\n"
            );
            let rex = home.root.join("bin/rex");
            fs::write(&rex, script).unwrap();
            fs::set_permissions(rex, fs::Permissions::from_mode(0o700)).unwrap();
        }
        home
    }

    /// Nothing of the developer's reaches the editor: no settings, no Rex,
    /// no Discord, and no variable but the ones named here.
    fn command(&self, program: impl AsRef<Path>) -> Command {
        let mut command = Command::new(program.as_ref());
        let mut path = self.root.join("bin").into_os_string();
        path.push(":/usr/bin:/bin");
        command
            .env_clear()
            .current_dir(self.root.join("project"))
            .env("PATH", path)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    fn web(&self, binary: &Path, scenario: &Scenario) -> Command {
        let mut command = self.command(binary);
        command.args(["web", "--no-open", "--port", "0"]).stdin(Stdio::null()).stdout(Stdio::piped());
        if scenario.rex.is_some() {
            command
                .env("TERM_PROGRAM", "rex")
                .env("REX_SESSION", "session:test")
                .env("REX_BLOCK", "block:shell")
                .env("REX_CONFIG", self.root.join("init.lua"));
        }
        command
    }

    /// How often the stand-in for Rex has been run.
    fn rex_calls(&self) -> usize {
        fs::read_to_string(self.root.join("rex.log")).map_or(0, |log| log.lines().count())
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A running `milo web`, ended when dropped.
struct Server(Child);

impl Server {
    /// Waits for the private link and returns it without its line ending.
    fn link(&mut self) -> String {
        let mut line = String::new();
        BufReader::new(self.0.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
        assert!(line.starts_with("http://127.0.0.1:"), "no link from milo web: {line:?}");
        line.trim_end().to_owned()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // SAFETY: The child has not been waited for, so its ID still names it.
        unsafe { libc::kill(self.0.id().cast_signed(), libc::SIGTERM) };
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until && self.0.try_wait().is_ok_and(|status| status.is_none()) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Asks for the editor's state as the page does, and checks that the answer is a whole one.
fn state(link: &str) {
    let (origin, token) = link.split_once("/#").expect("a link with a token");
    let host = origin.strip_prefix("http://").unwrap();
    let mut socket = TcpStream::connect(host).unwrap();
    socket.set_read_timeout(Some(WAIT)).unwrap();
    write!(socket, "GET /api/state HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n\r\n").unwrap();
    let mut reply = String::new();
    socket.read_to_string(&mut reply).unwrap();
    let body = reply.strip_prefix("HTTP/1.1 200 OK\r\n").and_then(|rest| rest.split_once("\r\n\r\n"));
    assert!(body.is_some_and(|(_, body)| body.starts_with('{') && body.ends_with('}')), "not the state: {reply:?}");
}

/// Starts the editor twice, once for each stand-in for a browser, and
/// returns how often Rex was asked before the first state was served.
fn run(samples: &mut Samples, binary: &Path, home: &Home, scenario: &Scenario) -> usize {
    let asked = home.rex_calls();
    let mut calls = 0;
    for browser in [Duration::ZERO, BROWSER] {
        let began = Instant::now();
        let mut server = Server(home.web(binary, scenario).spawn().unwrap());
        let link = server.link();
        let linked = began.elapsed();
        std::thread::sleep(browser);
        state(&link);
        let answered = began.elapsed();
        if browser.is_zero() {
            samples.push(scenario.name, "link", linked);
            samples.push(scenario.name, "state", answered);
            calls = home.rex_calls() - asked;
        } else {
            samples.push(scenario.name, "state-after-browser", answered);
        }
    }
    calls
}

#[test]
#[ignore = "a benchmark; run it alone on a release build"]
fn web_link_and_first_state() {
    let bench = Bench::from_env(env!("CARGO_BIN_EXE_milo"), "web_bench");
    let mut heading = bench.heading("milo web bench: time from starting the process");
    writeln!(heading, "# state-after-browser: the state is asked for {BROWSER:?} after the link").unwrap();
    writeln!(heading, "# rex-pane-slow: the stand-in for Rex takes {SLOW_REX}s to answer").unwrap();
    let mut samples = [Samples::default(), Samples::default()];
    for scenario in SCENARIOS {
        let home = Home::new(scenario);
        let mut asked = 0;
        for turn in 0..bench.runs {
            for (binary, build) in bench.builds(turn) {
                let calls = run(&mut samples[build], binary, &home, scenario);
                if build == 0 {
                    asked = calls;
                }
            }
        }
        writeln!(heading, "# {}: Rex was asked {asked} times before the state was served", scenario.name).unwrap();
    }
    bench.report(heading, &samples, |_| NOISE);
}
