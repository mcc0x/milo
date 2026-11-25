//! Runs the real `milo menubar attention` commands the menu app uses for its
//! Needs you section, first against a stand-in for `rex` and then, when asked,
//! against a disposable Rex server.
//!
//! Each test has a home of its own with its own `rex` first on the path, so
//! no test can reach the Rex of whoever runs it. The stand-in prints what a
//! test writes, as Rex's script prints what Rex tells it, and answers the
//! jump's questions from files. The ignored test needs Rex installed: it
//! starts a server on a socket in its own folder and pins every call to it.

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const HOST: &str = "27ea019d-95c6-4fba-ab3c-d19b89cbb459";
const OTHER_HOST: &str = "27ea019d-95c6-4fba-ab3c-d19b89cbb450";
const SESSION: &str = "session:01a21295-d49e-78d9-9143-37837c6d246c";
const WINDOW: &str = "window:01a21295-d4b1-789b-b97a-a06ba3837cbf";
const ONE: &str = "block:01a21295-d49e-7bcb-a89c-ef73e104fd7e";
const TWO: &str = "block:01a2129a-2773-77db-94ac-4c4b3ecf330a";
const APP: &str = "client:01a21295-d097-70de-b037-f56cd986d699";
/// Longer than the watch waits before it asks for Rex again.
const RETRY: Duration = Duration::from_secs(20);
static NEXT: AtomicU32 = AtomicU32::new(0);

/// As a script host it prints the lines of `rex-says`. Every other call is
/// logged and answered from a file the test wrote, or fails without one.
const REX: &str = r#"#!/bin/sh
case " $* " in
*" do --all "*) echo $$ > "$HOME/rex-pid"; exec tail -n +1 -f "$HOME/rex-says" ;;
esac
printf '%s\n' "$*" >> "$HOME/rex-calls"
case " $* " in
*" whoami "*) exec cat "$HOME/whoami.json" ;;
*" block inspect "*) exec cat "$HOME/block.json" ;;
*" api call "*) [ -e "$HOME/focus-ok" ] && echo '{"revision":2}' ;;
*" client ls "*) exec cat "$HOME/clients.json" ;;
*" session.select "*) [ -e "$HOME/select-ok" ] ;;
*) exit 1 ;;
esac
"#;

struct Home {
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        // A socket's path must be short, so this is not under the system's temporary folder.
        let name = format!("/tmp/milo-at-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let home = Self { root: PathBuf::from(name) };
        let _ = fs::remove_dir_all(&home.root);
        fs::create_dir_all(home.root.join("bin")).unwrap();
        fs::write(home.root.join("rex-says"), "").unwrap();
        home.install_rex(REX);
        home
    }

    fn install_rex(&self, script: &str) {
        let rex = self.root.join("bin/rex");
        fs::write(&rex, script).unwrap();
        fs::set_permissions(&rex, fs::Permissions::from_mode(0o700)).unwrap();
        // A test thread's fork can briefly inherit the file open for writing.
        assert!(soon(|| Command::new(&rex).arg("ready?").env("HOME", &self.root).status().is_ok()));
        let _ = fs::remove_file(self.root.join("rex-calls"));
    }

