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
//!
//! The native overlay regression additionally needs a macOS GUI login and
//! Xcode's tools. Only the accessory fixture's windows are ordered onscreen:
//! `cargo test --test rex_lab persistent_preview_coexists_with_shell -- --ignored --nocapture`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[path = "rex_lab/preview_tick.rs"]
mod preview_tick;

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

/// Runs the production layout and observer Lua on a real disposable Rex server.
/// The simulated peer owns blocks with actual Rex calls; it does not draw a GUI.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs Rex and Python 3; starts a disposable server and simulated viewer"]
fn preview_layout_and_observer() {
    use std::io::{BufRead as _, BufReader};

    fn unix_seconds() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    // Compare geometry and sequencing independently from absolute time, but
    // only after validating the positive record's original wire shape.
    fn watch_records(log: &str, started_at: u64) -> impl Iterator<Item = &str> {
        fn decimal(field: &str) -> bool {
            let mut bytes = field.bytes();
            match bytes.next() {
                Some(b'0') => bytes.next().is_none(),
                Some(b'1'..=b'9') => bytes.all(|byte| byte.is_ascii_digit()),
                _ => false,
            }
        }

        let now = unix_seconds();
        log.split_inclusive('\n').map_while(|line| line.strip_suffix('\n')).map(move |record| {
            if matches!(record, "hide" | "unclaimed") {
                return record;
            }
            let (state, stamp) = record.rsplit_once(" @").unwrap_or_else(|| panic!("missing timestamp: {record}"));
            assert!(decimal(stamp), "invalid observation time: {record}");
            let observed_at = stamp.parse::<u64>().unwrap();
            assert!((started_at..=now).contains(&observed_at), "observation outside watcher lifetime: {record}");
            if state != "renew" {
                let (pid, rect) = state
                    .strip_prefix("show ")
                    .and_then(|show| show.split_once(" tile "))
                    .unwrap_or_else(|| panic!("invalid positive record: {record}"));
                let mut fields = rect.split(' ');
                assert!(
                    decimal(pid) && (0..4).all(|_| fields.next().is_some_and(decimal)) && fields.next().is_none(),
                    "invalid tiled geometry: {record}"
                );
            }
            state
        })
    }

    fn continues_tile(record: &str, current: &str) -> bool {
        current.starts_with("show ") && (record == "renew" || record == current)
    }

    fn watch_changes(log: &str, started_at: u64) -> impl Iterator<Item = &str> {
        let mut current = "";
        watch_records(log, started_at).filter(move |&record| {
            if continues_tile(record, current) {
                return false;
            }
            current = record;
            true
        })
    }

    struct PausedWatcher(u32);
    impl Drop for PausedWatcher {
        fn drop(&mut self) {
            // SAFETY: This is the lab's own watcher, which we just stopped.
            unsafe { libc::kill(self.0 as libc::pid_t, libc::SIGCONT) };
        }
    }

    let mut lab = Lab::start();
    // The request runner must be a real PTY descendant, just as Milo is.
    // It keeps its own PID while invoking Rex and supplies only test inputs.
    let root = lab.root.to_str().unwrap();
    let viewer = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_lab/preview_viewer.py");
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/src/rex/preview.lua");
    let url = "http://localhost:4321/lab/a%2Fb&$HOME";
    let native_window_id = u32::MAX; // A sentinel; the simulated app has no native window.
    let pid_file = lab.root.join("preview-arguments");
    let nested = lab.root.join("nested-worktree");
    fs::create_dir(&nested).unwrap();
    let open_script = lab.root.join("preview-open.lua");
    fs::write(
        &open_script,
        format!(
            "if rex.args.switch_to then\n\
                 local split = rex.session.new_split\n\
                 rex.session.new_split = function(args)\n\
                     assert(rex.session.focus_window{{window_id=rex.args.switch_to}})\n\
                     return split(args)\n\
                 end\n\
             end\n\
             dofile('{script}')\n"
        ),
    )
    .unwrap();
    let open_script = open_script.display();
    fs::write(
        lab.root.join("preview-request"),
        format!(
            "read -r target < \"$1\"\n\
             payload=$(cat \"$1.json\")\n\
             payload=${{payload/PID_TOKEN/$$}}\n\
             '{root}/bin/rex' do -s \"$REX_SESSION\" -b \"$target\" '{open_script}' --args \"$payload\" > \"$1.reply\"\n\
             mv \"$1.reply\" \"$1.done\"\n"
        ),
    )
    .unwrap();
    let side_loop = format!(
        "request='{root}/request-'$REX_BLOCK\n\
         while :; do if [ -f \"$request\" ]; then /bin/bash '{root}/preview-request' \"$request\"; rm \"$request\"; fi; \
         /bin/sleep 0.05; done\n"
    );
    for role in ["outline", "usage"] {
        fs::write(lab.root.join(role), &side_loop).unwrap();
    }
    fs::write(
        nested.join("preview-anchor"),
        format!(
            "ROOT='{root}'\nREX='{root}/bin/rex'\nSCRIPT='{script}'\n{}",
            include_str!("rex_lab/preview_anchor.sh")
        ),
    )
    .unwrap();
    let created = lab.rex(&["new", "layout", "-c", root, "--shell=none", "--json", "--", "/bin/sh", "-i"]);
    let session = id(&created, "session_id");
    let tab = id(&created, "window_id");
    let main = id(&created, "block_ids");
    let second_tab = lab.new_tab(&session, &lab.root);
    let second_shell = lab.view().tab(&second_tab).panes[0].block.clone();
    let split = |role: &str, arguments: &[&str]| {
        let mut command = vec!["split", "-s", &session, "-b", &main, "-c", root, "--focus=false", "--shell=none", "--json"];
        let label = format!("milo:{role}");
        command.extend(["--label", &label, "--", "/bin/bash", role]);
        command.extend_from_slice(arguments);
        id(&lab.rex(&command), "block_ids")
    };
    let rail = split("outline", &["--shell", &main]);
    let stale = split("outline", &["--shell", &second_shell]);
    let usage = split("usage", &[]);
    let another_shell = id(
        &lab.rex(&[
            "split", "-s", &session, "-b", &main, "-c", root, "--focus=false", "--shell=none", "--json", "--",
            "/bin/sh", "-i",
        ]),
        "block_ids",
    );

    let denied = lab
        .command(&lab.milo)
        .env("REX_SESSION", &session)
        .env("REX_BLOCK", &main)
        .args(["preview", url, "--placement", "right"])
        .output()
        .unwrap();
    assert!(!denied.status.success() && String::from_utf8_lossy(&denied.stderr).contains("isn't owned"), "{denied:?}");

    let mut peer = lab
        .command("python3")
        .arg(viewer)
        .arg(lab.root.join("s.sock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let viewer_pid = peer.id();
    let mut peer_in = peer.stdin.take().unwrap();
    let mut peer_out = BufReader::new(peer.stdout.take().unwrap());
    lab.app = Some(peer);
    let mut client = String::new();
    peer_out.read_line(&mut client).unwrap();
    assert!(client.starts_with("client:"), "{client}");
    let client = client.trim();
    let owner = format!(
        r#","window":"{tab}","owner":"{client}","owner_pid":{viewer_pid},"native_window_id":{native_window_id}"#,
    );
    let mut viewer_session = |method, target: &str| {
        writeln!(peer_in, r#"["{method}",{{"session_id":"{target}"}}]"#).unwrap();
        let mut reply = String::new();
        peer_out.read_line(&mut reply).unwrap();
        assert!(reply.contains(r#""type":"response""#), "{method}: {reply}");
    };
    let transfer = |block: &str| {
        let mut other = lab
            .command("python3")
            .arg(viewer)
            .arg(lab.root.join("s.sock"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = other.stdin.take().unwrap();
        let mut output = BufReader::new(other.stdout.take().unwrap());
        let mut other_id = String::new();
        output.read_line(&mut other_id).unwrap();
        assert!(other_id.starts_with("client:"), "{other_id}");
        let mut reply = String::new();
        for (method, payload) in [
            ("session.attach", format!(r#"{{"session_id":"{session}"}}"#)),
            ("session.own_block", format!(r#"{{"session_id":"{session}","block_ids":["{block}"]}}"#)),
        ] {
            writeln!(input, r#"["{method}",{payload}]"#).unwrap();
            reply.clear();
            output.read_line(&mut reply).unwrap();
            assert!(reply.contains(r#""type":"response""#), "{method}: {reply}");
        }
        (other, input, output, other_id.trim().to_owned())
    };
    // An attachment outside the current session can change during a preview.
    let background = lab.rex(&["new", "also-attached", "-c", root, "--shell=none", "--json", "--", "/bin/sleep", "300"]);
    let background = id(&background, "session_id");
    viewer_session("session.attach", &session);
    viewer_session("session.own_block", &session);

    let stale_env = lab
        .command(&lab.milo)
        .env("REX_SESSION", &session)
        .env("REX_BLOCK", &main)
        .args(["preview", url, "--placement", "right"])
        .output()
        .unwrap();
    assert!(!stale_env.status.success()
        && String::from_utf8_lossy(&stale_env.stderr).contains("not in the specified Rex terminal"), "{stale_env:?}");

    let socket = lab.socket();
    // SAFETY: geteuid reads this account's own effective user ID.
    let uid = unsafe { libc::geteuid() };
    let args = |mode: &str, block: &str, pid: &str, extra: &str| {
        format!(r#"{{"mode":"{mode}","server":"{socket}","uid":{uid},"pid":{pid},"session":"{session}","block":"{block}"{extra}}}"#)
    };
    let send = |caller: &str, command: &str| {
        lab.rex(&["send", "-s", &session, "-b", caller, command]);
        lab.rex(&["send-key", "-s", &session, "-b", caller, "enter"]);
    };
    fn request_file(lab: &Lab, caller: &str) -> PathBuf {
        lab.root.join(format!("request-{caller}"))
    }
    let request = |mode: &str, origin: &str, target: &str, extra: &str| {
        let file = request_file(&lab, origin);
        let done = file.with_extension("done");
        fs::write(file.with_extension("json"), args(mode, target, "PID_TOKEN", extra)).unwrap();
        fs::write(&file, format!("{target}\n")).unwrap();
        if origin == main || origin == another_shell {
            send(origin, &format!("/bin/bash '{root}/preview-request' '{}'; rm '{}'", file.display(), file.display()));
        }
        assert!(soon(|| done.exists() && !file.exists()), "no reply from {origin} for {mode} on {target}");
        let reply = fs::read_to_string(&done).unwrap();
        fs::remove_file(done).unwrap();
        reply
    };
    let identify = |origin: &str, target: &str| {
        request("identify", origin, target, r#","exe":"/bin/bash""#)
    };
    let open = |origin: &str, target: &str, placement: &str, expected: &str, cwd: &str| {
        let extra = format!(
            r#","exe":"/bin/bash","url":"{url}","placement":"{placement}","main":"{expected}","cwd":"{cwd}"{owner}"#,
        );
        request("open", origin, target, &extra)
    };
    let launch = |caller: &str, placement: &str| {
        let plan = identify(caller, caller);
        assert_eq!(id(&plan, "main"), main);
        let cwd = id(&plan, "cwd");
        assert_eq!(Path::new(&cwd), nested.canonicalize().unwrap());
        assert_eq!(id(&plan, "owner"), client);
        assert_eq!(id(&plan, "window"), tab);
        open(caller, caller, placement, &main, &cwd)
    };
    let close = |block: &str| lab.rex(&["block", "close", "-s", &session, block]);
    assert_eq!(identify(&stale, &stale).trim(), "stale");
    assert_eq!(identify(&usage, &usage).trim(), "shell");
    assert_eq!(identify(&main, &another_shell).trim(), "caller");
    assert_eq!(identify(&another_shell, &main).trim(), "caller");
    assert_eq!(open(&main, &another_shell, "right", &another_shell, root).trim(), "caller");
    assert_eq!(open(&another_shell, &main, "right", &main, root).trim(), "caller");
    assert_eq!(open(&rail, &rail, "right", &main, "/wrong/directory").trim(), "changed");
    lab.rex(&["zoom", "-s", &session, &main, "--on"]);
    assert_eq!(identify(&main, &main).trim(), "zoom");
    lab.rex(&["zoom", "-s", &session, &main, "--off"]);
    lab.rex(&["focus", "-s", &session, &second_tab]);
    assert_eq!(identify(&main, &main).trim(), "tab");
    lab.rex(&["focus", "-s", &session, &tab]);
    let float_block = |block: &str| {
        let floated = format!(r#"{{"session_id":"{session}","block_id":"{block}","bounds":[0.1,0.1,0.6,0.6]}}"#);
        lab.rex(&["api", "call", "session.float_block", &floated]);
    };
    float_block(&another_shell);
    assert_eq!(identify(&another_shell, &another_shell).trim(), "floating_caller");
    send(&main, &format!("(cd '{}' && exec /bin/sh -i)", nested.display()));
    send(&main, &format!("pwd > '{root}/nested-cwd'"));
    assert!(soon(|| lab.read("nested-cwd").trim().ends_with("/nested-worktree")));
    for block in [&stale, &usage, &another_shell] {
        close(block);
    }

    let scope = |request: &str| lab.rex(&["do", "-s", &session, "-e", request]);
    let show_for = |block: &str| {
        let rect = scope(&format!(
            "local view=assert(rex.session.view{{}}); \
             local function leaf(node) \
                if node.block_id=='{block}' then return node.rect end; \
                local split=node.split; if split then return leaf(split.before) or leaf(split.after) end \
             end; \
             for _,w in ipairs(view.windows) do if w.window_id=='{tab}' then \
                for _,l in ipairs(w.layers) do if l.kind=='tiled' then \
                    local r=leaf(l.layout); \
                    if r then print(string.format('%.17g %.17g %.17g %.17g',r[1],r[2],r[3],r[4])); return end \
                end end \
             end end; error('no tiled leaf for preview')"
        ));
        let rect: Vec<f64> = rect.split_whitespace().map(|component| component.parse().unwrap()).collect();
        let &[x, y, w, h] = rect.as_slice() else { panic!("expected a tiled rectangle, got {rect:?}") };
        let fraction = |value: f64| (value * 1_000_000.0).round();
        let (x0, y0) = (fraction(x), fraction(y));
        let (width, height) = (fraction(x + w) - x0, fraction(y + h) - y0);
        assert!(x0 >= 0.0 && y0 >= 0.0 && width > 0.0 && height > 0.0 && x0 + width <= 1e6 && y0 + height <= 1e6);
        format!("show {viewer_pid} tile {x0} {y0} {width} {height}")
    };
    let move_block = |block: &str, beside: &str| {
        scope(&format!(
            "assert(rex.session.move_block{{block_id='{block}',anchor_block_id='{beside}',direction='horizontal',side='after'}})"
        ))
    };
    let plan = identify(&main, &main);
    let planned_cwd = id(&plan, "cwd");
    move_block(&main, &second_shell);
    lab.rex(&["focus", "-s", &session, &second_tab]);
    assert_eq!(open(&main, &main, "right", &main, &planned_cwd).trim(), "changed");
    move_block(&main, &rail);
    lab.rex(&["focus", "-s", &session, &tab]);
    lab.rex(&["focus", "-s", &session, &main]);
    let raced = format!(
        r#","exe":"/bin/bash","url":"{url}","placement":"right","main":"{main}","cwd":"{planned_cwd}","switch_to":"{second_tab}"{owner}"#,
    );
    assert_eq!(request("open", &main, &main, &raced).trim(), "changed");
    assert!(lab.view().tabs.values().all(|tab| !tab.labels().iter().any(|label| label.starts_with("milo:preview"))));
    let _ = fs::remove_file(&pid_file);
    lab.rex(&["focus", "-s", &session, &tab]);
    assert_eq!(launch(&rail, "floating").trim(), "placement");
    assert!(!lab.view().tab(&tab).labels().iter().any(|label| label.starts_with("milo:preview")));

    let anchor_scope = format!(r#","main":"{main}"{owner}"#);
    let focus_main = |block: &str| request("focus", block, block, &anchor_scope);
    let start_watch = |block: &str, child: u32| {
        let payload = args("watch", block, &child.to_string(), &anchor_scope);
        let file = request_file(&lab, block);
        let cmd = file.with_extension("watch.json");
        let staged = file.with_extension("watch.tmp");
        fs::write(&staged, payload).unwrap();
        fs::rename(staged, cmd).unwrap();
        file
    };
    let alive = |pid: u32| lab.command("/bin/ps").args(["-p", &pid.to_string(), "-o", "pid="]).output().unwrap().status.success();
    let check_focus = |expected: &str| {
        let check = format!(
            "local s=assert(rex.session.view{{}}); assert(s.active_window_id=='{tab}'); \
             for _,w in ipairs(s.windows) do if w.window_id=='{tab}' then assert(w.focused_block_id=='{expected}', 'focus: '..tostring(w.focused_block_id)); return end end; error('missing working tab')"
        );
        scope(&check);
    };
    for (placement, direction, side, caller) in [
        ("right", "horizontal", "after", &main),
        ("left", "horizontal", "before", &rail),
        ("above", "vertical", "before", &rail),
        ("below", "vertical", "after", &rail),
    ] {
        // A launch from the focused sidebar must not steal keyboard focus either.
        lab.rex(&["focus", "-s", &session, caller]);
        assert_eq!(launch(caller, placement).trim(), "ok", "{placement}");
        check_focus(caller);
        let literal = format!("\n{url}\n{main}\n{tab}\n{client}\n{viewer_pid}\n{native_window_id}\n");
        assert!(soon(|| lab.read("preview-arguments").ends_with(&literal)));
        let child = lab.read("preview-arguments").lines().next().unwrap().parse::<u32>().unwrap();
        fs::remove_file(&pid_file).unwrap();
        let found = scope(&format!(
            "local view=assert(rex.session.view{{}}); for _,w in ipairs(view.windows) do if w.window_id=='{tab}' then \
             for _,l in ipairs(w.layers) do for _,b in ipairs(l.blocks) do if b.label=='milo:preview' then \
             local function adjacent(node) local sp=node.split; if not sp then return false end; \
             return (sp.direction=='{direction}' and sp['{side}'].block_id==b.block_id \
             and (sp.before.block_id=='{main}' or sp.after.block_id=='{main}')) \
             or adjacent(sp.before) or adjacent(sp.after) end; \
             assert(l.kind=='tiled' and adjacent(l.layout)); print(b.block_id..' '..tostring(b.owner)); return end end end \
             end end; error('no preview')"
        ));
        let mut found = found.split_whitespace();
        let block = found.next().unwrap();
        let owner_before_claim = found.next().unwrap();
        if placement == "above" {
            // Publishing a split created by a CLI does not itself assign an app owner.
            assert_eq!(owner_before_claim, "nil");
        } else {
            viewer_session("session.own_block", &session);
        }
        let transferred = if placement == "right" {
            lab.rex(&["focus", "-s", &session, &second_tab]);
            Some(transfer(block))
        } else {
            None
        };

        if placement == "left" {
            let payload = args("watch", block, &child.to_string(), &anchor_scope);
            let output = lab.rex_output(&["do", "-s", &session, "-b", block, script, "--args", &payload]).unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(output.stdout, b"hide\n");
            let payload = args("focus", block, &child.to_string(), &anchor_scope);
            assert_eq!(lab.rex(&["do", "-s", &session, "-b", block, script, "--args", &payload]).trim(), "focus");
        }
        let started_at = unix_seconds();
        let watch = start_watch(block, child);
        let records = || fs::read_to_string(watch.with_extension("watch.log")).unwrap_or_default();
        let cursor = std::cell::Cell::new(0);
        let current = RefCell::new(String::new());
        let next_record = |deadline| {
            let index = cursor.get();
            loop {
                let log = records();
                if let Some(line) = watch_records(&log, started_at).nth(index) {
                    cursor.set(index + 1);
                    break line.to_owned();
                }
                assert!(Instant::now() < deadline, "{placement}: missing record {index}: {log}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let expect = |state: &str| {
            let deadline = Instant::now() + PATIENCE;
            loop {
                let line = next_record(deadline);
                if continues_tile(&line, &current.borrow()) {
                    assert!(Instant::now() < deadline, "{placement}: excessive heartbeats");
                    continue;
                }
                assert_eq!(line, state, "{placement}: {}", records());
                *current.borrow_mut() = line;
                break;
            }
        };
        let watch_pid = || {
            let pid = || fs::read_to_string(watch.with_extension("watch.pid")).ok()?.trim().parse::<u32>().ok();
            assert!(soon(|| pid().is_some()), "{placement}: no watcher PID");
            pid().unwrap()
        };
        let mut show = show_for(block);
        expect("hide");
        if let Some((mut other, input, output, _)) = transferred {
            lab.rex(&["focus", "-s", &session, &tab]);
            std::thread::sleep(Duration::from_millis(250));
            assert_eq!(records(), "hide\n", "preview appeared for a foreign owner before its first show");
            viewer_session("session.own_block", &session);
            drop((input, output));
            assert!(other.wait().unwrap().success());
        }
        if placement == "above" {
            std::thread::sleep(Duration::from_millis(250));
            assert_eq!(records(), "hide\n", "an unowned preview must stay hidden after publication");
            viewer_session("session.own_block", &session);
        }
        expect(&show);
        if placement == "right" {
            scope(&format!("assert(rex.session.detach_block{{block_id='{main}'}})"));
            expect("hide");
            move_block(&main, &rail);
            lab.rex(&["focus", "-s", &session, &main]);
            show = show_for(block);
            expect(&show);
        }
        let (before, after) = if side == "before" { (block, main.as_str()) } else { (main.as_str(), block) };
        scope(&format!(
            "assert(rex.session.resize_pane{{direction='{direction}', before_block_id='{before}', \
             after_block_id='{after}', ratio=0.4}})"
        ));
        let resized = show_for(block);
        assert_ne!(show, resized, "{placement}: Rex's leaf did not move on resize");
        expect(&resized);
        let show = resized;
        lab.rex(&["zoom", "-s", &session, &main, "--on"]);
        expect("hide");
        if placement == "right" {
            assert_eq!(focus_main(block).trim(), "focus");
        }
        lab.rex(&["zoom", "-s", &session, &main, "--off"]);
        expect(&show);
        if placement == "right" {
            lab.rex(&["focus", "-s", &session, block]);
            assert_eq!(focus_main(block).trim(), "ok");
            check_focus(&main);
            assert_eq!(focus_main(block).trim(), "ok", "Rex already targets the working shell");
            // Observe regular positive observations with unchanged geometry.
            let deadline = Instant::now() + PATIENCE;
            let log = records();
            for line in watch_records(&log, started_at).skip(cursor.get()) {
                assert!(continues_tile(line, &show), "{placement}: {line}");
                cursor.set(cursor.get() + 1);
            }
            for _ in 0..4 {
                let line = next_record(deadline);
                assert!(continues_tile(&line, &show), "{placement}: {line}");
            }

            // Stop just the lab's watcher and leave its output open. The
            // native fixture separately checks expiry on a silent pipe.
            let watcher = watch_pid();
            // SAFETY: This PID belongs to the watcher started in our disposable Rex lab.
            assert_eq!(unsafe { libc::kill(watcher as libc::pid_t, libc::SIGSTOP) }, 0);
            let paused = PausedWatcher(watcher);
            assert!(soon(|| {
                let output = lab.command("/bin/ps").args(["-o", "stat=", "-p", &watcher.to_string()]).output().unwrap();
                String::from_utf8_lossy(&output.stdout).trim_start().starts_with('T')
            }));
            let silent = records();
            for line in watch_records(&silent, started_at).skip(cursor.get()) {
                assert!(continues_tile(line, &show), "{placement}: {line}");
                cursor.set(cursor.get() + 1);
            }
            std::thread::sleep(Duration::from_millis(3250));
            assert_eq!(records(), silent);
            assert!(alive(watcher));
            drop(paused);
            let line = next_record(Instant::now() + PATIENCE);
            assert!(continues_tile(&line, &show), "{placement}: {line}");
            check_focus(&main);
        }
        if placement == "above" {
            lab.rex(&["zoom", "-s", &session, block, "--on"]);
            expect("hide");
            lab.rex(&["zoom", "-s", &session, block, "--off"]);
            expect(&show);
            // A separate Rex layer still conceals the tiled preview while visible.
            let cover = scope(&format!(
                "local cover=assert(rex.session.new_layer{{window_id='{tab}',bounds={{0.25,0.2,0.55,0.55}},focus=false, \
                 layout={{block={{flavor='com.superlogical.terminal.shell', \
                 options={{command={{'/bin/sleep','60'}},cwd='{root}',shell='none'}}}}}}}}).block_ids[1]; \
                 local s=assert(rex.session.view{{}}); for _,w in ipairs(s.windows) do if w.window_id=='{tab}' then \
                 for _,l in ipairs(w.layers) do if l.kind=='floating' and l.layout.block_id==cover then print(l.layer_id); return end end \
                 end end; error('no covering layer')"
            ));
            let cover = cover.trim();
            expect("hide");
            assert_eq!(focus_main(block).trim(), "focus");
            scope(&format!("assert(rex.session.set_layer_state{{layer_id='{cover}', state='minimized'}})"));
            expect(&show);
            scope(&format!("assert(rex.session.set_layer_state{{layer_id='{cover}', state='normal', focus=false}})"));
            expect("hide");
            scope(&format!("assert(rex.session.close_layer{{layer_id='{cover}'}})"));
            expect(&show);
        }
        lab.rex(&["focus", "-s", &session, &second_tab]);
        expect("hide");
        if placement == "right" {
            assert_eq!(focus_main(block).trim(), "focus");
            assert_eq!(scope("print(assert(rex.session.view{}).active_window_id)").trim(), second_tab);
        }
        lab.rex(&["focus", "-s", &session, &tab]);
        expect(&show);
        if placement == "right" {
            let revision = || scope("print(assert(rex.session.view{}).revision)");
            let before = revision();
            viewer_session("session.attach", &background);
            let line = next_record(Instant::now() + PATIENCE);
            assert!(continues_tile(&line, &show), "{placement}: {line}");
            assert_eq!(revision(), before, "other-session attachment changed the scoped layout");
            let before = revision();
            viewer_session("session.detach", &background);
            let line = next_record(Instant::now() + PATIENCE);
            assert!(continues_tile(&line, &show), "{placement}: {line}");
            assert_eq!(revision(), before, "other-session detachment changed the scoped layout");
            check_focus(&main);
        }
        let watcher = watch_pid();
        if placement == "left" {
            move_block(block, &second_shell);
            lab.rex(&["focus", "-s", &session, &second_tab]);
            expect("hide");
            assert!(soon(|| fs::read_to_string(watch.with_extension("watch.exit")).is_ok_and(|code| code.trim() == "0")));
            close(block);
            lab.rex(&["focus", "-s", &session, &main]);
        } else if placement == "below" {
            float_block(block);
            expect("hide");
            assert!(soon(|| fs::read_to_string(watch.with_extension("watch.exit")).is_ok_and(|code| code.trim() == "0")));
            close(block);
        } else {
            close(block);
            // Closing the owning PTY can end the subprocess before Rex
            // delivers its final hide. EOF is sufficient for the native pane.
            assert!(soon(|| !alive(watcher)));
            let log = records();
            let mut tail = watch_records(&log, started_at).skip(cursor.get()).skip_while(|line| continues_tile(line, &show));
            if let Some(final_state) = tail.next() {
                assert_eq!(final_state, "hide");
                assert!(tail.next().is_none(), "{placement}: records after close: {log}");
            }
        }
        assert!(soon(|| !alive(watcher)), "{placement}: watcher didn't end");
    }

    // The real Rust anchor reaches its startup error without asking for or
    // exercising GUI access. Use the production Lua launch and exit policy.
    let launcher = lab.root.join("preview-startup");
    let child = lab.milo.display();
    fs::write(&launcher, format!("#!/bin/sh\nSSH_CONNECTION=lab exec '{child}' \"$@\"\n")).unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    let plan = identify(&main, &main);
    let cwd = id(&plan, "cwd");
    let launch_startup = |placement: &str| {
        let exe = launcher.display();
        request(
            "open",
            &main,
            &main,
            &format!(
                r#","exe":"{exe}","url":"{url}","placement":"{placement}","main":"{main}","cwd":"{cwd}"{owner}"#,
            ),
        )
    };
    assert_eq!(launch_startup("right").trim(), "ok");
    let failure_block = lab.view().tab(&tab).pane("milo:preview").block.clone();
    let capture = || lab.rex(&["capture", "-s", &session, "-b", &failure_block, "--trim"]);
    assert!(soon(|| capture().contains("Press Enter to dismiss")), "{}", capture());
    std::thread::sleep(Duration::from_millis(350));
    let error = capture();
    assert!(error.contains("SSH session") && !error.contains(url), "{error}");
    lab.rex(&["send-key", "-s", &session, "-b", &failure_block, "enter"]);
    assert!(soon(|| !lab.view().tab(&tab).labels().contains(&"milo:preview")));

    // Run both real children against the simulated viewer. A Python peer
    // has no native Rex window to match the pinned number.
    fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{root}/preview-postspawn-pid'\nexec '{child}' \"$@\"\n"
        ),
    )
    .unwrap();
    assert_eq!(launch_startup("right").trim(), "ok");
    viewer_session("session.own_block", &session);
    let postspawn = lab.view().tab(&tab).pane("milo:preview").block.clone();
    let error_started = Instant::now();
    let capture = || lab.rex(&["capture", "-s", &session, "-b", &postspawn, "--trim"]);
    assert!(soon(|| capture().contains("Press Enter to dismiss")), "{}", capture());
    let error = capture();
    assert!(error.contains("WebKit preview stopped") && !error.contains(url), "{error}");
    let pid = lab.read("preview-postspawn-pid").trim().parse::<u32>().unwrap();
    assert!(alive(pid), "the diagnostic pane ended before the user could read it");
    let children = lab.command("/usr/bin/pgrep").args(["-P", &pid.to_string()]).output().unwrap();
    assert!(children.stdout.is_empty() && children.status.code() == Some(1), "{children:?}");
    assert!(soon(|| !lab.view().tab(&tab).labels().contains(&"milo:preview")));
    assert!(error_started.elapsed() <= Duration::from_secs(17));

    // A successful anchor can also exit in less than Rex's quick-exit time
    // (for example when closed during startup) without leaving a dead pane.
    fs::write(&launcher, format!("#!/bin/sh\n/bin/sleep 0.1\nprintf done > '{root}/preview-quick-exit'\nexit 0\n"))
        .unwrap();
    assert_eq!(launch_startup("right").trim(), "ok");
    assert!(soon(|| lab.read("preview-quick-exit") == "done"));
    assert!(soon(|| !lab.view().tab(&tab).labels().contains(&"milo:preview")));

    // One Mac app can retain four session attachments. Keep the working
    // shell focused and the published preview visible through a resize.
    for name in ["also-attached-a", "also-attached-b", "also-attached-c"] {
        let created = lab.rex(&["new", name, "-c", root, "--shell=none", "--json", "--", "/bin/sleep", "300"]);
        viewer_session("session.attach", &id(&created, "session_id"));
    }
    scope(&format!(
        "for _,c in ipairs(assert(rex.call('client.list',{{kinds={{'app'}}}})).clients) do \
         if c.client_id=='{client}' then assert(#c.session_ids==4 and c.principal.pid=={viewer_pid}); return end end; \
         error('missing lab viewer')"
    ));
    lab.rex(&["focus", "-s", &session, &main]);
    assert_eq!(launch(&main, "right").trim(), "ok");
    check_focus(&main);
    assert!(soon(|| lab.read("preview-arguments").ends_with(&format!("\n{native_window_id}\n"))));
    let anchor_pid = lab.read("preview-arguments").lines().next().unwrap().parse::<u32>().unwrap();
    let anchor_block = lab.view().tab(&tab).pane("milo:preview").block.clone();
    viewer_session("session.own_block", &session);
    let started_at = unix_seconds();
    let owned_watch = start_watch(&anchor_block, anchor_pid);
    let records = || fs::read_to_string(owned_watch.with_extension("watch.log")).unwrap_or_default();
    let expect_states = |expected: &[&str]| {
        assert!(
            soon(|| watch_changes(&records(), started_at).eq(expected.iter().copied())),
            "{}",
            records()
        );
    };
    let first = show_for(&anchor_block);
    expect_states(&["hide", &first]);
    scope(&format!(
        "assert(rex.session.resize_pane{{direction='horizontal', before_block_id='{main}', \
         after_block_id='{anchor_block}', ratio=0.65}})"
    ));
    let resized = show_for(&anchor_block);
    assert_ne!(resized, first);
    let visible = ["hide", first.as_str(), resized.as_str()];
    expect_states(&visible);
    let started = Instant::now();
    let (mut renewals, mut full_snapshots) = (0, 0);
    while started.elapsed() < Duration::from_secs(13) {
        let log = records();
        assert!(watch_changes(&log, started_at).eq(visible), "{log}");
        (renewals, full_snapshots) = watch_records(&log, started_at)
            .skip_while(|line| *line != resized.as_str())
            .fold((0, 0), |(renew, full), line| {
                (renew + usize::from(line == "renew"), full + usize::from(line == resized.as_str()))
            });
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(renewals >= 8 && full_snapshots >= 2, "missing sustained heartbeats: {}", records());
    check_focus(&main);
    let (mut other, input, output, other_id) = transfer(&anchor_block);
    scope(&format!(
        "local b=assert(rex.session.describe_block{{block_id='{anchor_block}'}}).block; assert(b.owner=='{other_id}')"
    ));
    expect_states(&["hide", &first, &resized, "hide"]);
    lab.rex(&["focus", "-s", &session, &anchor_block]);
    assert_eq!(focus_main(&anchor_block).trim(), "focus");
    check_focus(&anchor_block);
    viewer_session("session.own_block", &session);
    drop((input, output));
    assert!(other.wait().unwrap().success());
    expect_states(&["hide", &first, &resized, "hide", &resized]);
    assert_eq!(focus_main(&anchor_block).trim(), "ok");
    check_focus(&main);
    // The departure must conceal the pane even if the observer's first
    // snapshot after these calls already sees the original tab again.
    scope(&format!(
        "assert(rex.session.focus_window{{window_id='{second_tab}'}}); \
         assert(rex.session.focus_window{{window_id='{tab}'}})"
    ));
    expect_states(&["hide", &first, &resized, "hide", &resized, "hide", &resized]);
    check_focus(&main);
    close(&main);
    expect_states(&["hide", &first, &resized, "hide", &resized, "hide", &resized, "hide"]);
    assert!(soon(|| fs::read_to_string(owned_watch.with_extension("watch.exit")).is_ok_and(|code| code.trim() == "0")));
    close(&anchor_block);
}

/// The floating reader the outline opens in Rex, edited through Rex's own keys.
#[test]
#[ignore = "needs an installed Rex; starts a disposable server"]
fn a_floating_reader_holds_the_outlines_files_as_tabs_and_keeps_their_unsaved_edits() {
    let mut lab = Lab::start();
    lab.start_app();
    let dir = lab.root.join("edit");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.txt"), "first\n").unwrap();
    fs::write(dir.join("b.txt"), "second\n").unwrap();
    let [session, tab] = lab.new_session("edit", "files", &dir);
    let milo = lab.milo.to_str().unwrap().to_owned();
    let outline_in = |session: &str, tab: &str, dir: &Path| {
        let place = ["-s", session, "-w", tab, "-c", dir.to_str().unwrap()];
        let outline = ["--split=left", "--shell=none", "--label", "outline", "--", milo.as_str(), "outline"];
        lab.rex(&[["run"].as_slice(), &place, &outline].concat());
    };
    outline_in(&session, &tab, &dir);

    // The reader is named after the file it was last asked for; what it runs is checked by what its screen shows.
    let pane_in = |tab: &str, label: &str| {
        let view = lab.view();
        view.tab(tab).panes.iter().find(|pane| pane.label == label).map(|pane| pane.block.clone())
    };
    let pane = |label: &str| pane_in(&tab, label);
    let panes = || lab.view().tab(&tab).panes.len();
    let screen_in = |session: &str, block: &str| lab.rex(&["capture", "-s", session, "-b", block, "--trim"]);
    let screen = |block: &str| screen_in(&session, block);
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
    let with_reader = panes();
    shows(&reader, "first");
    shows(&reader, "e edit");
    keys(&reader, &["e"]);
    shows(&reader, "^S save");
    lab.rex(&["send", "-s", &session, "-b", &reader, "typed "]);
    shows(&reader, "unsaved");
    shows(&reader, "typed first");
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "first\n");

    // A second file is a tab of the same reader, beside the draft, and no second window.
    keys(&outline, &["down", "enter"]);
    shows(&reader, "*a.txt  b.txt ×");
    shows(&reader, "second");
    assert!(soon(|| pane("b.txt").as_deref() == Some(reader.as_str())), "{:?}", lab.view());
    assert_eq!(panes(), with_reader);

    // The file that already has a tab is shown in it, with its draft, however it is asked for.
    keys(&outline, &["up", "enter"]);
    shows(&reader, "*a.txt ×  b.txt");
    shows(&reader, "typed first");
    keys(&reader, &["ctrl+n"]);
    shows(&reader, "*a.txt  b.txt ×");
    keys(&reader, &["ctrl+p"]);
    shows(&reader, "*a.txt ×  b.txt");
    assert_eq!((panes(), fs::read_to_string(dir.join("a.txt")).unwrap().as_str()), (with_reader, "first\n"));

    // Closing the tab asks about its draft; saved, the tab goes and the other file is the whole reader again.
    keys(&reader, &["ctrl+w"]);
    shows(&reader, "Unsaved edits");
    keys(&reader, &["s"]);
    assert!(soon(|| !screen(&reader).contains("a.txt")), "{}", screen(&reader));
    shows(&reader, "second");
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "typed first\n");

    // Another project's outline, in a session of its own, gets a reader of its own.
    let other = lab.root.join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("c.txt"), "third\n").unwrap();
    let [other_session, other_tab] = lab.new_session("other", "files", &other);
    outline_in(&other_session, &other_tab, &other);
    assert!(soon(|| pane_in(&other_tab, "outline").is_some()), "{:?}", lab.view());
    let other_outline = pane_in(&other_tab, "outline").unwrap();
    assert!(soon(|| screen_in(&other_session, &other_outline).contains("c.txt")));
    lab.rex(&["send-key", "-s", &other_session, "-b", &other_outline, "enter"]);
    assert!(soon(|| pane_in(&other_tab, "c.txt").is_some()), "{:?}", lab.view());
    let other_reader = pane_in(&other_tab, "c.txt").unwrap();
    assert!(soon(|| screen_in(&other_session, &other_reader).contains("third")));
    assert!(!screen(&reader).contains("c.txt") && panes() == with_reader, "{}", screen(&reader));

    // A reader that Rex closes keeps the unsaved edits of every tab outside the project.
    keys(&outline, &["enter"]);
    shows(&reader, "b.txt  a.txt ×");
    keys(&reader, &["e"]);
    shows(&reader, "^S save");
    lab.rex(&["send", "-s", &session, "-b", &reader, "also "]);
    shows(&reader, "b.txt  *a.txt ×");
    keys(&reader, &["ctrl+p", "e"]);
    shows(&reader, "^S save");
    lab.rex(&["send", "-s", &session, "-b", &reader, "lost? "]);
    shows(&reader, "*b.txt ×  *a.txt");
    lab.rex(&["block", "close", "-s", &session, &reader]);
    let drafts = lab.root.join("home/.local/state/milo/drafts");
    let kept = || {
        let entries = fs::read_dir(&drafts).into_iter().flatten().flatten();
        let mut kept: Vec<String> = entries.filter_map(|entry| fs::read_to_string(entry.path()).ok()).collect();
        kept.sort();
        kept
    };
    assert!(soon(|| kept() == ["also typed first\n", "lost? second\n"]), "{:?}", kept());
    assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "second\n");

    // The outline can open a file again, and the editor offers its draft back.
    keys(&outline, &["down", "enter"]);
    assert!(soon(|| pane("b.txt").is_some()), "{:?}", lab.view());
    let reader = pane("b.txt").unwrap();
    shows(&reader, "second");
    keys(&reader, &["e"]);
    shows(&reader, "Kept draft found");
    keys(&reader, &["r", "ctrl+s"]);
    assert!(soon(|| fs::read_to_string(dir.join("b.txt")).unwrap() == "lost? second\n"));
    assert!(soon(|| kept() == ["also typed first\n"]), "{:?}", kept());
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
