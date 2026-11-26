//! Follows one person's Milo across two Rex hosts, each a disposable Rex
//! server with a home, settings, state and socket of its own in a throwaway
//! folder. Needs Rex, so it is ignored unless asked for:
//!
//! ```sh
//! cargo test --test rex_hosts -- --ignored --nocapture
//! ```
//!
//! Host A is the computer the person sits at; host B is the other one. The
//! test reads each host's Doctor, opens B's settings on A through a Rex
//! forward and saves through it, carries a bundle of settings from A to B and
//! takes it back, and checks after each step that A's own files are as they
//! were. Every `rex` it runs is told never to start a server, and reaches only
//! the two sockets below the throwaway folder. Set `MILO_TEST_REX` if `rex`
//! is not on your path, and `MILO_TEST_BINARY` to check another build of Milo.

#![cfg(unix)]

use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

struct Host {
    name: &'static str,
    root: PathBuf,
    rex: PathBuf,
    milo: PathBuf,
    server: Option<Child>,
}

fn installed_rex() -> PathBuf {
    if let Some(named) = std::env::var_os("MILO_TEST_REX").map(PathBuf::from) {
        assert!(named.is_file(), "MILO_TEST_REX names no file: {}", named.display());
        return named;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("rex"))
        .find(|rex| rex.is_file())
        .expect("install Rex, or set MILO_TEST_REX to its rex executable")
}

