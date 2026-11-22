//! Runs Milo's Rex integration against a disposable Rex server.
//!
//! The server, its settings and state, the home directory and two Git
//! worktrees live in one throwaway folder, `/tmp/milo-rex-lab-PID-N`. Every
//! `rex` this test runs names that server's socket and cannot start another
//! server. Programs the lab's server starts inherit the lab's home and its
//! `REX_SERVER`; the test checks that in a lab terminal and on the listener's
//! command line. A shell tab on macOS runs under `login`, which gives it the
//! developer's home again, so `ZDOTDIR` points its zsh at the lab's startup
//! files, and the test checks that the shell read them. The Rex a developer
//! is working in is never asked anything, and none of its settings, sessions
//! or tabs change.
//!
//! The test needs an installed `rex`, so it is ignored by default:
//!
//! ```sh
//! cargo test --test rex_lab -- --ignored --nocapture
//! ```
//!
//! Set `MILO_TEST_REX` if `rex` is not in `REX_BIN_DIR` or on `PATH`, and
//! `MILO_TEST_BINARY` to check a Milo built from another revision. A test
//! process that is killed cannot clean up: stop the server named in the
//! folder's `bin/rex` and remove the folder.
//!
//! A new shell tab gets its rail or the test fails; no tab is opened in the
//! place of one that did not. Besides the tabs it opens one at a time, the
//! test opens several back to back, with no pause between them, more than once.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
/// A rail normally appears well within a second of its tab. A tab still
/// without one after this long has been passed over, not kept waiting.
const PATIENCE: Duration = Duration::from_secs(10);
/// How many tabs are opened back to back, and how many times that is done.
const BURST: usize = 4;
const BURSTS: usize = 3;
const RAIL: [&str; 3] = ["milo:outline", "milo:history", "milo:usage"];
const SAVED_WIDTH: &str = "0.200000";

/// One pane of a tab, as `tests/rex_lab/view.lua` prints it.
#[derive(Debug)]
struct Pane {
    block: String,
    label: String,
    /// `milo` for the binary under test, `shell` for a login shell, else `other`.
    program: String,
    dir: String,
}

#[derive(Debug)]
struct Tab {
    /// The left column's share of the tab when the tab is split side by side.
    left: String,
    panes: Vec<Pane>,
}

impl Tab {
    /// Whether this is one shell beside a full rail of Milo panes started in `dir`.
    fn has_rail_for(&self, dir: &Path) -> bool {
        let [outline, history, usage, shell] = self.panes.as_slice() else { return false };
        [outline, history, usage]
            .iter()
            .zip(RAIL)
            .all(|(pane, label)| pane.label == label && pane.program == "milo" && Path::new(&pane.dir) == dir)
            && shell.program == "shell"
    }

    /// Whether every pane of the rail runs `program`.
    fn rail_runs(&self, program: &str) -> bool {
        RAIL.iter().all(|label| self.pane(label).program == program)
    }

    fn labels(&self) -> Vec<&str> {
        self.panes.iter().map(|pane| pane.label.as_str()).collect()
    }

    fn pane(&self, label: &str) -> &Pane {
        self.panes.iter().find(|pane| pane.label == label).unwrap_or_else(|| panic!("no {label} pane in {self:?}"))
    }
}

/// Everything the server shows: tabs by window ID, and every name a person can set.
#[derive(Debug, Default)]
struct View {
    tabs: BTreeMap<String, Tab>,
    /// Session and window labels by ID.
    names: BTreeMap<String, String>,
}

impl View {
    fn parse(text: &str) -> Self {
        let mut view = Self::default();
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["session", id, label] => {
                    view.names.insert((*id).into(), (*label).into());
                }
                ["window", _, id, label, left, panes @ ..] => {
                    let panes = panes
                        .iter()
                        .map(|pane| {
                            let [block, label, program, dir] = pane.split('|').collect::<Vec<_>>()[..] else {
                                panic!("unexpected pane {pane:?}");
                            };
                            Pane { block: block.into(), label: label.into(), program: program.into(), dir: dir.into() }
                        })
                        .collect();
                    view.names.insert((*id).into(), (*label).into());
                    view.tabs.insert((*id).into(), Tab { left: (*left).into(), panes });
                }
                _ => panic!("unexpected line from view.lua: {line:?}"),
            }
        }
        view
    }

    fn tab(&self, id: &str) -> &Tab {
        self.tabs.get(id).unwrap_or_else(|| panic!("no tab {id} in {self:?}"))
    }
}

/// The browser editor of a `milo web` running in a tab of the disposable server.
struct Editor {
    address: String,
    token: String,
    version: u64,
}