    fn milo(&self, args: &[&str]) -> Command {
        let mut milo = Command::new(env!("CARGO_BIN_EXE_milo"));
        milo.args(args)
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.root.join("bin").display()))
            .env("TMPDIR", &self.root)
            .current_dir(&self.root);
        milo
    }

    /// Runs a command that must succeed and returns what it printed.
    fn say(&self, args: &[&str]) -> String {
        let output = self.milo(args).stdin(Stdio::null()).output().unwrap();
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.root.join(name), text).unwrap();
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }

    /// What Rex's script would print next.
    fn rex_says(&self, report: &str) {
        let mut file = fs::OpenOptions::new().append(true).open(self.root.join("rex-says")).unwrap();
        writeln!(file, "milo-attention {report}").unwrap();
    }

    fn rex_pid(&self) -> Option<i32> {
        self.read("rex-pid").trim().parse().ok()
    }

    fn watch(&self) -> Watch {
        let mut child =
            self.milo(&["menubar", "attention", "watch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let (input, output) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
        let (send, frames) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Watch { child, input: Some(input), frames }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(pid) = self.rex_pid() {
            let _ = Command::new("kill").arg(pid.to_string()).stderr(Stdio::null()).status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A running `milo menubar attention watch`, as the menu app holds one.
struct Watch {
    child: Child,
    input: Option<ChildStdin>,
    frames: Receiver<String>,
}

impl Watch {
    /// The next frame, which must come and must satisfy `expected`.
    fn frame(&self, what: &str, expected: impl Fn(&str) -> bool) -> String {
        let frame = self.frames.recv_timeout(RETRY).unwrap_or_else(|_| panic!("no frame: {what}"));
        assert!(expected(&frame), "{what}: {frame}");
        frame
    }

    /// Skips frames until one satisfies `expected`. A real Rex reports a pane in steps.
    fn eventually(&self, what: &str, expected: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + RETRY;
        let mut last = String::new();
        while let Ok(frame) = self.frames.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            if expected(&frame) {
                return frame;
            }
            last = frame;
        }
        panic!("{what}: last frame {last}");
    }

    fn quiet(&self) {
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(self.frames.try_recv().ok(), None, "a frame nobody asked for");
    }

    fn send(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().unwrap();
        input.write_all(bytes).unwrap();
        input.flush().unwrap();
    }

    fn ended(&mut self) -> bool {
        soon(|| self.child.try_wait().is_ok_and(|status| status.is_some_and(|status| status.success())))
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // Closing its input ends a watch. One that a test already ended is only reaped.
        self.input = None;
        if !soon(|| self.child.try_wait().is_ok_and(|status| status.is_some())) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn soon(mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn alive(pid: i32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Each row of a frame as `name state`, in order.
fn rows(frame: &str) -> Vec<String> {
    frame
        .split(r#""name":""#)
        .skip(1)
        .map(|entry| {
            let name = entry.split('"').next().unwrap();
            let state = entry.split(r#""state":""#).nth(1).unwrap().split('"').next().unwrap();
            format!("{name} {state}")
        })
        .collect()
}

fn block(block: &str, name: &str, records: &str) -> String {
    format!(
        r#"{{"block":"{block}","session":"{SESSION}","window":"{WINDOW}","name":"{name}","tab":"Tests","records":[{records}]}}"#
    )
}

fn record(state: &str) -> String {
    format!(r#"{{"id":"","state":"{state}","app":"agent","at":"2026-11-18T00:00:00Z"}}"#)
}

#[test]
fn while_the_section_is_off_nothing_runs_and_rex_is_never_asked() {
    let home = Home::new();
    assert!(home.say(&["menubar", "attention"]).starts_with("Needs you is off. Milo is not watching Rex for it."));
    let mut watch = home.watch();
    assert!(watch.ended());
    assert_eq!(watch.frames.try_recv().ok(), None);
    // A choice this build cannot read is off too, not a watch with a guessed meaning.
    fs::create_dir_all(home.root.join(".config/milo")).unwrap();
    home.write(".config/milo/menubar.conf", "attention = always\n");
    assert!(home.watch().ended());
    assert_eq!(home.rex_pid(), None);
    assert_eq!(home.read("rex-calls"), "");
}

#[test]
fn rows_follow_the_chosen_states_one_row_is_marked_seen_and_a_lost_rex_counts_nothing() {
    let home = Home::new();
    assert!(home.say(&["menubar", "attention", "dot"]).contains("with a dot in the menu bar"));
    let mut watch = home.watch();
    // Nothing is said until Rex has reported in full.
    home.rex_says(&format!(r#"{{"server":"{HOST}"}}"#));
    home.rex_says(&block(ONE, "one", &record("blocked")));
    watch.quiet();
    home.rex_says(r#"{"ready":true}"#);
    let frame = watch.frame("the first report", |frame| rows(frame) == ["one waiting"]);
    assert!(frame.contains(r#""rex":"connected","dot":true"#) && frame.contains(r#""tab":"Tests""#), "{frame}");
    assert!(frame.contains(&format!(r#""host":"{HOST}","block":"{ONE}""#)), "{frame}");

    // Finished work is left out until it is chosen. Each state is chosen on its own.
    home.rex_says(&block(TWO, "two", &record("done")));
    watch.quiet();
    home.say(&["menubar", "attention", "states", "waiting,done"]);
    watch.send(&[0]);
    watch.frame("finished work chosen", |frame| rows(frame) == ["one waiting", "two done"]);
    home.say(&["menubar", "attention", "states", "errors"]);
    watch.send(&[0]);
    watch.frame("only errors chosen", |frame| rows(frame).is_empty());
    home.say(&["menubar", "attention", "states", "waiting,errors,done"]);
    watch.send(&[0]);
    watch.frame("every state chosen", |frame| rows(frame) == ["one waiting", "two done"]);

    // The menu-only choice lists the same rows without the dot.
    home.say(&["menubar", "attention", "menu"]);
    watch.send(&[0]);
    watch.frame("menu only", |frame| frame.contains(r#""dot":false"#) && rows(frame).len() == 2);

    // Work resumes in one pane: its row goes, the other stays.
    home.rex_says(&format!(r#"{{"block":"{ONE}","clear":true}}"#));
    watch.frame("work resumed", |frame| rows(frame) == ["two done"]);
    home.rex_says(&block(ONE, "one", &record("error")));
    watch.frame("a failure", |frame| rows(frame) == ["one error", "two done"]);
    // Marking one row seen removes that row only.
    watch.send(format!("seen {TWO}\n").as_bytes());
    watch.frame("marked seen", |frame| rows(frame) == ["one error"]);

    // Rex goes away. Its panes are not counted, and the watch asks again by itself.
    home.write("rex-says", "");
    let first = home.rex_pid().unwrap();
    assert!(Command::new("kill").arg(first.to_string()).status().unwrap().success());
    watch.frame("Rex lost", |frame| frame.contains(r#""rex":"waiting","dot":false,"entries":[]"#));
    assert!(soon(|| home.rex_pid().is_some_and(|pid| pid != first)), "Rex was not asked again");
    home.rex_says(&format!(r#"{{"server":"{HOST}"}}"#));
    home.rex_says(&block(TWO, "two", &record("done")));
    home.rex_says(&block(ONE, "one", &record("blocked")));
    home.rex_says(r#"{"ready":true}"#);
    // The reconnect starts from what Rex reports now, and what was seen stays seen.
    watch.frame("Rex back", |frame| frame.contains(r#""rex":"connected""#) && rows(frame) == ["one waiting"]);

    // Turned off, the watch ends and takes its Rex script with it.
    let script = home.rex_pid().unwrap();
    home.say(&["menubar", "attention", "off"]);
    watch.send(&[0]);
    assert!(watch.ended());
    assert!(soon(|| !alive(script)), "the Rex script outlived the watch");
}

#[test]
fn a_watch_that_is_told_to_end_or_loses_its_listener_takes_its_rex_script_with_it() {
    for ending in ["terminated", "listener gone"] {
        let home = Home::new();
        home.say(&["menubar", "attention", "menu"]);
        let mut watch = home.watch();
        home.rex_says(&format!(r#"{{"server":"{HOST}"}}"#));
        home.rex_says(r#"{"ready":true}"#);
        watch.frame("attached", |frame| frame.contains(r#""rex":"connected""#));
        let script = home.rex_pid().unwrap();
        assert!(alive(script));
        if ending == "terminated" {
            // As the menu app ends it when it quits or the section is turned off.
            assert!(Command::new("kill").arg(watch.child.id().to_string()).status().unwrap().success());
        } else {
            watch.input = None;
        }
        assert!(soon(|| watch.child.try_wait().is_ok_and(|status| status.is_some())), "{ending}");
        assert!(soon(|| !alive(script)), "{ending}: the Rex script went on watching");
    }
}

#[test]
fn a_jump_checks_the_host_and_the_pane_before_it_changes_anything() {
    let home = Home::new();
    let jump = |host: &str| home.say(&["menubar", "attention", "jump", host, ONE]);
    let calls = || home.read("rex-calls");
    let who =
        |host: &str| format!(r#"{{"principal":{{"kind":"peer","transport":"unix","uid":0}},"host_id":"{host}"}}"#);
    let placed = format!(
        r#"{{"session_id":"{SESSION}","block":{{"block_id":"{ONE}","creator_name":"com.superlogical.terminal",
            "host_id":"{HOST}","placement":"placed","window_id":"{WINDOW}"}}}}"#
    );
    // SAFETY: Getting the current effective user ID touches no memory.
    let uid = unsafe { libc::geteuid() };
    let app = format!(
        r#"{{"client_id":"{APP}","info":{{"kind":"app"}},"connection_state":"connected",
            "principal":{{"kind":"peer","transport":"unix","uid":{uid},"pid":4242}}}}"#
    );

    // No Rex answers.
    assert!(jump(HOST).starts_with("unavailable\nRex isn’t answering"));
    // Another server answers where the pane's server was: nothing is looked up or focused there.
    home.write("whoami.json", &who(OTHER_HOST));
    home.write("block.json", &placed);
    home.write("focus-ok", "");
    assert!(jump(HOST).starts_with("unavailable\nThat pane was reported by a different Rex server"));
    assert!(!calls().contains("inspect") && !calls().contains("api call"), "{}", calls());
    // The pane has closed, or the block is now another host's.
    home.write("whoami.json", &who(HOST));
    home.write("block.json", &placed.replace(r#""placed""#, r#""detached""#));
    assert!(jump(HOST).starts_with("unavailable\nThat pane has closed"));
    home.write("block.json", &placed.replace(HOST, OTHER_HOST));
    assert!(jump(HOST).starts_with("unavailable\nThat pane has closed"));
    assert!(!calls().contains("api call"), "{}", calls());

    // The pane is there. It is focused by its whole ID in the session Rex says holds it now.
    home.write("block.json", &placed);
    home.write("clients.json", r#"{"clients":[]}"#);
    assert!(jump(HOST).starts_with("focused\nThe pane is focused in its session, but no Rex app on this Mac"));
    let focus = format!(r#"api call -s {SESSION} session.focus_block {{"block_id":"{ONE}"}}"#);
    assert!(calls().contains(&focus), "{}", calls());
    assert!(!calls().contains("session.select"), "{}", calls());

    // One app of this person's is connected here. Rex refuses, as it does with Remote Control off:
    // the jump says so and asks for nothing else.
    home.write("clients.json", &format!(r#"{{"clients":[{app}]}}"#));
    let refused = jump(HOST);
    assert!(refused.starts_with("focused\n") && refused.contains("Milo doesn’t change that setting"), "{refused}");
    let select = format!("-C {APP} do session.select session_id={SESSION} window_id={WINDOW}");
    assert!(calls().contains(&select), "{}", calls());
    // The app takes the request: the menu is told which process to bring forward.
    home.write("select-ok", "");
    assert_eq!(jump(HOST), "shown 4242\n");
    // Milo never starts a server, turns a Rex setting on or sends anything to a terminal.
    for call in calls().lines() {
        assert!(call.starts_with("--autostart=false --timeout "), "{call}");
        assert!(
            !["config", "send", "capture", "server", "hosts"]
                .iter()
                .any(|word| call.split(' ').any(|arg| arg == *word)),
            "{call}"
        );
    }
    // Two apps here: Milo does not pick one.
    home.write("clients.json", &format!(r#"{{"clients":[{app},{}]}}"#, app.replace("d699", "d698")));
    assert!(jump(HOST).starts_with("focused\nThe pane is focused in its session. Several Rex apps"));
}

mod real_rex {
    use super::*;

    struct Server(Child);

    impl Drop for Server {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                // SAFETY: This is our unreaped disposable server child, not a discovered PID.
                unsafe { libc::kill(self.0.id().cast_signed(), libc::SIGTERM) };
                if !soon(|| self.0.try_wait().is_ok_and(|status| status.is_some())) {
                    let _ = self.0.kill();
                }
            }
            let _ = self.0.wait();
        }
    }

    /// A lab of one disposable server. Every `rex` Milo or the test runs is the wrapper, pinned to its socket.
    struct Lab {
        home: Home,
        rex: PathBuf,
        socket: String,
    }

    impl Lab {
        fn new() -> Self {
            let home = Home::new();
            let path = std::env::var_os("PATH").unwrap_or_default();
            let rex = std::env::var_os("MILO_TEST_REX")
                .map(PathBuf::from)
                .or_else(|| std::env::split_paths(&path).map(|dir| dir.join("rex")).find(|path| path.is_file()))
                .expect("set MILO_TEST_REX to the rex executable")
                .canonicalize()
                .unwrap();
            let socket = format!("unix://{}", home.root.join("rex.sock").display());
            assert!(!rex.to_string_lossy().contains('\'') && !socket.contains('\''));
            home.install_rex(&format!(
                "#!/bin/sh\nrex='{}'\nendpoint='{socket}'\n{}",
                rex.display(),
                r#"for arg do
    case "$arg" in
        -S|--server|-S*|--server=*|--autostart) exit 1 ;;
        --autostart=*) [ "$arg" = --autostart=false ] || exit 1 ;;
        --) break ;;
    esac
done
[ -S "${endpoint#unix://}" ] || exit 1
exec "$rex" --autostart=false -S "$endpoint" "$@"
"#
            ));
            home.write("init.lua", "");
            Self { home, rex, socket }
        }

        fn command(&self, program: &Path) -> Command {
            let root = &self.home.root;
            let mut command = Command::new(program);
            command
                .env_clear()
                .env("HOME", root)
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("XDG_CONFIG_HOME", root.join(".config"))
                .env("XDG_STATE_HOME", root.join(".local/state"))
                .env("XDG_RUNTIME_DIR", root)
                .env("TMPDIR", root)
                .env("ZDOTDIR", root)
                .current_dir(root)
                .stdin(Stdio::null());
            command
        }

        /// Runs the pinned `rex` and returns what it printed.
        fn rex(&self, args: &[&str]) -> String {
            let output = self.command(&self.home.root.join("bin/rex")).args(args).output().unwrap();
            assert!(output.status.success(), "rex {args:?}: {}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8(output.stdout).unwrap()
        }

        fn start(&self) -> Server {
            let root = &self.home.root;
            let child = self
                .command(&self.rex)
                .args(["server", "run", "--restore=false", "--listen", &self.socket, "--state-dir"])
                .arg(root.join("rex-state"))
                .arg("--config")
                .arg(root.join("init.lua"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let server = Server(child);
            let answers = || {
                let wrapper = self.home.root.join("bin/rex");
                self.command(&wrapper).args(["whoami", "--json"]).output().is_ok_and(|out| out.status.success())
            };
            assert!(soon(answers), "disposable Rex did not start");
            server
        }

        /// A session of one terminal whose program reports each status the test writes to `feed`.
        /// Returns the terminal's block ID.
        fn pane(&self, label: &str, feed: &str) -> String {
            self.home.write(feed, "");
            let script = format!(
                r#"tail -n +1 -f '{}' | while IFS= read -r status; do printf '\033]7501;%s\033\\' "$status"; done"#,
                self.home.root.join(feed).display()
            );
            let created = self.rex(&["new", label, "--shell", "none", "--json", "--", "/bin/sh", "-c", &script]);
            id_after(&created, "\"block:").expect("a block ID")
        }

        fn reports(&self, feed: &str, status: &str) {
            let mut file = fs::OpenOptions::new().append(true).open(self.home.root.join(feed)).unwrap();
            writeln!(file, "{status}").unwrap();
        }
    }

    /// The first whole ID in `text` that starts with `prefix`, such as `"block:`.
    fn id_after(text: &str, prefix: &str) -> Option<String> {
        let start = text.find(prefix)? + 1;
        Some(text[start..].split('"').next()?.to_owned())
    }

    fn field(json: &str, key: &str) -> String {
        let marker = format!("\"{key}\": \"");
        json.split(&marker).nth(1).and_then(|rest| rest.split('"').next()).unwrap_or_default().to_owned()
    }

    #[test]
    #[ignore = "requires installed Rex; runs a disposable server on a socket of its own"]
    fn panes_are_listed_from_their_reported_status_jumped_to_exactly_and_dropped_when_rex_stops() {
        let lab = Lab::new();
        let home = &lab.home;
        let server = lab.start();
        let host = field(&lab.rex(&["whoami", "--json"]), "host_id");
        home.say(&["menubar", "attention", "dot"]);
        let mut watch = home.watch();
        watch.frame("nothing needs anyone", |frame| frame.contains(r#""rex":"connected","dot":true,"entries":[]"#));

        // Waiting, then resumed.
        let one = lab.pane("agent-one", "feed-one");
        lab.reports("feed-one", "state=blocked:kind=permission:app=agent:msg=c2VjcmV0IHByb21wdA==");
        let frame = watch.eventually("waiting", |frame| rows(frame) == ["agent-one waiting"]);
        assert!(frame.contains(&format!(r#""host":"{host}","block":"{one}""#)), "{frame}");
        assert!(frame.contains(r#""app":"agent","asks":"permission""#), "{frame}");
        // The program's own words never leave Rex: not its message, not its title.
        assert!(!frame.contains("secret") && !frame.contains("c2VjcmV0"), "{frame}");
        lab.reports("feed-one", "state=working:app=agent");
        watch.eventually("resumed", |frame| rows(frame).is_empty());

        // Finished work is listed once it is chosen; a failure in another pane leads it.
        lab.reports("feed-one", "state=done:app=agent");
        watch.quiet();
        home.say(&["menubar", "attention", "states", "waiting,errors,done"]);
        watch.send(&[0]);
        watch.eventually("done", |frame| rows(frame) == ["agent-one done"]);
        let two = lab.pane("agent-two", "feed-two");
        lab.reports("feed-two", "state=error:app=agent");
        watch.eventually("error and done", |frame| rows(frame) == ["agent-two error", "agent-one done"]);

        // The jump focuses the very pane, in a tab that was not the active one.
        let session = field(&lab.rex(&["block", "inspect", &one, "--json"]), "session_id");
        let tab = field(&lab.rex(&["block", "inspect", &one, "--json"]), "window_id");
        lab.rex(&["window", "new", "Other", "-s", &session, "--json"]);
        assert_ne!(field(&lab.rex(&["session", "inspect", &session, "--json"]), "active_window_id"), tab);
        // A pane from a server that is no longer this one is refused, and nothing moves.
        let refused = home.say(&["menubar", "attention", "jump", OTHER_HOST, &one]);
        assert!(refused.starts_with("unavailable\nThat pane was reported by a different Rex server"), "{refused}");
        assert_ne!(field(&lab.rex(&["session", "inspect", &session, "--json"]), "active_window_id"), tab);
        // No Rex app is connected to this server, so the supported fallback is the focused pane.
        let jumped = home.say(&["menubar", "attention", "jump", &host, &one]);
        assert!(jumped.starts_with("focused\nThe pane is focused in its session, but no Rex app"), "{jumped}");
        let view = lab.rex(&["session", "inspect", &session, "--json"]);
        assert_eq!(field(&view, "active_window_id"), tab);
        assert!(view.contains(&format!("\"focused_block_id\": \"{one}\"")), "{view}");

        // Marking the finished pane seen leaves the failed one listed.
        watch.send(format!("seen {one}\n").as_bytes());
        watch.eventually("seen", |frame| rows(frame) == ["agent-two error"]);

        // The server stops: its panes are no longer counted. A new one is heard without another choice.
        drop(server);
        watch.eventually("Rex stopped", |frame| frame.contains(r#""rex":"waiting","dot":true,"entries":[]"#));
        let closed = home.say(&["menubar", "attention", "jump", &host, &two]);
        assert!(closed.starts_with("unavailable\nRex isn’t answering"), "{closed}");
        let _restarted = lab.start();
        watch.eventually("Rex back", |frame| frame.contains(r#""rex":"connected","dot":true,"entries":[]"#));
        lab.pane("agent-three", "feed-three");
        lab.reports("feed-three", "state=blocked:kind=question:app=agent");
        watch.eventually("waiting again", |frame| rows(frame) == ["agent-three waiting"]);

        home.say(&["menubar", "attention", "off"]);
        watch.send(&[0]);
        assert!(watch.ended());
    }
}