fn soon(mut happens: impl FnMut() -> bool) -> bool {
    let began = Instant::now();
    loop {
        if happens() {
            return true;
        }
        if began.elapsed() >= WAIT {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Host {
    fn start(lab: &Path, name: &'static str) -> Self {
        let rex = installed_rex();
        let milo =
            std::env::var_os("MILO_TEST_BINARY").map_or_else(|| env!("CARGO_BIN_EXE_milo").into(), PathBuf::from);
        let milo = milo.canonicalize().unwrap_or_else(|e| panic!("no Milo at {}: {e}", milo.display()));
        let root = lab.join(name);
        for dir in ["bin", "state", "tmp", "claude", "home/.config/rex", "home/.local/state"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("home/.zshenv"), "unsetopt global_rcs\nunset HISTFILE\n").unwrap();
        fs::write(root.join("home/.config/rex/init.lua"), "").unwrap();
        // This `rex` can never start a server. Which server it asks is the
        // host's own unless a command names another.
        let wrapper = root.join("bin/rex");
        fs::write(&wrapper, format!("#!/bin/sh\nexec '{}' --autostart=false \"$@\"\n", rex.display())).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&milo, root.join("bin/milo")).unwrap();
        let mut host = Self { name, root, rex, milo, server: None };
        let server = host
            .command(&host.rex)
            .args(["server", "run", "--restore=false", "--listen", &host.socket(), "--state-dir"])
            .arg(host.root.join("state"))
            .arg("--config")
            .arg(host.root.join("home/.config/rex/init.lua"))
            .stdout(host.log("server.log"))
            .stderr(host.log("server.log"))
            .spawn()
            .unwrap();
        host.server = Some(server);
        let answered =
            soon(|| host.rex_output(&["whoami", "--json"]).is_some_and(|reply| reply.contains(&host.socket())));
        assert!(answered, "the disposable Rex server of {name} did not start:\n{}", host.read("server.log"));
        host
    }

    fn socket(&self) -> String {
        format!("unix://{}", self.root.join("s.sock").display())
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn config(&self) -> PathBuf {
        self.home().join(".config/milo")
    }

    fn log(&self, name: &str) -> File {
        File::options().create(true).append(true).open(self.root.join(name)).unwrap()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }

    /// Nothing of the developer's reaches a host: not their home, their
    /// settings, their Keychain, nor the Rex session this test may run in.
    fn command(&self, program: impl AsRef<Path>) -> Command {
        let home = self.home();
        let mut command = Command::new(program.as_ref());
        command
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.root.join("bin").display()))
            .env("HOME", &home)
            .env("ZDOTDIR", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("MILO_ALERTS_KEYCHAIN", self.root.join("test.keychain"))
            .env("MILO_TEST_HOST_NAME", self.name)
            .env("TMPDIR", self.root.join("tmp"))
            .env("SHELL", "/bin/zsh")
            .env("TERM", "xterm-256color")
            .env("REX_SERVER", self.socket())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(&self.root)
            .stdin(Stdio::null());
        command
    }

    fn rex_output(&self, args: &[&str]) -> Option<String> {
        let output = self.command(self.root.join("bin/rex")).args(args).output().ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn rex(&self, args: &[&str]) -> String {
        self.rex_output(args).unwrap_or_else(|| panic!("rex {args:?} failed on {}", self.name))
    }

    /// Runs Milo on this host in `dir` and gives what it printed, with whether it succeeded.
    fn milo_in(&self, dir: &Path, args: &[&str]) -> (bool, String) {
        let output = self.command(&self.milo).current_dir(dir).args(args).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        (output.status.success(), text)
    }

    fn milo(&self, dir: &Path, args: &[&str]) -> String {
        let (ok, text) = self.milo_in(dir, args);
        assert!(ok, "milo {args:?} failed on {}:\n{text}", self.name);
        text
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let output = self
            .command("/usr/bin/git")
            .current_dir(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }

    /// Every file of Milo's settings and state on this host, with its bytes.
    fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for entry in entries.map(Result::unwrap) {
                let path = entry.path();
                // A lock is touched by every reader; it holds no setting.
                if path.is_dir() {
                    walk(&path, out)
                } else if path.extension().is_none_or(|kind| kind != "lock") {
                    out.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        let mut files = Vec::new();
        walk(&self.config(), &mut files);
        walk(&self.home().join(".local/state/milo"), &mut files);
        files.sort();
        files
    }

    fn preferences(&self) -> String {
        fs::read_to_string(self.config().join("preferences.conf")).unwrap_or_default()
    }
}

impl Host {
    /// Asks this host's server, by its socket, to end, and waits for it.
    /// `rex server stop` does not follow `REX_SERVER`, so the socket is named.
    fn stop(&mut self) -> bool {
        let _ = self.rex_output(&["-S", &self.socket(), "server", "stop"]);
        let Some(server) = self.server.as_mut() else { return true };
        soon(|| server.try_wait().is_ok_and(|status| status.is_some()))
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Nothing here may panic: a second panic would skip the rest.
        let _ = self.rex_output(&["-S", &self.socket(), "server", "stop"]);
        if let Some(mut server) = self.server.take() {
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until && server.try_wait().is_ok_and(|status| status.is_none()) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = server.kill();
            let _ = server.wait();
        }
    }
}

/// The two hosts, and one repository checked out at a different path on each.
struct Lab {
    root: PathBuf,
    a: Host,
    b: Host,
    project_a: PathBuf,
    project_b: PathBuf,
}

impl Lab {
    fn start() -> Self {
        // A Unix socket's path is limited to about a hundred bytes.
        // Each lab of this process has a folder, and so two servers, of its own.
        static LABS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let lab = LABS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("milo-rex-hosts-{}-{lab}", std::process::id());
        let root = Path::new("/tmp").canonicalize().unwrap().join(name);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // The editor's link, which is a password, passes through here.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let (a, b) = (Host::start(&root, "host-a"), Host::start(&root, "host-b"));
        let project_a = a.home().join("src/app");
        let project_b = b.home().join("work/app-copy");
        fs::create_dir_all(&project_a).unwrap();
        fs::create_dir_all(project_b.parent().unwrap()).unwrap();
        a.git(&project_a, &["init", "-q", "--template=", "-b", "main"]);
        fs::write(project_a.join("README"), "one project, two hosts\n").unwrap();
        a.git(&project_a, &["add", "README"]);
        a.git(&project_a, &["commit", "-q", "-m", "start"]);
        b.git(&b.root, &["clone", "-q", project_a.to_str().unwrap(), project_b.to_str().unwrap()]);
        Self { root, a, b, project_a, project_b }
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        if std::thread::panicking() {
            for host in [&self.a, &self.b] {
                eprintln!("{} server log, last lines:", host.name);
                let log = host.read("server.log");
                let lines: Vec<&str> = log.lines().collect();
                eprintln!("{}", lines[lines.len().saturating_sub(15)..].join("\n"));
            }
        }
        // The servers end before their folders go.
        self.a.stop();
        self.b.stop();
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One HTTP request as a browser at `authority` would send it, and the answer's status and body.
fn http(
    port: u16,
    authority: &str,
    method: &str,
    path: &str,
    token: &str,
    origin: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {token}\r\n");
    if let Some(origin) = origin {
        request.push_str(&format!("Origin: {origin}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    request.push_str(body.unwrap_or(""));
    stream.write_all(request.as_bytes()).unwrap();
    let mut reply = String::new();
    let _ = stream.read_to_string(&mut reply);
    let status = reply.split(' ').nth(1).and_then(|code| code.parse().ok()).unwrap_or(0);
    (status, reply.split_once("\r\n\r\n").map(|(_, body)| body.to_owned()).unwrap_or_default())
}

/// A whole number field of the editor's JSON.
fn number(json: &str, field: &str) -> u64 {
    let (_, rest) = json.split_once(&format!("\"{field}\":")).unwrap_or_else(|| panic!("no {field} in {json}"));
    rest.trim_start().chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap()
}

/// A string as a JSON literal, for a bundle sent to the editor.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

/// Host B's settings editor, open on host A through a Rex forward.
struct Forwarded {
    process: Child,
    errors: PathBuf,
    port: u16,
    token: String,
}

impl Forwarded {
    fn open(lab: &Lab) -> Self {
        let errors = lab.root.join(format!("web-{}.log", lab.a.read("opened").len()));
        fs::write(lab.a.root.join("opened"), format!("{}x", lab.a.read("opened"))).unwrap();
        let mut process = lab
            .a
            .command(&lab.a.milo)
            .args(["web", "--no-open", "--host", &lab.b.socket(), "--project"])
            .arg(&lab.project_b)
            .stdout(Stdio::piped())
            .stderr(File::create(&errors).unwrap())
            .spawn()
            .unwrap();
        let mut link = String::new();
        BufReader::new(process.stdout.take().unwrap()).read_line(&mut link).unwrap();
        let said = fs::read_to_string(&errors).unwrap_or_default();
        let rest = link
            .trim()
            .strip_prefix("http://127.0.0.1:")
            .unwrap_or_else(|| panic!("milo web printed {link:?}\n{said}"));
        let (port, token) = rest.split_once("/#").unwrap();
        Self { process, errors, port: port.parse().unwrap(), token: token.to_owned() }
    }

    fn authority(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn origin(&self) -> String {
        format!("http://{}", self.authority())
    }

    fn get(&self, path: &str) -> (u16, String) {
        http(self.port, &self.authority(), "GET", path, &self.token, None, None)
    }

    fn post(&self, path: &str, body: &str) -> (u16, String) {
        http(self.port, &self.authority(), "POST", path, &self.token, Some(&self.origin()), Some(body))
    }

    fn state(&self) -> String {
        let (status, state) = self.get("/api/state");
        assert_eq!(status, 200, "{state}");
        state
    }

    fn said(&self) -> String {
        fs::read_to_string(&self.errors).unwrap_or_default()
    }

    /// Waits for A's `milo web` to end and gives what it said as it went.
    fn ended(mut self) -> String {
        assert!(soon(|| self.process.try_wait().unwrap().is_some()), "milo web --host did not end:\n{}", self.said());
        assert!(self.process.wait().unwrap().success(), "{}", self.said());
        self.said()
    }
}

impl Drop for Forwarded {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Nothing of a forwarded editor is left on either host.
fn assert_cleaned_up(lab: &Lab, port: u16, said: &str) {
    assert!(said.contains("closed the forward and the editor on"), "{said}");
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "port {port} on host A still listens");
    let gone = soon(|| !lab.b.rex(&["ls"]).contains("milo-settings"));
    assert!(gone, "host B still has the editor's session:\n{}", lab.b.rex(&["ls"]));
    let forwards = lab.b.rex(&["forward", "ls", "--all", "--quiet"]);
    assert_eq!(forwards.trim(), "", "host B still has a forward");
    let milo_left = Command::new("/usr/bin/pgrep")
        .args(["-f", &format!("{}.*--forwarded-port", lab.b.root.display())])
        .output()
        .unwrap();
    assert!(!milo_left.status.success(), "an editor is still running on host B");
}

#[test]
#[ignore = "needs an installed Rex; run with --ignored"]
fn doctor_names_each_host_and_its_rex() {
    let mut lab = Lab::start();
    let report = |host: &Host| host.milo(&host.root, &["doctor"]);
    let (a, b) = (report(&lab.a), report(&lab.b));
    let host_line = |report: &str| report.lines().find(|line| line.starts_with("Host: ")).map(str::to_owned);
    let (line_a, line_b) = (host_line(&a).unwrap(), host_line(&b).unwrap());
    assert!(line_a.starts_with("Host: host-a (Rex host ") && line_b.starts_with("Host: host-b (Rex host "), "{a}\n{b}");
    assert_ne!(line_a.rsplit(' ').next(), line_b.rsplit(' ').next(), "two hosts have two Rex host IDs");
    for report in [&a, &b] {
        let version = lab.a.rex(&["version", "--json"]);
        let api = number(&version, "api_revision");
        assert!(report.contains(&format!(", API {api}; rex command: the same")), "{report}");
        assert!(report.contains("Rex compatibility: checked with API "), "{report}");
        assert!(report.contains("Rex apps: none connected"), "{report}");
    }
    // A host whose server is gone says so and still reports the rest.
    assert!(lab.b.stop(), "host B's server did not stop");
    let stopped = report(&lab.b);
    assert!(stopped.contains("Host: host-b\nRex server: none answered"), "{stopped}");
    assert!(stopped.contains("Preferences: "), "{stopped}");
    assert!(report(&lab.a).contains(&line_a), "host A is unaffected");
}

#[test]
#[ignore = "needs an installed Rex; run with --ignored"]
fn the_other_hosts_settings_open_here_through_a_forward_that_ends_with_them() {
    let lab = Lab::start();
    let before_a = lab.a.files();
    let editor = Forwarded::open(&lab);
    let said = editor.said();
    assert!(
        said.contains("starting its settings editor") && said.contains("carried over a Rex forward on 127.0.0.1:"),
        "{said}"
    );

    // The page is host B's, and says so.
    let state = editor.state();
    assert!(state.contains("\"host\":{\"name\":\"host-b\",\"forwarded\":true"), "{state}");
    assert!(state.contains(&format!("\"worktree\":\"{}\"", lab.project_b.display())), "{state}");

    // The forward carries one authority, one origin and one key, to the letter.
    let wrong_name =
        http(editor.port, &format!("localhost:{}", editor.port), "GET", "/api/state", &editor.token, None, None);
    assert_eq!(wrong_name.0, 403, "{wrong_name:?}");
    let wrong_key = http(editor.port, &editor.authority(), "GET", "/api/state", &"0".repeat(64), None, None);
    assert_eq!(wrong_key.0, 401, "{wrong_key:?}");
    let set = format!(
        "{{\"action\":\"set\",\"version\":{},\"field\":\"appearance.icons\",\"value\":\"off\"}}",
        number(&state, "version")
    );
    let wrong_origin = format!("http://localhost:{}", editor.port);
    let refused =
        http(editor.port, &editor.authority(), "POST", "/api/action", &editor.token, Some(&wrong_origin), Some(&set));
    assert_eq!(refused.0, 403, "{refused:?}");
    // Host B's own port answers only to the address on host A, not to its own.
    let listed = lab.b.rex(&["forward", "ls", "--all"]);
    let own_port: u16 = listed
        .split_whitespace()
        .find_map(|word| {
            word.strip_prefix("localhost:")
                .or_else(|| word.strip_prefix("127.0.0.1:"))
                .filter(|_| !word.ends_with(&format!(":{}", editor.port)))
        })
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("no forward target in:\n{listed}"));
    let direct = http(own_port, &format!("127.0.0.1:{own_port}"), "GET", "/api/state", &editor.token, None, None);
    assert_eq!(direct.0, 403, "{direct:?}");

    // A change saved through the forward lands on host B and nowhere else.
    let (status, drafted) = editor.post("/api/action", &set);
    assert_eq!(status, 200, "{drafted}");
    let save = format!(
        "{{\"action\":\"save\",\"version\":{},\"group\":\"preferences\",\"scope\":\"global\"}}",
        number(&drafted, "version")
    );
    let (status, saved) = editor.post("/api/action", &save);
    assert_eq!(status, 200, "{saved}");
    assert_eq!(lab.b.preferences(), "icons = off\n");
    assert_eq!(lab.a.files(), before_a, "host A's own settings were not touched");

    // Done in the page ends the editor on B and the forward on A.
    let port = editor.port;
    let (status, _) = editor.post("/api/close", &format!("{{\"version\":{}}}", number(&saved, "version")));
    assert_eq!(status, 204);
    assert_cleaned_up(&lab, port, &editor.ended());

    // Ctrl-C on host A does the same.
    let again = Forwarded::open(&lab);
    assert!(again.state().contains("\"icons\":\"off\""), "the reopened editor reads what was saved on host B");
    let pid = libc_pid(&again.process);
    assert!(Command::new("/bin/kill").args(["-INT", &pid]).status().unwrap().success());
    let port = again.port;
    assert_cleaned_up(&lab, port, &again.ended());
    assert_eq!(lab.a.files(), before_a);
}

#[test]
#[ignore = "needs an installed Rex; run with --ignored"]
fn losing_the_forward_or_host_does_not_report_a_successful_editor_session() {
    let mut lab = Lab::start();
    let before = lab.a.files();
    for stop_host in [false, true] {
        let mut editor = Forwarded::open(&lab);
        assert!(editor.state().contains("\"forwarded\":true"));
        if stop_host {
            assert!(lab.b.stop());
        } else {
            lab.b.rex(&["forward", "close", "-s", "milo-settings", "--all"]);
        }

        assert!(soon(|| editor.process.try_wait().unwrap().is_some()), "forward stayed open");
        assert!(TcpStream::connect(("127.0.0.1", editor.port)).is_err());
        assert_eq!(lab.a.files(), before);
        assert!(!editor.process.wait().unwrap().success(), "{}", editor.said());
        if !stop_host {
            assert_cleaned_up(&lab, editor.port, &editor.said());
        }
    }
}

fn libc_pid(child: &Child) -> String {
    child.id().to_string()
}

#[test]
#[ignore = "needs an installed Rex; run with --ignored"]
fn a_host_that_is_not_there_or_has_no_milo_is_said_plainly_and_leaves_nothing() {
    let lab = Lab::start();
    let missing = format!("unix://{}", lab.root.join("nobody.sock").display());
    let (ok, said) = lab.a.milo_in(&lab.a.root, &["web", "--no-open", "--host", &missing]);
    assert!(!ok && said.contains("could not reach a Rex server at"), "{said}");
    let (ok, said) = lab.a.milo_in(&lab.a.root, &["web", "--no-open", "--host", &lab.a.socket()]);
    assert!(!ok && said.contains("is this computer's own Rex server"), "{said}");

    // Host B without Milo on its search path: the session it started is ended again.
    fs::remove_file(lab.b.root.join("bin/milo")).unwrap();
    let (ok, said) = lab.a.milo_in(&lab.a.root, &["web", "--no-open", "--host", &lab.b.socket()]);
    assert!(!ok && said.contains("did not start its editor"), "{said}");
    assert!(said.contains("closed the forward and the editor on"), "{said}");
    assert!(!lab.b.rex(&["ls"]).contains("milo-settings"), "{}", lab.b.rex(&["ls"]));
    assert!(lab.a.files().is_empty() && lab.b.files().is_empty());
}

#[test]
#[ignore = "needs an installed Rex; run with --ignored"]
fn chosen_settings_travel_from_one_host_to_the_other_and_come_back_out() {
    let lab = Lab::start();
    let (a, b) = (&lab.a, &lab.b);
    // Host A has global choices, a layout, an override for the project, and things that must stay.
    fs::create_dir_all(a.config().join("layouts")).unwrap();
    fs::write(
        a.config().join("preferences.conf"),
        "icons = plain\ntext_git_marks = true\nusage_codex = hide\nusage_openrouter = always\ntab_titles = true\n",
    )
    .unwrap();
    fs::write(a.config().join("layouts/global"), "milo-layout-v1 global\nv1 r 0.30 0.70 2 ohu 110\n").unwrap();
    fs::write(a.config().join("usage-accounts.conf"), "MILO_OPENROUTER_KEY=sk-never-travels\n").unwrap();
    fs::write(a.config().join("alerts.conf"), "telegram_chat = -100never-travels\n").unwrap();
    a.milo(&lab.project_a, &["doctor"]);
    let (ok, said) = a.milo_in(&lab.project_a, &["settings", "export", "workflow", "--scope", "worktree"]);
    assert!(!ok && said.contains("no settings of its own"), "{said}");
    // The outline's own toggle is what makes a worktree override; write the file it writes.
    let prefs_listing = a.milo(&lab.project_a, &["settings", "export", "all"]);
    assert!(prefs_listing.contains("[global]") && !prefs_listing.contains("[project]"), "{prefs_listing}");
    let before_a = a.files();

    let bundle = lab.root.join("bundle.txt");
    let exported = a
        .milo(&lab.project_a, &["settings", "export", "appearance,usage,layout", "--output", bundle.to_str().unwrap()]);
    assert!(exported.contains("Exported appearance, usage, layout"), "{exported}");
    let text = fs::read_to_string(&bundle).unwrap();
    for left in ["never-travels", "openrouter", "tab_titles", "telegram", a.home().to_str().unwrap()] {
        assert!(!text.contains(left), "{left} is in the bundle:\n{text}");
    }
    assert_eq!(fs::metadata(&bundle).unwrap().permissions().mode() & 0o777, 0o600);
    let (ok, said) = a.milo_in(&lab.project_a, &["settings", "export", "all", "--output", bundle.to_str().unwrap()]);
    assert!(!ok && said.contains("never replaces a file"), "{said}");

    // Host B had choices of its own. A preview changes nothing.
    fs::create_dir_all(b.config()).unwrap();
    fs::write(
        b.config().join("preferences.conf"),
        "# host b\nicons = off\nfollow_activity = false\nusage_devin = always\n",
    )
    .unwrap();
    let before_b = b.files();
    let preview = b.milo(&lab.project_b, &["settings", "import", bundle.to_str().unwrap()]);
    assert!(preview.contains("Icons: off → plain") && preview.contains("Nothing was changed."), "{preview}");
    assert_eq!(b.files(), before_b);

    // Applied: host B has A's chosen settings, keeps its own for the rest, and A is as it was.
    let applied = b.milo(&lab.project_b, &["settings", "import", bundle.to_str().unwrap(), "--apply"]);
    assert!(applied.contains("Changed 4 settings."), "{applied}");
    let prefs = b.preferences();
    for line in [
        "# host b",
        "icons = plain",
        "text_git_marks = true",
        "usage_codex = hide",
        "follow_activity = false",
        "usage_devin = always",
    ] {
        assert!(prefs.lines().any(|kept| kept == line), "{line} is missing on host B:\n{prefs}");
    }
    assert!(!prefs.contains("openrouter") && !prefs.contains("tab_titles"), "{prefs}");
    assert_eq!(
        fs::read_to_string(b.config().join("layouts/global")).unwrap(),
        "milo-layout-v1 global\nv1 r 0.30 0.70 2 ohu 110\n"
    );
    assert!(!b.config().join("usage-accounts.conf").exists() && !b.config().join("alerts.conf").exists());
    let doctor = b.milo(&lab.project_b, &["doctor"]);
    assert!(doctor.contains("Icons: Plain; Git text marks: on"), "host B's panes read the imported choices:\n{doctor}");
    assert_eq!(a.files(), before_a, "host A was only read");

    // Undo on host B puts its own files back, byte for byte.
    let undone = b.milo(&lab.project_b, &["settings", "undo"]);
    assert!(undone.contains("Took back the last import"), "{undone}");
    assert_eq!(b.files(), before_b);

    // The same bundle through host B's forwarded editor, opened on host A.
    let editor = Forwarded::open(&lab);
    let preview = format!("{{\"action\":\"preview\",\"bundle\":{},\"here\":null}}", quoted(&text));
    let (status, plan) = editor.post("/api/transfer", &preview);
    assert_eq!(status, 200, "{plan}");
    assert_eq!(number(&plan, "changes"), 4, "{plan}");
    assert_eq!(b.files(), before_b, "a preview through the editor writes nothing");
    let basis = plan.split_once("\"basis\":\"").unwrap().1.split('"').next().unwrap().to_owned();
    // Someone saves on host B between the preview and the import.
    fs::write(b.config().join("preferences.conf"), "# host b\nicons = nerd\n").unwrap();
    let version = number(&editor.state(), "version");
    let apply = |basis: &str| {
        format!(
            "{{\"action\":\"apply\",\"bundle\":{},\"here\":null,\"basis\":\"{basis}\",\"version\":{version}}}",
            quoted(&text)
        )
    };
    let (status, stale) = editor.post("/api/transfer", &apply(&basis));
    assert_eq!(status, 409, "{stale}");
    assert_eq!(b.preferences(), "# host b\nicons = nerd\n", "a stale import keeps nothing");
    let (_, plan) = editor.post("/api/transfer", &preview);
    let basis = plan.split_once("\"basis\":\"").unwrap().1.split('"').next().unwrap().to_owned();
    let (status, state) = editor.post("/api/transfer", &apply(&basis));
    assert_eq!(status, 200, "{state}");
    assert!(state.contains("\"transfer\":{\"undo\":true}") && state.contains("\"icons\":\"plain\""), "{state}");
    assert!(b.preferences().contains("icons = plain"));
    assert_eq!(a.files(), before_a, "host A was only read");
    let undo = format!("{{\"action\":\"undo\",\"version\":{}}}", number(&state, "version"));
    let (status, state) = editor.post("/api/transfer", &undo);
    assert_eq!(status, 200, "{state}");
    assert_eq!(b.preferences(), "# host b\nicons = nerd\n");
    let (status, _) = editor.post("/api/close", &format!("{{\"version\":{}}}", number(&state, "version")));
    assert_eq!(status, 204);
    let port = editor.port;
    assert_cleaned_up(&lab, port, &editor.ended());
}

#[test]
#[ignore = "needs an installed Rex; run with --ignored"]
fn a_projects_settings_follow_its_repository_to_a_different_path_on_the_other_host() {
    let lab = Lab::start();
    let (a, b) = (&lab.a, &lab.b);
    // Milo's own editor on host A saves an override for the project there.
    let override_on_a = |field: &str, value: &str| {
        let mut web = a
            .command(&a.milo)
            .current_dir(&lab.project_a)
            .env_remove("REX_SERVER")
            .args(["web", "--no-open", "--port", "0"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut link = String::new();
        BufReader::new(web.stdout.take().unwrap()).read_line(&mut link).unwrap();
        let (port, token) = link.trim().strip_prefix("http://127.0.0.1:").unwrap().split_once("/#").unwrap();
        let (port, authority) = (port.parse().unwrap(), format!("127.0.0.1:{port}"));
        let post = |path: &str, body: &str| {
            http(port, &authority, "POST", path, token, Some(&format!("http://{authority}")), Some(body))
        };
        let (_, state) = http(port, &authority, "GET", "/api/state", token, None, None);
        let set = format!(
            "{{\"action\":\"set\",\"version\":{},\"field\":\"{field}\",\"value\":{value}}}",
            number(&state, "version")
        );
        let (status, state) = post("/api/action", &set);
        assert_eq!(status, 200, "{state}");
        let save = format!(
            "{{\"action\":\"save\",\"version\":{},\"group\":\"preferences\",\"scope\":\"worktree\"}}",
            number(&state, "version")
        );
        let (status, state) = post("/api/action", &save);
        assert_eq!(status, 200, "{state}");
        let (status, _) = post("/api/close", &format!("{{\"version\":{}}}", number(&state, "version")));
        assert_eq!(status, 204);
        assert!(web.wait().unwrap().success());
    };
    override_on_a("workflow.changes_only", "true");
    let before_a = a.files();

    let bundle = lab.root.join("project.txt");
    a.milo(
        &lab.project_a,
        &["settings", "export", "workflow", "--scope", "worktree", "--output", bundle.to_str().unwrap()],
    );
    let text = fs::read_to_string(&bundle).unwrap();
    assert!(
        text.contains("[project]\nname = app\nhome = src/app\nrepository = ") && text.contains("changes_only = true"),
        "{text}"
    );
    assert!(!text.contains(a.home().to_str().unwrap()), "{text}");

    // On host B the repository is at another path under another name, and is found by its history.
    let preview = b.milo(&lab.project_b, &["settings", "import", bundle.to_str().unwrap()]);
    assert!(preview.contains(&format!("Project app → {} (same repository)", lab.project_b.display())), "{preview}");
    b.milo(&lab.project_b, &["settings", "import", bundle.to_str().unwrap(), "--apply"]);
    let doctor = b.milo(&lab.project_b, &["doctor"]);
    assert!(
        doctor.contains("Preferences: worktree over global default") && doctor.contains("outline: changes only"),
        "{doctor}"
    );
    assert!(b.preferences().is_empty(), "host B's global defaults were not touched");
    assert_eq!(a.files(), before_a, "host A was only read");

    // An unrelated checkout that happens to share the name is never chosen for the person.
    let unrelated = b.home().join("elsewhere/app");
    fs::create_dir_all(&unrelated).unwrap();
    b.git(&unrelated, &["init", "-q", "--template=", "-b", "main"]);
    fs::write(unrelated.join("README"), "another project\n").unwrap();
    b.git(&unrelated, &["add", "README"]);
    b.git(&unrelated, &["commit", "-q", "-m", "another start"]);
    b.milo(&lab.project_b, &["settings", "undo"]);
    let preview = b.milo(&unrelated, &["settings", "import", bundle.to_str().unwrap(), "--apply"]);
    assert!(preview.contains("Project app: not applied.") && preview.contains("--map app=DIR"), "{preview}");
    assert!(b.files().is_empty(), "nothing was written for a project that was not matched");
    let chosen = b.milo(&unrelated, &["settings", "import", bundle.to_str().unwrap(), "--map", "app=.", "--apply"]);
    assert!(chosen.contains(&format!("Project app → {} (chosen by you)", unrelated.display())), "{chosen}");
}