impl Editor {
    /// Sends what the editor's page would and returns the body of a successful reply.
    fn request(&self, path: &str, body: Option<&str>) -> String {
        let Self { address, token, .. } = self;
        let mut socket = TcpStream::connect(address).unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        let method = if body.is_some() { "POST" } else { "GET" };
        let body = body.unwrap_or_default();
        write!(
            socket,
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\n\
             Authorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut reply = String::new();
        socket.read_to_string(&mut reply).unwrap();
        let (head, state) = reply.split_once("\r\n\r\n").unwrap_or((&reply, ""));
        assert!(head.starts_with("HTTP/1.1 200 "), "{method} {path} {body}: {reply}");
        state.to_owned()
    }

    /// Applies one editor action to the draft the browser would be showing.
    fn act(&mut self, action: &str) -> String {
        let state = self.request("/api/action", Some(&format!("{{\"version\":{},{action}}}", self.version)));
        self.version += 1;
        state
    }
}

struct Lab {
    root: PathBuf,
    /// The installed Rex. The test itself reaches it only through the wrapper in `bin`.
    rex: PathBuf,
    milo: PathBuf,
    server: Option<Child>,
    app: Option<Child>,
    /// What each session and tab should be called: the name given when it was
    /// made or renamed here, or Rex's own name for a tab nobody named.
    names: RefCell<BTreeMap<String, String>>,
    /// Tabs Milo has no business arranging, which must stay one pane.
    alone: RefCell<Vec<String>>,
}

impl Lab {
    fn start() -> Self {
        let rex = installed_rex();
        let milo =
            std::env::var_os("MILO_TEST_BINARY").map_or_else(|| env!("CARGO_BIN_EXE_milo").into(), PathBuf::from);
        let milo = milo.canonicalize().unwrap_or_else(|e| panic!("no Milo at {}: {e}", milo.display()));
        // A Unix socket's path is limited to about a hundred bytes, which the
        // usual temporary folder on macOS nearly uses up on its own. Each
        // lab of this process has a folder, and so a server, of its own.
        static LABS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let lab = LABS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("milo-rex-lab-{}-{lab}", std::process::id());
        let root = Path::new("/tmp").canonicalize().unwrap().join(name);
        let _ = fs::remove_dir_all(&root);
        for dir in ["bin", "state", "tmp", "home/.config", "home/.local/state"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        // The editor's link, which is a password, is written here.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        // The lab's shell reads no system-wide settings and none of the
        // developer's, so what it runs as it starts, and how long that takes,
        // do not depend on the machine. It notes where it found its settings,
        // then copies itself several times over, as prompts and plugins make
        // a shell do while the listener is still arranging its new tab.
        fs::write(root.join("home/.zshenv"), "unsetopt global_rcs\nunset HISTFILE\n").unwrap();
        let startup = format!(
            "print -r -- \"$ZDOTDIR\" \"$XDG_CONFIG_HOME\" \"$REX_SERVER\" >>'{}'\n\
             zmodload zsh/zselect && repeat 8 ( zselect -t 1 )\n",
            root.join("shells").display()
        );
        fs::write(root.join("home/.zshrc"), startup).unwrap();
        let (names, alone) = Default::default();
        let mut lab = Self { root, rex, milo, server: None, app: None, names, alone };
        for path in [&lab.rex, &lab.milo, &lab.root] {
            assert!(!path.to_string_lossy().contains('\''), "cannot quote {} for sh", path.display());
        }
        lab.write_wrapper();
        lab.start_server();
        lab
    }

    fn socket(&self) -> String {
        format!("unix://{}", self.root.join("s.sock").display())
    }

    /// The `rex` this test and the lab's server find first. It can reach only
    /// the lab's server and can never start one, here or anywhere else.
    fn write_wrapper(&self) {
        let script =
            format!("#!/bin/sh\nexec '{}' --autostart=false -S '{}' \"$@\"\n", self.rex.display(), self.socket());
        let path = self.root.join("bin/rex");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The shell of the lab's terminals: zsh where there is one.
    fn shell() -> &'static str {
        ["/bin/zsh", "/bin/bash"].into_iter().find(|shell| Path::new(shell).exists()).unwrap_or("/bin/sh")
    }

    /// Nothing from the developer's shell reaches the lab: not their home, their
    /// settings folders, nor the Rex session this test may itself be running in.
    fn command(&self, program: impl AsRef<Path>) -> Command {
        let home = self.root.join("home");
        let mut command = Command::new(program.as_ref());
        command
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.root.join("bin").display()))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("HOME", &home)
            // `login` gives a shell tab the developer's home, but leaves this.
            .env("ZDOTDIR", &home)
            .env("HISTFILE", home.join("history"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("SHELL", Self::shell())
            .env("TERM", "xterm-256color")
            .env("REX_SERVER", self.socket())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(&self.root)
            .stdin(Stdio::null());
        command
    }

    fn log(&self, name: &str) -> File {
        File::options().create(true).append(true).open(self.root.join(name)).unwrap()
    }

    fn start_server(&mut self) {
        let config = self.root.join("home/.config/rex/init.lua");
        let server = self
            .command(&self.rex)
            .args(["server", "run", "--listen", &self.socket(), "--state-dir"])
            .arg(self.root.join("state"))
            .arg("--config")
            .arg(config)
            .stdout(self.log("server.log"))
            .stderr(self.log("server.log"))
            .spawn()
            .unwrap();
        self.server = Some(server);
        let ready = soon(|| self.rex_output(&["whoami", "--json"]).is_ok_and(|output| output.status.success()));
        assert!(ready, "the disposable Rex server did not start:\n{}", self.read("server.log"));
        let identity = self.rex(&["whoami", "--json"]);
        assert!(identity.contains(&format!("\"endpoint\": \"{}\"", self.socket())), "{identity}");
    }

    /// Asks the server to save its sessions and end, as quitting Rex does.
    fn stop_server(&mut self) {
        self.rex(&["server", "stop"]);
        for child in [&mut self.server, &mut self.app] {
            let ended = soon(|| child.as_mut().is_none_or(|child| child.try_wait().unwrap().is_some()));
            assert!(ended, "a lab process outlived its server");
            *child = None;
        }
    }

    fn start_app(&mut self) {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_lab/app.lua");
        let _ = fs::remove_file(self.root.join("app.log"));
        let app = self
            .command(self.root.join("bin/rex"))
            .args(["do", script, "--all"])
            .stdout(self.log("app.log"))
            .stderr(self.log("app.log"))
            .spawn()
            .unwrap();
        self.app = Some(app);
        assert!(soon(|| self.read("app.log").contains("app ready")), "{}", self.read("app.log"));
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }

    fn rex_output(&self, args: &[&str]) -> std::io::Result<Output> {
        self.command(self.root.join("bin/rex")).args(args).output()
    }

    fn rex(&self, args: &[&str]) -> String {
        let output = self.rex_output(args).unwrap();
        assert!(output.status.success(), "rex {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    fn milo(&self, args: &[&str]) -> String {
        let output = self.command(&self.milo).args(args).output().unwrap();
        assert!(output.status.success(), "milo {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let status = self
            .command("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.name=Milo Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// Two worktrees of one repository, each with a file the other lacks.
    fn worktrees(&self) -> [PathBuf; 2] {
        let [first, second] = ["first", "second"].map(|name| self.root.join(name));
        fs::create_dir(&first).unwrap();
        self.git(&first, &["init", "-q", "--template=", "-b", "main"]);
        fs::write(first.join("shared.txt"), "shared\n").unwrap();
        self.git(&first, &["add", "-A"]);
        self.git(&first, &["commit", "-q", "-m", "Start"]);
        self.git(&first, &["worktree", "add", "-q", "-b", "second", second.to_str().unwrap()]);
        for dir in [&first, &second] {
            let name = dir.file_name().unwrap().to_str().unwrap();
            fs::write(dir.join(format!("only-in-{name}.txt")), "here\n").unwrap();
        }
        [first, second]
    }

    fn view(&self) -> View {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_lab/view.lua");
        View::parse(&self.rex(&["do", script, &format!("milo={}", self.milo.display())]))
    }

    /// The view, once no name has changed and no tab Milo should leave alone
    /// has gained a pane.
    fn check(&self) -> View {
        let view = self.view();
        assert_eq!(view.names, *self.names.borrow());
        for tab in self.alone.borrow().iter() {
            assert_eq!(view.tab(tab).panes.len(), 1, "{tab} was arranged: {:?}", view.tab(tab));
        }
        view
    }

    /// Waits for the listener to give each tab its worktree's rail. A tab it
    /// leaves as it was fails the test.
    fn get_rails(&self, tabs: &[(&str, &Path)]) {
        let began = Instant::now();
        loop {
            let view = self.view();
            let bare: Vec<_> = tabs.iter().filter(|(tab, dir)| !view.tab(tab).has_rail_for(dir)).collect();
            if bare.is_empty() {
                return;
            }
            assert!(began.elapsed() < PATIENCE, "the listener left new tabs without a rail: {bare:?} in {view:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Opens a shell tab in `dir`, as the app's New Tab does, and sees it get its rail.
    fn rail_tab(&self, session: &str, dir: &Path) -> String {
        let tab = self.new_tab(session, dir);
        self.get_rails(&[(&tab, dir)]);
        tab
    }

    /// Makes a session in `dir`, sees its first tab get its rail, and returns
    /// the IDs of that session and tab.
    fn rail_session(&self, name: &str, tab_name: &str, dir: &Path) -> [String; 2] {
        let ids = self.new_session(name, tab_name, dir);
        self.get_rails(&[(&ids[1], dir)]);
        ids
    }

    fn close_tab(&self, session: &str, tab: &str) {
        self.rex(&["window", "close", "-s", session, tab]);
        self.names.borrow_mut().remove(tab);
    }

    /// Makes a session whose first tab has a name, and returns both IDs.
    fn new_session(&self, name: &str, tab_name: &str, dir: &Path) -> [String; 2] {
        let made = self.rex(&["new", name, "-c", dir.to_str().unwrap(), "--window", tab_name, "--json"]);
        let ids = [id(&made, "session_id"), id(&made, "window_id")];
        self.names.borrow_mut().extend([(ids[0].clone(), name.to_owned()), (ids[1].clone(), tab_name.to_owned())]);
        ids
    }

    /// Records a tab made without a name under the name Rex gives such a tab.
    fn unnamed(&self, made: &str) -> String {
        let tab = id(made, "window_id");
        self.names.borrow_mut().insert(tab.clone(), format!("Window {}", &tab[tab.len() - 8..]));
        tab
    }

    /// Opens a shell tab as the app's New Tab does, without taking focus.
    fn new_tab(&self, session: &str, dir: &Path) -> String {
        self.unnamed(&self.rex(&[
            "window",
            "new",
            "-s",
            session,
            "-c",
            dir.to_str().unwrap(),
            "--focus=false",
            "--json",
        ]))
    }

    fn rename_tab(&self, session: &str, tab: &str, name: &str) {
        self.rex(&["window", "rename", "-s", session, tab, name]);
        self.names.borrow_mut().insert(tab.to_owned(), name.to_owned());
    }

    fn rename_session(&self, session: &str, name: &str) {
        self.rex(&["session", "rename", session, name]);
        self.names.borrow_mut().insert(session.to_owned(), name.to_owned());
    }

    /// Drags the divider between the rail and the shell, as a person would.
    fn drag_rail(&self, session: &str, tab: &Tab, left: &str) {
        let (rail, shell) = (&tab.pane(RAIL[0]).block, &tab.panes.last().unwrap().block);
        let request = format!(
            "{{\"session_id\":\"{session}\",\"direction\":\"horizontal\",\"before_block_id\":\"{rail}\",\
             \"after_block_id\":\"{shell}\",\"ratio\":{left}}}"
        );
        self.rex(&["api", "call", "session.resize_pane", &request]);
    }

    /// The rail width Milo would give a new tab opened in `dir`.
    fn remembered_width(&self, dir: &Path) -> String {
        let profile = self.milo(&["layout-profile", "read", dir.to_str().unwrap()]);
        let width: f64 = profile.split(' ').nth(2).and_then(|width| width.parse().ok()).expect("a rail width");
        format!("{width:.6}")
    }

    fn remembers(&self, dir: &Path, width: &str) {
        assert!(soon(|| self.remembered_width(dir) == width), "{} for {}", self.remembered_width(dir), dir.display());
    }

    fn screen(&self, session: &str, pane: &Pane) -> String {
        self.rex(&["capture", "-s", session, "-b", &pane.block, "--trim"])
    }

    /// A terminal of the lab's server must be in the lab too: its home, its
    /// settings and its server are what Milo's panes will use. A shell tab
    /// must have started from the lab's files, not the developer's.
    fn check_terminals(&self, session: &str) {
        let (home, seen) = (self.root.join("home"), self.root.join("terminal"));
        let print = format!(
            "printf '%s\\n' \"$HOME\" \"$XDG_CONFIG_HOME\" \"$XDG_STATE_HOME\" \"$REX_SERVER\" >'{}'",
            seen.display()
        );
        let made = self.rex(&["run", "-s", session, "--focus=false", "--json", "--wait", "--", "sh", "-c", &print]);
        let _ = self.rex_output(&["window", "close", "-s", session, &id(&made, "window_id")]);
        let home = home.display();
        assert_eq!(self.read("terminal"), format!("{home}\n{home}/.config\n{home}/.local/state\n{}\n", self.socket()));
        if Self::shell().ends_with("/zsh") {
            let started = format!("{home} {home}/.config {}", self.socket());
            let shells = || self.read("shells");
            assert!(soon(|| !shells().is_empty()) && shells().lines().all(|shell| shell == started), "{}", shells());
        }
    }

    /// The process ID and command line of each layout listener serving the lab.
    fn listeners(&self) -> Vec<(String, String)> {
        let processes = self.command("ps").args(["-axww", "-o", "pid=,command="]).output().unwrap();
        let config = format!("{}/", self.root.display());
        String::from_utf8_lossy(&processes.stdout)
            .lines()
            .filter(|process| process.contains(&config) && process.contains("listener.lua"))
            .filter_map(|process| process.trim_start().split_once(' '))
            .map(|(pid, command)| (pid.to_owned(), command.to_owned()))
            .collect()
    }

    /// Waits for the one listener to hear `session`. Like the test's own
    /// calls, it must name the lab's server and be unable to start another.
    fn listener_hears(&self, session: &str) {
        let hears = || {
            let [(pid, command)] = &self.listeners()[..] else { return false };
            assert!(
                command.contains("--autostart=false") && command.contains(&format!("-S {}", self.socket())),
                "{command}"
            );
            let clients: String =
                self.rex(&["api", "call", "-s", session, "session.list_clients"]).split_whitespace().collect();
            [',', '}'].iter().any(|end| clients.contains(&format!("\"pid\":{pid}{end}")))
        };
        assert!(soon(hears), "no single listener hears {session}: {:?}", self.listeners());
    }

    /// Saves the default layout with new shell tabs arranged, through the
    /// browser editor of a `milo web` started in a tab of `session`.
    fn install(&self, session: &str, dir: &Path) {
        self.milo(&["setup", "--skip-onboarding"]);
        let serve = format!(
            "PATH='{}':\"$PATH\" exec '{}' web --no-open --port 0 >'{}' 2>&1",
            self.root.join("bin").display(),
            self.milo.display(),
            self.root.join("web.log").display()
        );
        let made = self.rex(&[
            "run",
            "-s",
            session,
            "-c",
            dir.to_str().unwrap(),
            "--focus=false",
            "--json",
            "--",
            "sh",
            "-c",
            &serve,
        ]);
        assert!(soon(|| self.read("web.log").contains('\n')), "milo web printed {:?}", self.read("web.log"));
        let printed = self.read("web.log");
        let link = printed.lines().next().and_then(|line| line.strip_prefix("http://"));
        let (address, token) = link.and_then(|link| link.split_once("/#")).unwrap_or_else(|| panic!("{printed}"));
        let mut editor = Editor { address: address.into(), token: token.into(), version: 0 };
        let state = editor.request("/api/state", None);
        editor.version =
            state.strip_prefix("{\"version\":").and_then(|rest| rest.split(',').next()?.parse().ok()).unwrap();
        editor.act(r#""action":"set","field":"layout.auto_arrange","value":true"#);
        let saved = editor.act(r#""action":"save","group":"layout","scope":"global""#);
        assert!(saved.contains("\"auto_arrange\":true") && saved.contains("\"error\":false"), "{saved}");
        self.rex(&["window", "close", "-s", session, &id(&made, "window_id")]);
        let health = self.milo(&["doctor"]);
        assert!(health.contains("Server actions: loaded") && health.contains("Layout listener: running"), "{health}");
        self.listener_hears(session);
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        // Nothing here may panic: a second panic would skip the rest.
        if self.server.is_some() {
            let _ = self.rex_output(&["server", "stop"]);
        }
        for mut child in [self.server.take(), self.app.take()].into_iter().flatten() {
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until && child.try_wait().is_ok_and(|status| status.is_none()) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        // A listener normally ends with its server. This ends one that did not.
        let _ = self.command(&self.milo).args(["listen", "--stop"]).stderr(Stdio::null()).status();
        if std::thread::panicking() {
            let log = self.read("server.log");
            let problems: Vec<&str> =
                log.lines().filter(|line| line.contains(" WARN ") || line.contains(" ERROR ")).collect();
            let last = &problems[problems.len().saturating_sub(20)..];
            eprintln!("the disposable Rex server's last warnings:\n{}", last.join("\n"));
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn installed_rex() -> PathBuf {
    if let Some(named) = std::env::var_os("MILO_TEST_REX").map(PathBuf::from) {
        assert!(named.is_file(), "MILO_TEST_REX names no file: {}", named.display());
        return named;
    }
    let beside_server = std::env::var_os("REX_BIN_DIR").map(|dir| Path::new(&dir).join("rex"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let on_path = std::env::split_paths(&path).map(|dir| dir.join("rex"));
    beside_server
        .into_iter()
        .chain(on_path)
        .find(|rex| rex.is_file())
        .expect("install Rex, or set MILO_TEST_REX to its rex executable")
}

/// The first value of an ID field in Rex's JSON. IDs never need escaping.
fn id(json: &str, field: &str) -> String {
    let (_, rest) = json.split_once(&format!("\"{field}\"")).unwrap_or_else(|| panic!("no {field} in {json}"));
    rest.split('"').nth(1).unwrap().to_owned()
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

/// The lab with Milo installed, two worktrees, and a session that began in the first.
struct Scene {
    lab: Lab,
    first: PathBuf,
    second: PathBuf,
    session: String,
}

impl Scene {
    /// Before Milo is installed, a named tab and an unnamed one show the names
    /// Rex itself gives. Neither is arranged later: both were already open.
    fn start() -> Self {
        let mut lab = Lab::start();
        let [first, second] = lab.worktrees();
        let [session, named] = lab.new_session("lab", "first tab", &first);
        lab.start_app();
        lab.check_terminals(&session);
        let unnamed = lab.new_tab(&session, &first);
        lab.alone.borrow_mut().extend([named, unnamed]);
        lab.check();
        lab.install(&session, &first);
        Self { lab, first, second, session }
    }

    /// A new shell tab in each worktree gets that worktree's rail at the saved width.
    fn new_tabs_get_their_worktrees_rail(&self) -> [String; 2] {
        let Self { lab, first, second, session } = self;
        let tabs = [lab.rail_tab(session, first), lab.rail_tab(session, second)];
        let view = lab.check();
        for (tab, own, other) in [(&tabs[0], "first", "second"), (&tabs[1], "second", "first")] {
            assert_eq!(view.tab(tab).left, SAVED_WIDTH);
            let outline = view.tab(tab).pane(RAIL[0]);
            let shows = |name: &str| lab.screen(session, outline).contains(&format!("only-in-{name}.txt"));
            assert!(soon(|| shows(own)) && !shows(other), "{}", lab.screen(session, outline));
        }
        tabs
    }

    /// A bare shell builtin emits no OSC 7, but Rex's shell process cwd
    /// still drives the Outline as the pane stays open.
    fn rail_follows_bare_cd(&self, tab: &str) {
        let Self { lab, first, second, session } = self;
        let view = lab.check();
        let tab = view.tab(tab);
        let outline = tab.pane(RAIL[0]);
        let shell = &tab.panes.last().unwrap().block;
        let reported = || lab.rex(&["block", "call", "-s", session, "-b", shell, "pwd"]);
        let pwd = reported();
        assert!(!pwd.contains("\"pwd\""), "{pwd}");
        for (dir, own, other) in [
            (second, "only-in-second.txt", "only-in-first.txt"),
            (first, "only-in-first.txt", "only-in-second.txt"),
        ] {
            let cwd = format!("cd -- '{}'", dir.display());
            lab.rex(&["send", "-s", session, "-b", shell, &cwd]);
            lab.rex(&["send-key", "-s", session, "-b", shell, "enter"]);
            assert!(
                soon(|| {
                    let screen = lab.screen(session, outline);
                    screen.contains(own) && !screen.contains(other)
                }),
                "Outline did not follow {cwd}:\n{}",
                lab.screen(session, outline)
            );
            let pwd = reported();
            assert!(!pwd.contains("\"pwd\""), "{pwd}");
        }
    }

    /// Tabs opened one straight after another, as holding down the New Tab
    /// key opens them, each get their own worktree's rail. Each round closes
    /// its tabs, so the next begins as it did.
    fn tabs_opened_back_to_back_each_get_their_rail(&self) {
        let Self { lab, first, second, session } = self;
        for _ in 0..BURSTS {
            let dirs = (0..BURST).map(|tab| if tab % 2 == 0 { first } else { second });
            let tabs: Vec<(String, &Path)> = dirs.map(|dir| (lab.new_tab(session, dir), dir.as_path())).collect();
            let tabs: Vec<(&str, &Path)> = tabs.iter().map(|(tab, dir)| (tab.as_str(), *dir)).collect();
            lab.get_rails(&tabs);
            let view = lab.check();
            for (tab, _) in &tabs {
                assert_eq!(view.tab(tab).left, SAVED_WIDTH);
                lab.close_tab(session, tab);
            }
        }
        lab.check();
    }

    /// A new session's first tab is arranged, under the name it was made
    /// with. A tab opened to run a command is not.
    fn a_new_session_gets_a_rail_and_a_command_tab_does_not(&self) -> String {
        let Self { lab, second, session, .. } = self;
        let command = lab.unnamed(&lab.rex(&["run", "-s", session, "--focus=false", "--json", "--", "sleep", "600"]));
        lab.alone.borrow_mut().push(command);
        let [other_session, _] = lab.rail_session("second-session", "second tab", second);
        lab.check();
        other_session
    }

    /// Dragging a rail's divider is remembered for that worktree only.
    fn a_drag_is_remembered_for_its_worktree(&self, dragged: &str) -> String {
        let Self { lab, first, second, session } = self;
        lab.drag_rail(session, lab.view().tab(dragged), "0.31");
        lab.remembers(first, "0.310000");
        assert_eq!(lab.remembered_width(second), SAVED_WIDTH);
        let (wide, narrow) = (lab.rail_tab(session, first), lab.rail_tab(session, second));
        let view = lab.check();
        assert_eq!((view.tab(&wide).left.as_str(), view.tab(&narrow).left.as_str()), ("0.310000", SAVED_WIDTH));
        wide
    }

    /// A name a person gives a tab or a session survives what Milo does next.
    fn given_names_are_kept(&self, tab: &str) -> View {
        let Self { lab, first, session, .. } = self;
        lab.rename_tab(session, tab, "my tab");
        lab.rename_session(session, "my session");
        lab.drag_rail(session, lab.view().tab(tab), "0.27");
        lab.remembers(first, "0.270000");
        let later = lab.rail_tab(session, first);
        let view = lab.check();
        assert_eq!(view.tab(&later).left, "0.270000");
        view
    }

    /// Quitting Rex ends the listener with it. Starting Rex again brings back
    /// every tab, name and divider, one listener, and rails for new tabs.
    fn a_restart_brings_everything_back(&mut self, before: &View, other_session: &str) -> [String; 2] {
        self.lab.stop_server();
        assert!(soon(|| self.lab.listeners().is_empty()), "{:?} outlived the server", self.lab.listeners());
        self.lab.start_server();
        self.lab.start_app();
        let Self { lab, first, second, session } = &*self;
        lab.listener_hears(session);
        let tabs = [lab.rail_tab(other_session, second), lab.rail_tab(session, first)];
        let view = lab.check();
        assert_eq!((view.tab(&tabs[0]).left.as_str(), view.tab(&tabs[1]).left.as_str()), (SAVED_WIDTH, "0.270000"));
        for (id, tab) in &before.tabs {
            let restored = view.tab(id);
            assert_eq!((restored.labels(), &restored.left), (tab.labels(), &tab.left), "{id} changed in the restart");
        }
        tabs
    }

    /// Rex decides whether a restored pane runs its program again. Either way
    /// Milo stays consistent: it remembers a drag only for a rail whose panes
    /// it is running, and still remembers the drag of a rail it has just made.
    fn a_restored_rail_is_remembered_only_if_milo_runs_in_it(
        &self,
        restored: &str,
        other_session: &str,
        new: [&str; 2],
    ) {
        let Self { lab, first, second, session } = self;
        let began = Instant::now();
        let view = loop {
            let view = lab.view();
            if view.tab(restored).rail_runs("milo") || view.tab(restored).rail_runs("shell") {
                break view;
            }
            assert!(began.elapsed() < WAIT, "restored panes run neither milo nor a shell: {:?}", view.tab(restored));
            std::thread::sleep(Duration::from_millis(100));
        };
        let running = view.tab(restored).rail_runs("milo");
        eprintln!("after the restart, restored rail panes run {}", if running { "milo" } else { "a shell, not milo" });
        lab.drag_rail(other_session, view.tab(new[0]), "0.33");
        lab.remembers(second, "0.330000");
        lab.drag_rail(session, view.tab(restored), "0.4");
        // The listener saves drags in order, so this later one settles last.
        lab.drag_rail(session, view.tab(new[1]), "0.29");
        lab.remembers(first, "0.290000");
        assert_eq!(lab.remembered_width(second), if running { "0.400000" } else { "0.330000" });
        lab.check();
        assert_eq!(lab.listeners().len(), 1);
    }

    /// Refusing an unconfirmed rebuild, cancelling, and then confirming all
    /// exercise the same restored blocks on the disposable server.
    fn a_restored_rail_needs_confirmation_to_rebuild(&self, restored: &str) {
        let Self { lab, second, session, .. } = self;
        let view = lab.view();
        if view.tab(restored).rail_runs("milo") {
            return;
        }
        let shell = view.tab(restored).panes.last().unwrap().block.clone();
        let unchanged = || {
            let after = lab.view();
            let tab = after.tab(restored);
            assert!(tab.rail_runs("shell"), "{tab:?}");
            assert!(
                tab.panes.iter().map(|pane| &pane.block).eq(view.tab(restored).panes.iter().map(|pane| &pane.block))
            );
        };
        let report = lab
            .command(&lab.milo)
            .current_dir(second)
            .env("REX_SESSION", session)
            .env("REX_BLOCK", &shell)
            .arg("doctor")
            .output()
            .unwrap();
        let report = String::from_utf8(report.stdout).unwrap();
        assert!(report.contains("Live sidebar: restored shells, not Milo panes."), "{report}");
        let arrange = lab.rex_output(&["do", "milo_layout", "-s", session, "-b", &shell]).unwrap();
        assert!(!arrange.status.success());
        let error = String::from_utf8(arrange.stderr).unwrap();
        assert!(error.contains("in a connected Rex app to confirm the rebuild"), "{error}");
        unchanged();

        // The lab's measuring client isn't a Rex app and cannot answer a
        // real picker. Stub only the answer in a one-off copy of its Lua.
        let generated = lab.read("home/.config/milo/rex.lua");
        let answer = |reply: &str| {
            let script = lab.root.join("restore-reply.lua");
            let lua = format!(
                r#"local rex = setmetatable({{ args = {{}}, ui = {{ pick = function(prompt)
                    assert(prompt.title == "Rebuild Milo sidebar?")
                    assert(prompt.items[2].subtitle:find("Closes 3 restored shells", 1, true))
                    {reply}
                end }} }}, {{ __index = rex }})
                {generated}"#
            );
            fs::write(&script, lua).unwrap();
            lab.rex(&["do", script.to_str().unwrap(), "--action", "milo_layout", "-s", session, "-b", &shell]);
        };
        answer("return nil, 'cancelled'");
        unchanged();
        answer("return 'rebuild'");
        lab.get_rails(&[(restored, second)]);
        let rebuilt = lab.view();
        let tab = rebuilt.tab(restored);
        assert_eq!(tab.left, lab.remembered_width(second));
        assert_eq!(tab.panes.last().unwrap().block, shell);
        for label in RAIL {
            assert_ne!(tab.pane(label).block, view.tab(restored).pane(label).block);
        }
        lab.drag_rail(session, lab.view().tab(restored), "0.36");
        lab.remembers(second, "0.360000");
        lab.check();
    }
}

#[test]
#[ignore = "needs an installed Rex; starts a disposable server"]
fn rails_follow_their_worktree_through_new_tabs_resizes_renames_and_a_restart() {
    let mut scene = Scene::start();
    let [one, two] = scene.new_tabs_get_their_worktrees_rail();
    scene.rail_follows_bare_cd(&one);
    scene.tabs_opened_back_to_back_each_get_their_rail();
    let other_session = scene.a_new_session_gets_a_rail_and_a_command_tab_does_not();
    let wide = scene.a_drag_is_remembered_for_its_worktree(&one);
    let before = scene.given_names_are_kept(&wide);
    let [fresh, barrier] = scene.a_restart_brings_everything_back(&before, &other_session);
    scene.a_restored_rail_is_remembered_only_if_milo_runs_in_it(&two, &other_session, [&fresh, &barrier]);
    scene.a_restored_rail_needs_confirmation_to_rebuild(&two);
}

/// The lab has scripts attached to its sessions, as the listener and the
/// presence helper's own watch are, but no Rex app. Discord presence is for a
/// person with Rex open, so here nothing may be shown. The stand-in for
/// Discord listens in the lab's temporary folder, the only place the helper
/// looks: it is never the Discord app of whoever runs this.
#[test]
#[ignore = "needs an installed Rex; starts a disposable server"]
fn discord_presence_shows_nothing_without_a_rex_app_and_its_helper_ends_with_the_server() {
    let mut lab = Lab::start();
    let [first, _] = lab.worktrees();
    lab.new_session("lab", "first tab", &first);
    lab.start_app();
    let discord = std::os::unix::net::UnixListener::bind(lab.root.join("tmp/discord-ipc-0")).unwrap();
    discord.set_nonblocking(true).unwrap();

    // Asking for both names makes the script look at sessions, which this server must accept.
    let on = lab.milo(&["presence", "enable", "--details", "both", "--yes"]);
    assert!(on.contains("Nothing is shown until a Rex app opens a session."), "{on}");
    lab.new_session("more", "another tab", &first);
    std::thread::sleep(Duration::from_secs(2));
    assert!(discord.accept().is_err(), "scripts attached to sessions are not a person using Rex");
    assert!(lab.milo(&["presence"]).contains("Nothing is shown until a Rex app opens a session."));

    lab.stop_server();
    assert!(soon(|| lab.milo(&["presence"]).contains("Milo's presence helper is not running")));
    assert!(discord.accept().is_err());
}

/// The floating reader the outline opens in Rex, edited through Rex's own keys.
#[test]
#[ignore = "needs an installed Rex; starts a disposable server"]
fn a_floating_reader_edits_a_file_and_is_not_replaced_while_it_has_unsaved_changes() {
    let mut lab = Lab::start();
    lab.start_app();
    let dir = lab.root.join("edit");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.txt"), "first\n").unwrap();
    fs::write(dir.join("b.txt"), "second\n").unwrap();
    let [session, tab] = lab.new_session("edit", "files", &dir);
    let milo = lab.milo.to_str().unwrap().to_owned();
    let place = ["-s", session.as_str(), "-w", tab.as_str(), "-c", dir.to_str().unwrap()];
    let outline = ["--split=left", "--shell=none", "--label", "outline", "--", milo.as_str(), "outline"];
    lab.rex(&[["run"].as_slice(), &place, &outline].concat());

    // The reader is named after its file; what it runs is checked by what its screen shows.
    let pane = |label: &str| {
        let view = lab.view();
        view.tab(&tab).panes.iter().find(|pane| pane.label == label).map(|pane| pane.block.clone())
    };
    let screen = |block: &str| lab.rex(&["capture", "-s", &session, "-b", block, "--trim"]);
    let shows = |block: &str, text: &str| {
        assert!(soon(|| screen(block).contains(text)), "expected {text:?} on:\n{}", screen(block));
    };
    let keys = |block: &str, keys: &[&str]| {
        lab.rex(&[["send-key", "-s", session.as_str(), "-b", block].as_slice(), keys].concat());
    };
    let outline = {
        assert!(soon(|| pane("outline").is_some()), "{:?}", lab.view());
        pane("outline").unwrap()
    };

    // Reading is the default: the reader the outline opens offers to edit and writes nothing.
    shows(&outline, "a.txt");
    keys(&outline, &["enter"]);
    assert!(soon(|| pane("a.txt").is_some()), "no floating reader in {:?}", lab.view());
    let reader = pane("a.txt").unwrap();
    shows(&reader, "first");
    shows(&reader, "e edit");
    keys(&reader, &["e"]);
    shows(&reader, "^S save");
    lab.rex(&["send", "-s", &session, "-b", &reader, "typed "]);
    shows(&reader, "unsaved");
    shows(&reader, "typed first");
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "first\n");

    // Opening another file would end this reader, so the outline leaves it alone.
    keys(&outline, &["down", "enter"]);
    shows(&outline, "The open reader has unsaved edits.");
    assert!(pane("b.txt").is_none() && pane("a.txt").as_deref() == Some(reader.as_str()));

    keys(&reader, &["ctrl+s"]);
    shows(&reader, "Saved");
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "typed first\n");
    keys(&outline, &["enter"]);
    assert!(soon(|| pane("b.txt").is_some() && pane("a.txt").is_none()), "{:?}", lab.view());

    // A reader that Rex closes with unsaved changes keeps them outside the project.
    let reader = pane("b.txt").unwrap();
    shows(&reader, "second");
    keys(&reader, &["e"]);
    shows(&reader, "^S save");
    lab.rex(&["send", "-s", &session, "-b", &reader, "lost? "]);
    shows(&reader, "unsaved");
    lab.rex(&["block", "close", "-s", &session, &reader]);
    let drafts = lab.root.join("home/.local/state/milo/drafts");
    let kept = || fs::read_dir(&drafts).ok()?.flatten().find_map(|entry| fs::read_to_string(entry.path()).ok());
    assert!(soon(|| kept().as_deref() == Some("lost? second\n")), "{:?}", kept());
    assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "second\n");

    // The outline can open it again, and the editor offers the draft back.
    keys(&outline, &["enter"]);
    assert!(soon(|| pane("b.txt").is_some()), "{:?}", lab.view());
    let reader = pane("b.txt").unwrap();
    shows(&reader, "second");
    keys(&reader, &["e"]);
    shows(&reader, "Kept draft found");
    keys(&reader, &["r", "ctrl+s"]);
    assert!(soon(|| fs::read_to_string(dir.join("b.txt")).unwrap() == "lost? second\n"));
}

/// A program with no terminal hands a patch of its own edits to `milo review --float`.
#[test]
#[ignore = "needs an installed Rex; starts a disposable server"]
fn a_floating_review_marks_only_the_handed_over_edits_and_says_when_it_cannot() {
    let mut lab = Lab::start();
    lab.start_app();
    let dir = lab.root.join("review");
    fs::create_dir_all(&dir).unwrap();
    lab.git(&dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join("app.txt"), "one\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
    lab.git(&dir, &["add", "-A"]);
    lab.git(&dir, &["commit", "-q", "-m", "base"]);
    // Uncommitted work by a person and by the agent, in the same file.
    let now = "one, by a person\ntwo\nthree\nfour, by the agent\nfive\nsix, by a person\n";
    fs::write(dir.join("app.txt"), now).unwrap();
    fs::write(dir.join("new.txt"), "by the agent\n").unwrap();
    let patch = lab.root.join("edits.patch");
    let handed = "--- a/app.txt\n+++ b/app.txt\n@@ -1,6 +1,6 @@\n one\n two\n three\n-four\n+four, by the agent\n five\n six\n\
                  --- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+by the agent\n\
                  --- a/../home/.zshrc\n+++ b/../home/.zshrc\n@@ -0,0 +1 @@\n+outside\n";
    fs::write(&patch, handed).unwrap();

    let [session, tab] = lab.new_session("review", "agent", &dir);
    let pane = |label: &str| {
        let view = lab.view();
        view.tab(&tab).panes.iter().find(|pane| pane.label == label).map(|pane| pane.block.clone())
    };
    let shell = lab.view().tab(&tab).panes[0].block.clone();
    // What an agent's shell in that tab would run: no terminal, only the pane's Rex variables.
    let output = lab
        .command(&lab.milo)
        .current_dir(&dir)
        .env("REX_SESSION", &session)
        .env("REX_BLOCK", &shell)
        .args(["review", patch.to_str().unwrap(), "--from", "the agent", "--float"])
        .output()
        .unwrap();
    let said = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{said}{}", String::from_utf8_lossy(&output.stderr));
    assert!(said.contains("in 2 of 3 files"), "{said}");
    assert!(said.contains("Not shown: ../home/.zshrc: The patch names a path that is not a file inside"), "{said}");

    assert!(soon(|| pane("Patch review").is_some()), "no floating review in {:?}", lab.view());
    let review = pane("Patch review").unwrap();
    let screen = || lab.rex(&["capture", "-s", &session, "-b", &review, "--trim"]);
    let shows = |text: &str| assert!(soon(|| screen().contains(text)), "expected {text:?} on:\n{}", screen());
    let keys = |keys: &[&str]| {
        lab.rex(&[["send-key", "-s", session.as_str(), "-b", review.as_str()].as_slice(), keys].concat());
    };
    shows("patch from the agent");
    shows("4 + four, by the agent");
    let frame = screen();
    assert!(frame.contains("- four\n") || frame.contains("- four "), "{frame}");
    // The person's lines are there, and not as changes.
    for theirs in ["1   one, by a person", "6   six, by a person"] {
        assert!(frame.contains(theirs), "{frame}");
    }

    keys(&["]"]);
    shows("1 + by the agent");
    keys(&["]"]);
    shows("not a file inside the project");
    assert!(!screen().contains("zselect"), "a file outside the project was read:\n{}", screen());

    // Once a person rewrites the agent's line, the file shows no diff rather than a guess.
    keys(&["]"]);
    shows("4 + four, by the agent");
    fs::write(dir.join("app.txt"), now.replace("four, by the agent", "four, rewritten")).unwrap();
    keys(&["r"]);
    shows("no longer holds the lines the patch left");
    assert!(!screen().contains("rewritten"), "{}", screen());

    // Reading is all it does: `e` is refused, and the project is as it was left.
    keys(&["e"]);
    shows("Read-only: this shows a patch");
    keys(&["q"]);
    assert!(soon(|| pane("Patch review").is_none()), "{:?}", lab.view());
    assert_eq!(fs::read_to_string(dir.join("app.txt")).unwrap(), now.replace("four, by the agent", "four, rewritten"));
    assert!(!lab.root.join("home/.local/state/milo").exists(), "the review kept something in Milo's state folder");
}
