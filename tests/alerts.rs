//! Runs the real `milo alerts` with stand-ins for Rex, Telegram and Discord.
//!
//! The stand-in `rex` prints the events a test writes, as `rex events` would.
//! A server on this machine's loopback address takes the place of both
//! destinations and records every request, so nothing leaves the machine.
//! Secrets are synthetic and kept in the test's own folder: a file where
//! Milo uses one, and on macOS a keychain made for the test and on no list.
//!
//! Only a development build of Milo accepts these stand-ins. Against a
//! release build, which would speak to the real destinations, nothing runs.

#[cfg(debug_assertions)]
#[path = "alerts/real_rex.rs"]
mod real_rex;

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead as _, BufReader, ErrorKind, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const TOKEN: &str = "123456:SYNTHETICsyntheticSYNTHETIC00";
const LINK: &str = "https://discord.com/api/webhooks/1234567890/SYNTHETIC_synthetic-SYNTHETIC0";
/// A development relay connection. No relay is configured in the fixture,
/// so it can never be revoked except by the user's separate local-only choice.
const LOST_CONNECTION: &str = "milorelay_SYNTHETICsyntheticSYNTHETICsyntheticSYNTHET";
const TELEGRAM: &str = "/bot";
const DISCORD: &str = "/api/v10/webhooks/";
const ENABLED: &str = "Milo: Rex alert forwarding was enabled.";
const PAUSED: &str = "Milo: Rex alert forwarding was paused.";
const RECOVERED: &str = "Milo: Rex alert forwarding recovered after an interruption.";

/// Emulates a login command, a reachability check, or an event subscription.
/// `shown` marks the prefix not to replay on new subscriptions. A live stream
/// exits when Rex is taken down or its disposable home disappears.
const REX: &str = r#"#!/bin/sh
case "$1" in
    ready|do) exit 0 ;;
esac
case " $* " in
    *" ls "*)
        [ ! -f "$HOME/rex-down" ] || exit 1
        printf '%s\n' '[]'
        exit 0 ;;
    *" events "*) ;;
    *) exit 1 ;;
esac
printf '%s\n' "$*" >> "$HOME/started"
[ ! -f "$HOME/rex-down" ] && [ ! -f "$HOME/rex-flaps" ] || exit 1
if [ -p "$HOME/rex-lease" ]; then
    exec 4>"$HOME/rex-lease"
    printf x >&4
fi
exec 3<"$HOME/stream"
if [ -f "$HOME/shown" ]; then
    while IFS= read -r old; do
        IFS= read -r old <&3 || exit 1
    done < "$HOME/shown"
fi
while [ -d "$HOME" ] && [ ! -f "$HOME/rex-down" ]; do
    if IFS= read -r line <&3; then
        printf '%s\n' "$line" || exit 0
    else
        sleep 0.2
    fi
done
"#;
static NEXT: AtomicU32 = AtomicU32::new(0);

/// One request a destination received.
#[derive(Clone, Debug)]
struct Request {
    path: String,
    body: String,
}

struct Reply {
    path_prefix: &'static str,
    status: u16,
    body: &'static str,
    gate: Option<ReplyGate>,
}

struct ReplyGate {
    release: mpsc::Receiver<()>,
    socket: mpsc::Sender<HeldConnection>,
}

struct HeldReply {
    _release: mpsc::Sender<()>,
    socket: mpsc::Receiver<HeldConnection>,
}

impl HeldReply {
    fn connection(&self) -> HeldConnection {
        let connection =
            self.socket.recv_timeout(Duration::from_secs(30)).expect("request never reached the reply gate");
        connection.assert_pending();
        connection
    }
}

struct HeldConnection {
    socket: TcpStream,
    received: Instant,
}

impl HeldConnection {
    fn assert_pending(&self) {
        self.socket.set_nonblocking(true).unwrap();
        let result = self.socket.peek(&mut [0]);
        self.socket.set_nonblocking(false).unwrap();
        assert!(
            matches!(result, Err(error) if error.kind() == ErrorKind::WouldBlock),
            "client not waiting for a reply"
        );
    }

    fn assert_cancelled(&self) {
        self.socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        match self.socket.peek(&mut [0]) {
            Ok(0) => {}
            Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
            result => panic!("held HTTP request was not cancelled: {result:?}"),
        }
        // Curl has a 15-second deadline. Waiting for its own timeout must not pass.
        assert!(self.received.elapsed() < Duration::from_secs(12), "held HTTP request did not close promptly");
    }
}

/// Stands in for Telegram and Discord. It answers with the first queued
/// reply matching the request's path, or with success when none matches.
struct Destination {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
    answers: Arc<Mutex<VecDeque<Reply>>>,
    stop: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
}

impl Destination {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let answers = Arc::new(Mutex::new(VecDeque::<Reply>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, queued, stopping) = (Arc::clone(&requests), Arc::clone(&answers), Arc::clone(&stop));
        let server = thread::spawn(move || {
            thread::scope(|scope| {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let Some(request) = read_request(&mut stream) else { continue };
                    let reply = {
                        let mut queued = queued.lock().unwrap();
                        let index = queued.iter().position(|reply| request.path.starts_with(reply.path_prefix));
                        index.and_then(|index| queued.remove(index))
                    };
                    seen.lock().unwrap().push(request);
                    if reply.as_ref().is_some_and(|reply| reply.gate.is_some()) {
                        scope.spawn(move || respond(stream, reply));
                    } else {
                        respond(stream, reply);
                    }
                }
            });
        });
        Self { port, requests, answers, stop, server: Some(server) }
    }

    fn answer(&self, answers: &[(u16, &'static str)]) {
        self.answer_at("", answers);
    }

    fn answer_at(&self, path_prefix: &'static str, answers: &[(u16, &'static str)]) {
        let replies = answers.iter().map(|&(status, body)| Reply { path_prefix, status, body, gate: None });
        self.answers.lock().unwrap().extend(replies);
    }

    /// Hold the next reply until the caller drops the returned gate.
    fn hold_reply_at(&self, path_prefix: &'static str, status: u16, body: &'static str) -> HeldReply {
        let (release, released) = mpsc::channel();
        let (send_socket, socket) = mpsc::channel();
        let gate = Some(ReplyGate { release: released, socket: send_socket });
        self.answers.lock().unwrap().push_back(Reply { path_prefix, status, body, gate });
        HeldReply { _release: release, socket }
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    fn requests_at(&self, path_prefix: &str) -> Vec<Request> {
        self.requests.lock().unwrap().iter().filter(|request| request.path.starts_with(path_prefix)).cloned().collect()
    }

    fn texts_at(&self, path_prefix: &str) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.path.starts_with(path_prefix))
            .map(|request| text_of(&request.body))
            .collect()
    }

    /// The text of every message received, whichever destination it was for.
    fn texts(&self) -> Vec<String> {
        self.texts_at("")
    }
}

impl Drop for Destination {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        let _ = self.server.take().unwrap().join();
    }
}

fn respond(mut stream: TcpStream, reply: Option<Reply>) {
    if let Some(gate) = reply.as_ref().and_then(|reply| reply.gate.as_ref()) {
        let _ = gate.socket.send(HeldConnection { socket: stream.try_clone().unwrap(), received: Instant::now() });
        let _ = gate.release.recv_timeout(Duration::from_secs(30));
    }
    let (code, body) = reply.map_or((200, "{\"ok\":true}"), |reply| (reply.status, reply.body));
    let _ = write!(
        stream,
        "HTTP/1.1 {code} Answer\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let path = first.split(' ').nth(1)?.to_owned();
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request { path, body: String::from_utf8(body).ok()? })
}

/// The Telegram text or Discord embed description, with its escapes undone.
fn text_of(body: &str) -> String {
    let start = ["\"description\":\"", "\"text\":\""]
        .iter()
        .find_map(|key| body.find(key).map(|at| at + key.len()))
        .unwrap_or_else(|| panic!("no message in {body}"));
    let mut text = String::new();
    let mut chars = body[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('u') => {
                    let code: String = chars.by_ref().take(4).collect();
                    text.push(char::from_u32(u32::from_str_radix(&code, 16).unwrap()).unwrap());
                }
                Some(other) => text.push(other),
                None => break,
            },
            c => text.push(c),
        }
    }
    text
}

/// A home of the test's own, with its own `rex` first on the path.
struct Home {
    root: PathBuf,
    destination: Destination,
}

impl Home {
    /// None for a release build, which no test may run against.
    fn new() -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = loop {
            let name = format!("milo-alerts-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
            let root = temp.join(name);
            match fs::DirBuilder::new().mode(0o700).create(&root) {
                Ok(()) => break root,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("could not create disposable alerts home: {error}"),
            }
        };
        fs::create_dir(root.join("bin")).unwrap();
        fs::write(root.join("stream"), "").unwrap();
        let rex = root.join("bin/rex");
        fs::write(&rex, REX).unwrap();
        fs::set_permissions(&rex, fs::Permissions::from_mode(0o700)).unwrap();
        // A test thread's fork can briefly inherit the file open for writing.
        assert!(soon(|| Command::new(&rex).arg("ready").status().is_ok_and(|status| status.success())));
        let home = Self { root, destination: Destination::start() };
        if cfg!(target_os = "macos") {
            let made = Command::new("/usr/bin/security")
                .args(["create-keychain", "-p", "test"])
                .arg(home.keychain())
                .status()
                .unwrap();
            assert!(made.success());
        }
        Some(home)
    }

    fn keychain(&self) -> PathBuf {
        self.root.join("test.keychain-db")
    }

    fn milo(&self, args: &[&str]) -> Command {
        let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.root.join("bin").display());
        let mut milo = Command::new(env!("CARGO_BIN_EXE_milo"));
        milo.args(args)
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", path)
            .env("TMPDIR", &self.root)
            .env("MILO_ALERTS_TEST_ENDPOINT", format!("http://127.0.0.1:{}", self.destination.port))
            .env("MILO_ALERTS_KEYCHAIN", self.keychain())
            .current_dir(&self.root)
            .env("MILO_RELAY_ORIGIN", "disabled")
            .stdin(Stdio::null());
        milo
    }

    /// Runs one command to its end, giving it `input`, and returns whether it
    /// succeeded and everything it printed.
    fn run_with(&self, args: &[&str], input: &str) -> (bool, String) {
        Self::capture(self.milo(args), input)
    }

    fn capture(mut command: Command, input: &str) -> (bool, String) {
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        let printed = String::from_utf8_lossy(&output.stdout).into_owned() + &String::from_utf8_lossy(&output.stderr);
        (output.status.success(), printed)
    }

    fn run(&self, args: &[&str]) -> String {
        let (ok, printed) = self.run_with(args, "");
        assert!(ok, "milo {args:?} failed: {printed}");
        printed
    }

    fn save_telegram(&self) {
        let (ok, printed) = self.run_with(&["alerts", "telegram", "-1001234567890"], &format!("{TOKEN}\n"));
        assert!(ok, "{printed}");
    }

    fn save_discord(&self) {
        let (ok, printed) = self.run_with(&["alerts", "discord"], &format!("{LINK}\n"));
        assert!(ok, "{printed}");
    }

    /// Puts a connection made with Connect with Milo in place of the saved
    /// bot token for `chat`, which only the browser editor can do.
    fn connect_telegram_through_a_lost_relay(&self, chat: &str) {
        let secret = format!("{LOST_CONNECTION}:{chat}");
        if cfg!(target_os = "macos") {
            let saved = Command::new("/usr/bin/security")
                .args(["add-generic-password", "-U", "-s", "net.usemilo.alerts", "-a", "telegram", "-w", &secret])
                .arg(self.keychain())
                .status()
                .unwrap();
            assert!(saved.success());
        } else {
            let file = self.root.join(".local/state/milo/alerts-secrets");
            let secrets = fs::read_to_string(&file).unwrap();
            fs::write(&file, secrets.replace(TOKEN, &secret)).unwrap();
        }
    }

    /// Has the stand-in `rex` print one terminal event.
    fn emit(&self, block: &str, name: &str, payload: &str) {
        let line = format!(
            "{{\"type\":\"session_event\",\"payload\":{{\"session_id\":\"session:01a1\",\"event\":{{\"type\":\
             \"block_event\",\"payload\":{{\"block_id\":\"block:{block}\",\"creator_name\":\
             \"com.superlogical.terminal\",\"name\":\"{name}\",\"payload\":{payload}}}}}}}}}\n"
        );
        fs::OpenOptions::new().append(true).open(self.root.join("stream")).unwrap().write_all(line.as_bytes()).unwrap();
    }

    fn status(&self, block: &str, state: &str, extra: &str, msg: &str) {
        let record = format!(
            "{{\"state\":\"{state}\"{extra},\"app\":\"synthetic\",\"effective_app\":\"synthetic\",\"msg\":\"{msg}\",\
             \"updated_at\":\"2026-11-21T07:46:38Z\"}}"
        );
        self.emit(
            block,
            "program_status_changed",
            &format!("{{\"id\":\"\",\"record\":{record},\"reason\":\"report\"}}"),
        );
    }

    fn blocked(&self, block: &str, msg: &str) {
        self.status(block, "blocked", ",\"kind\":\"permission\"", msg);
    }

    fn starts(&self) -> usize {
        fs::read_to_string(self.root.join("started")).map_or(0, |text| text.lines().count())
    }

    /// New subscriptions start after the events the destination already saw.
    fn checkpoint(&self) {
        fs::copy(self.root.join("stream"), self.root.join("shown")).unwrap();
    }

    fn rex_fault(&self, fault: &str, active: bool) {
        let file = self.root.join(fault);
        if active {
            fs::write(file, "").unwrap();
        } else {
            fs::remove_file(file).unwrap();
        }
    }

    fn login(&self) {
        let file = self.root.join(".config/milo/listener.lua");
        if !file.exists() {
            fs::write(file, "").unwrap();
        }
        self.run(&["listen"]);
    }

    // The kernel, not stale contents or a process name, identifies the owner
    // of a lock in this fixture's disposable state directory.
    fn lock_holder(&self, name: &str) -> Option<libc::pid_t> {
        let lock = match fs::File::open(self.root.join(".local/state/milo").join(name)) {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => panic!("could not open {name}: {error}"),
        };
        // SAFETY: A flock is numbers; zeros cover the whole file from the start.
        let mut owner: libc::flock = unsafe { std::mem::zeroed() };
        owner.l_type = libc::F_WRLCK as _;
        // SAFETY: The descriptor is open and F_GETLK fills in this writable flock.
        assert_eq!(unsafe { libc::fcntl(lock.as_raw_fd(), libc::F_GETLK, &raw mut owner) }, 0);
        (owner.l_type != libc::F_UNLCK as _ && owner.l_pid > 1).then_some(owner.l_pid)
    }

    fn worker(&self) -> Option<libc::pid_t> {
        self.lock_holder("alerts.lock")
    }

    fn supervisor(&self) -> Option<libc::pid_t> {
        self.lock_holder("alerts-supervisor.lock")
    }

    fn crash_worker(&self) -> libc::pid_t {
        self.crash_lock_holder("alerts.lock")
    }

    fn crash_lock_holder(&self, lock: &str) -> libc::pid_t {
        let pid = self.lock_holder(lock).unwrap_or_else(|| panic!("{lock} has no holder to crash"));
        // SAFETY: Only the positive PID owning a lock in this fixture is targeted.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        pid
    }

    /// Rex and its subprocesses hold this FIFO open until their process group
    /// exits. A nonblocking reader distinguishes that from merely losing Milo.
    fn rex_lease(&self) -> fs::File {
        let path = self.root.join("rex-lease");
        let made = Command::new("mkfifo").env_clear().env("PATH", "/usr/bin:/bin").arg(&path).status().unwrap();
        assert!(made.success());
        fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).unwrap()
    }

    fn health(&self, label: &str) -> bool {
        let (ok, status) = self.run_with(&["alerts"], "");
        let prefix = "Rex alert mirroring: ";
        ok && status.lines().next().and_then(|line| line.strip_prefix(prefix)) == Some(label)
    }

    fn wait_for_health(&self, label: &str) {
        assert!(soon(|| self.health(label)), "never reported {label}: {}", self.run(&["alerts"]));
    }

    fn assert_stays_paused(&self) {
        let starts = self.starts();
        for _ in 0..3 {
            self.login();
            assert_eq!((self.supervisor(), self.worker()), (None, None));
        }
        // The crash fixture reaches a four-second retry before pausing.
        thread::sleep(Duration::from_secs(5));
        assert_eq!(self.starts(), starts);
        assert!(self.health("Paused"));
        assert_eq!((self.supervisor(), self.worker()), (None, None));
    }

    fn received(&self, count: usize) -> bool {
        soon(|| self.destination.requests.lock().unwrap().len() >= count)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        for destination in ["telegram", "discord"] {
            let _ = self
                .milo(&["alerts", "notices", "off", destination])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.milo(&["alerts", "off"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        if cfg!(target_os = "macos") {
            let _ = Command::new("/usr/bin/security")
                .arg("delete-keychain")
                .arg(self.keychain())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Wait for something, with a generous limit for a busy test machine.
fn soon(mut happens: impl FnMut() -> bool) -> bool {
    let began = Instant::now();
    loop {
        if happens() {
            return true;
        }
        if began.elapsed() >= Duration::from_secs(30) {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Long enough for a message to arrive that should not.
fn settle() {
    thread::sleep(Duration::from_millis(2500));
}

#[test]
fn config_profiles_keep_distinct_credentials_in_the_same_keychain_or_state_root() {
    let Some(home) = Home::new() else { return };
    home.save_discord();
    home.run(&["alerts", "notices", "on", "discord"]);
    let mut explicit_default = home.milo(&["alerts"]);
    explicit_default
        .env("XDG_CONFIG_HOME", home.root.join(".config"))
        .env("XDG_STATE_HOME", home.root.join(".local/state"));
    let (ok, status) = Home::capture(explicit_default, "");
    assert!(ok && status.contains("Discord: saved; forwarding off"), "{status}");

    let mut another_xdg_config = home.milo(&["alerts"]);
    another_xdg_config.env("XDG_CONFIG_HOME", home.root.join("xdg-config"));
    let (ok, status) = Home::capture(another_xdg_config, "");
    assert!(ok && status.contains("Discord: not saved"), "{status}");

    let alternate_home = home.root.join("alternate-home");
    let alternate_config = alternate_home.join(".config/milo");
    fs::create_dir_all(&alternate_config).unwrap();
    fs::write(alternate_config.join("alerts.conf"), "enabled = false\ndestination = discord\n").unwrap();
    let alternate = |args: &[&str], input: &str| {
        let mut command = home.milo(args);
        command.env("HOME", &alternate_home).env("XDG_STATE_HOME", home.root.join(".local/state"));
        let (ok, printed) = Home::capture(command, input);
        assert!(ok, "{printed}");
        printed
    };
    let status = alternate(&["alerts"], "");
    assert!(status.contains("Discord: not saved") && status.contains("does not import legacy shared credentials"));
    let replacement = "https://discord.com/api/webhooks/1234567891/SYNTHETIC_synthetic-SYNTHETIC1\n";
    alternate(&["alerts", "discord"], replacement);
    alternate(&["alerts", "notices", "on", "discord"], "");
    alternate(&["alerts", "forget", "discord"], "");
    alternate(&["alerts", "discord"], replacement);
    let status = alternate(&["alerts"], "");
    assert!(status.contains("Discord: saved; forwarding off; lifecycle notices off"), "{status}");

    alternate(&["alerts", "discord"], &format!("{LINK}\n"));
    alternate(&["alerts", "forget", "discord"], "");
    alternate(&["alerts", "discord"], &format!("{LINK}\n"));
    let status = home.run(&["alerts"]);
    assert!(status.contains("Discord: saved; forwarding off; lifecycle notices on"), "{status}");
    home.run(&["alerts", "on", "discord"]);
    home.wait_for_health("Listening");
    assert!(home.received(1));
    home.blocked("original", "SYNTHETIC private to the first profile");
    assert!(home.received(2));
    let sent = home.destination.requests();
    let original = LINK.strip_prefix("https://discord.com/api/webhooks/").unwrap();
    let path = format!("/api/v10/webhooks/{original}");
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|request| request.path == path));
}

#[test]
fn mirroring_is_off_until_turned_on_and_ends_at_once_when_turned_off() {
    let Some(home) = Home::new() else { return };
    assert!(home.health("Off"));
    let (on, why) = home.run_with(&["alerts", "on"], "");
    assert!(!on && why.contains("Nothing was turned on"), "{why}");
    let (saved, why) = home.run_with(&["alerts", "telegram", "-1001234567890"], "not-a-token\n");
    assert!(!saved && why.contains("not a bot token"), "{why}");

    home.save_telegram();
    let settings = fs::read_to_string(home.root.join(".config/milo/alerts.conf")).unwrap();
    assert!(settings.contains("enabled = false") && !settings.contains("SYNTHETIC"), "{settings}");
    // Saving a destination sends nothing and listens to nothing.
    home.blocked("a", "SYNTHETIC before on");
    settle();
    assert_eq!((home.starts(), home.destination.requests().len()), (0, 0));

    assert!(home.run(&["alerts", "test"]).contains("accepted the test message"));
    let test = &home.destination.requests()[0];
    assert_eq!(test.path, format!("/bot{TOKEN}/sendMessage"));
    assert!(test.body.contains("\"chat_id\":-1001234567890") && text_of(&test.body).starts_with("Milo test:"));

    let on = home.run(&["alerts", "on"]);
    assert!(on.contains("Telegram chat -1001234567890") && on.contains("No text from the program"), "{on}");
    assert!(home.received(2));
    assert_eq!(home.destination.texts()[1], "Rex: <code>synthetic</code> is waiting for permission");
    assert!(home.destination.requests()[1].body.contains("\"parse_mode\":\"HTML\""));
    home.wait_for_health("Listening");
    let status = home.run(&["alerts"]);
    assert!(status.contains("test stand-in"), "{status}");
    assert!(!status.contains("SYNTHETIC"), "{status}");

    assert!(home.run(&["alerts", "off"]).contains("Nothing more is sent"));
    home.status("b", "error", "", "SYNTHETIC after off");
    settle();
    assert_eq!(home.destination.requests().len(), 2);
    assert!(home.health("Paused"));

    assert!(home.run(&["alerts", "forget", "telegram"]).contains("Removed"));
    let status = home.run(&["alerts"]);
    assert!(status.contains("Telegram: not saved") && status.contains("Destination: no destination"), "{status}");
    let (tested, why) = home.run_with(&["alerts", "test"], "");
    assert!(!tested, "{why}");
}

#[test]
fn an_alert_is_sent_once_however_often_rex_repeats_it_or_milo_is_started() {
    let Some(home) = Home::new() else { return };
    home.save_discord();
    home.run(&["alerts", "on"]);
    // Every tab and pane may try to start mirroring; only the first does.
    for _ in 0..3 {
        home.run(&["alerts", "supervise"]);
    }
    assert!(soon(|| home.starts() == 1));

    for _ in 0..3 {
        home.blocked("a", "SYNTHETIC approve?");
        home.emit("a", "bell", "{}");
    }
    home.status("a", "working", "", "SYNTHETIC working");
    home.status("a", "done", "", "SYNTHETIC done");
    assert!(home.received(2));
    settle();
    assert_eq!(home.destination.texts(), ["`synthetic` **is waiting for permission**", "`synthetic` **finished**"]);
    let request = &home.destination.requests()[0];
    assert_eq!(request.path, "/api/v10/webhooks/1234567890/SYNTHETIC_synthetic-SYNTHETIC0");
    assert!(request.body.contains("\"allowed_mentions\":{\"parse\":[]}"), "{}", request.body);
    assert_eq!(home.starts(), 1);
}

#[test]
fn mirroring_left_on_starts_again_with_rex_s_listener() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    // As after a restart of the machine: turned on, with nothing running.
    let config = home.root.join(".config/milo");
    let settings = fs::read_to_string(config.join("alerts.conf")).unwrap();
    fs::write(config.join("alerts.conf"), settings.replace("enabled = false", "enabled = true")).unwrap();
    assert!(home.health("Stopped"));

    // Rex starts the listener each time its server loads its settings.
    for _ in 0..3 {
        home.login();
    }
    home.wait_for_health("Listening");
    assert_eq!(home.starts(), 1);
    home.blocked("a", "SYNTHETIC approve?");
    assert!(home.received(1));
}

#[test]
fn mirroring_follows_rex_again_after_disconnect_with_notices_off_by_default() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    let status = home.run(&["alerts"]);
    assert!(status.contains("Telegram: saved; forwarding off; lifecycle notices off"), "{status}");
    assert!(status.contains("Discord: not saved; forwarding off; lifecycle notices off"), "{status}");
    home.run(&["alerts", "on"]);
    home.wait_for_health("Listening");
    home.blocked("a", "SYNTHETIC approve?");
    assert!(home.received(1));
    home.checkpoint();
    home.rex_fault("rex-down", true);
    home.wait_for_health("Waiting for Rex");
    assert!(home.run(&["alerts"]).contains("Telegram: saved; forwarding on"));
    home.rex_fault("rex-down", false);
    home.wait_for_health("Listening");
    assert_eq!(home.starts(), 2);
    // A fresh event for the same terminal should be delivered after reconnect.
    home.blocked("a", "SYNTHETIC approve?");
    home.blocked("a", "SYNTHETIC repeated approve?");
    assert!(home.received(2));
    settle();
    assert_eq!(home.destination.requests().len(), 2);
    home.run(&["alerts", "off"]);
    settle();
    assert_eq!(home.destination.texts(), ["Rex: <code>synthetic</code> is waiting for permission"; 2]);
}

#[test]
fn a_killed_worker_is_replaced_but_pausing_during_the_next_retry_stays_off() {
    let Some(home) = Home::new() else { return };
    home.save_discord();
    home.run(&["alerts", "notices", "on", "discord"]);
    home.run(&["alerts", "on"]);
    home.wait_for_health("Listening");
    let supervisor = home.supervisor().expect("listening without a supervisor");
    assert!(home.received(1));
    home.blocked("a", "SYNTHETIC before crash");
    assert!(home.received(2));
    home.checkpoint();

    let killed = home.crash_worker();
    home.wait_for_health("Retrying");
    assert_eq!(home.worker(), None);
    assert_eq!(home.supervisor(), Some(supervisor));
    home.wait_for_health("Listening");
    let replacement = home.worker().expect("listening without a worker");
    assert_ne!(replacement, killed);
    assert_eq!(home.supervisor(), Some(supervisor));
    assert_eq!(home.starts(), 2);
    assert!(home.received(3));
    home.status("b", "done", "", "SYNTHETIC after crash");
    assert!(home.received(4));
    assert_eq!(home.destination.texts_at(DISCORD)[2], RECOVERED);

    // Another crash before the connection settles is part of that same
    // interruption; the replacement must not emit another recovery notice.
    home.checkpoint();
    assert_eq!(home.crash_worker(), replacement);
    home.wait_for_health("Retrying");
    home.wait_for_health("Listening");
    assert_eq!(home.starts(), 3);
    home.status("c", "error", "", "SYNTHETIC after another replacement");
    assert!(home.received(5));
    home.checkpoint();
    assert_ne!(home.crash_worker(), replacement);
    home.wait_for_health("Retrying");
    home.run(&["alerts", "off"]);
    assert!(home.received(6));
    home.status("d", "error", "", "SYNTHETIC must not escape the pause");
    home.assert_stays_paused();
    assert_eq!(
        home.destination.texts_at(DISCORD),
        [
            ENABLED,
            "`synthetic` **is waiting for permission**",
            RECOVERED,
            "`synthetic` **finished**",
            "`synthetic` **reported an error**",
            PAUSED,
        ]
    );
}

#[test]
fn killing_the_supervisor_ends_the_worker_and_its_process_group_without_a_pause() {
    let Some(home) = Home::new() else { return };
    home.save_discord();
    let mut rex_lease = home.rex_lease();
    home.run(&["alerts", "on"]);
    home.wait_for_health("Listening");
    let mut opened = [0];
    assert!(soon(|| match rex_lease.read(&mut opened) {
        Ok(1) => true,
        Ok(0) => false,
        Err(error) if error.kind() == ErrorKind::WouldBlock => false,
        result => panic!("unexpected Rex lease data: {result:?}"),
    }));
    assert_eq!(opened, [b'x']);
    let reply = home.destination.hold_reply_at(DISCORD, 503, "{}");
    home.blocked("a", "SYNTHETIC no reply before the supervisor crash");
    let connection = reply.connection();
    let supervisor = home.supervisor().unwrap();
    let worker = home.worker().unwrap();
    assert_ne!(supervisor, worker);
    assert!(matches!(rex_lease.read(&mut [0]), Err(error) if error.kind() == ErrorKind::WouldBlock));

    assert_eq!(home.crash_lock_holder("alerts-supervisor.lock"), supervisor);
    connection.assert_cancelled();
    assert!(soon(|| home.worker().is_none() && home.supervisor().is_none()));
    assert!(soon(|| match rex_lease.read(&mut [0]) {
        Ok(0) => true,
        Err(error) if error.kind() == ErrorKind::WouldBlock => false,
        result => panic!("unexpected Rex lease data: {result:?}"),
    }));
    assert!(home.health("Stopped"), "the crash must keep enabled intent");
    assert!(home.run(&["alerts"]).contains("Discord: saved; forwarding on"));
    drop(reply);

    home.checkpoint();
    home.login();
    home.wait_for_health("Listening");
    assert_ne!(home.worker(), Some(worker));
    home.status("b", "done", "", "SYNTHETIC after login restarted forwarding");
    assert!(home.received(2));
    assert_eq!(home.destination.texts(), ["`synthetic` **is waiting for permission**", "`synthetic` **finished**"]);
}

#[test]
fn a_user_pause_while_rex_is_unreachable_survives_recovery_and_login() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.run(&["alerts", "on"]);
    home.wait_for_health("Listening");
    home.rex_fault("rex-down", true);
    home.wait_for_health("Waiting for Rex");
    assert!(home.run(&["alerts"]).contains("Telegram: saved; forwarding on"));

    home.run(&["alerts", "off"]);
    home.rex_fault("rex-down", false);
    home.status("a", "error", "", "SYNTHETIC while paused");
    home.assert_stays_paused();
    assert!(home.destination.requests().is_empty());

    home.checkpoint();
    home.run(&["alerts", "on"]);
    home.wait_for_health("Listening");
    home.status("b", "done", "", "SYNTHETIC after explicit resume");
    assert!(home.received(1));
    assert_eq!(home.destination.texts(), ["Rex: <code>synthetic</code> finished"]);
}

#[test]
fn lifecycle_notices_are_per_provider_and_only_report_meaningful_transitions() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.save_discord();
    home.run(&["alerts", "notices", "on", "telegram"]);
    let status = home.run(&["alerts"]);
    assert!(status.contains("Telegram: saved; forwarding off; lifecycle notices on"), "{status}");
    assert!(status.contains("Discord: saved; forwarding off; lifecycle notices off"), "{status}");
    assert!(home.destination.requests().is_empty());

    home.run(&["alerts", "on", "telegram"]);
    assert!(home.received(1));
    home.run(&["alerts", "on", "discord"]);
    home.run(&["alerts", "on", "telegram"]);
    for _ in 0..3 {
        home.login();
    }
    home.wait_for_health("Listening");
    home.blocked("a", "SYNTHETIC before interrupted stream");
    assert!(home.received(3));
    assert_eq!(home.destination.texts_at(TELEGRAM), [ENABLED, "Rex: <code>synthetic</code> is waiting for permission"]);
    assert_eq!(home.destination.texts_at(DISCORD), ["`synthetic` **is waiting for permission**"]);
    home.checkpoint();

    // Rex answers its reachability probe after returning but repeatedly drops
    // the event stream before it is usable. Failures must neither spin nor
    // produce recovery notices until a stream actually stays open.
    let initial = home.starts();
    home.rex_fault("rex-flaps", true);
    home.rex_fault("rex-down", true);
    home.wait_for_health("Waiting for Rex");
    home.rex_fault("rex-down", false);
    assert!(soon(|| home.starts() > initial));
    for (attempt, minimum) in [(2, 600), (3, 1200)] {
        let began = Instant::now();
        assert!(soon(|| home.starts() >= initial + attempt));
        assert!(began.elapsed() >= Duration::from_millis(minimum), "Rex subscription {attempt} retried too soon");
    }
    assert!(home.health("Waiting for Rex"));
    assert_eq!(home.starts(), initial + 3);
    assert_eq!(home.destination.requests().len(), 3);
    home.rex_fault("rex-flaps", false);
    home.wait_for_health("Listening");
    assert!(home.received(4));
    assert_eq!(home.starts(), initial + 4);
    assert_eq!(home.destination.texts_at(TELEGRAM)[2], RECOVERED);

    // An ordinary alert is still delivered to both, then pausing Telegram
    // leaves Discord forwarding and neither a repeated off nor login duplicates
    // the pause notice, resurrects Telegram, or causes another recovery notice.
    home.status("b", "done", "", "SYNTHETIC after recovered stream");
    assert!(home.received(6));
    home.checkpoint();
    home.run(&["alerts", "off", "telegram"]);
    home.run(&["alerts", "off", "telegram"]);
    home.login();
    home.wait_for_health("Listening");
    home.status("c", "error", "", "SYNTHETIC only Discord");
    assert!(home.received(8));
    settle();
    assert_eq!(
        home.destination.texts_at(TELEGRAM),
        [
            ENABLED,
            "Rex: <code>synthetic</code> is waiting for permission",
            RECOVERED,
            "Rex: <code>synthetic</code> finished",
            PAUSED,
        ]
    );
    assert_eq!(
        home.destination.texts_at(DISCORD),
        ["`synthetic` **is waiting for permission**", "`synthetic` **finished**", "`synthetic` **reported an error**",]
    );
    assert!(home.run(&["alerts"]).contains("Telegram: saved; forwarding off; lifecycle notices on"));
}

#[test]
fn pausing_a_provider_and_forgetting_the_other_during_rex_backoff_do_not_resurrect_either() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.save_discord();
    home.run(&["alerts", "notices", "on", "discord"]);
    home.run(&["alerts", "on", "telegram"]);
    home.run(&["alerts", "on", "discord"]);
    home.wait_for_health("Listening");
    assert!(home.received(1));
    home.rex_fault("rex-down", true);
    home.wait_for_health("Waiting for Rex");
    home.run(&["alerts", "off", "telegram"]);

    home.rex_fault("rex-down", false);
    home.wait_for_health("Listening");
    assert!(soon(|| home.destination.texts_at(DISCORD).iter().any(|text| text == RECOVERED)));
    home.status("a", "done", "", "SYNTHETIC only Discord");
    assert!(soon(|| home.destination.texts_at(DISCORD).iter().any(|text| text.contains("finished"))));
    assert_eq!(
        home.destination.texts_at(DISCORD),
        [ENABLED, RECOVERED, "`synthetic` **finished**"],
        "only Discord owed a recovery notice; pausing Telegram must not re-enable or pause Discord",
    );
    assert!(home.destination.requests_at(TELEGRAM).is_empty());
    home.checkpoint();
    home.rex_fault("rex-down", true);
    home.wait_for_health("Waiting for Rex");
    assert!(home.run(&["alerts", "forget", "discord"]).contains("Removed"));
    let previous = home.destination.texts();

    home.rex_fault("rex-down", false);
    home.status("b", "error", "", "SYNTHETIC after forget");
    home.assert_stays_paused();
    let status = home.run(&["alerts"]);
    assert!(status.contains("Telegram: saved; forwarding off"), "{status}");
    assert!(status.contains("Discord: not saved; forwarding off; lifecycle notices off"), "{status}");
    assert!(!home.run_with(&["alerts", "test", "discord"], "").0);
    assert_eq!(home.destination.texts(), previous);
    assert!(!previous.iter().any(|text| text == PAUSED));
}

#[test]
fn a_refused_test_stops_automatic_posts_until_that_destination_is_approved_again() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.save_discord();
    home.run(&["alerts", "on", "telegram"]);
    home.run(&["alerts", "on", "discord"]);
    home.wait_for_health("Listening");
    home.destination.answer_at(TELEGRAM, &[(401, "{}")]);
    assert!(!home.run_with(&["alerts", "test", "telegram"], "").0);
    home.blocked("a", "after the test refused the token");
    assert!(soon(|| home.destination.requests_at(DISCORD).len() == 1));
    home.checkpoint();
    home.run(&["alerts", "off", "discord"]);
    home.run(&["alerts", "on", "discord"]);
    home.login();
    home.wait_for_health("Listening");
    home.status("b", "done", "", "after changing only Discord");
    assert!(soon(|| home.destination.requests_at(DISCORD).len() == 2));
    settle();
    assert_eq!(home.destination.requests_at(TELEGRAM).len(), 1, "only the explicit test may use this token");
}

#[test]
fn pausing_one_provider_leaves_the_others_in_flight_and_queued_alerts_intact() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.save_discord();
    home.run(&["alerts", "on", "telegram"]);
    home.run(&["alerts", "on", "discord"]);
    home.wait_for_health("Listening");
    let worker = home.worker().unwrap();
    let streams = home.starts();
    let telegram_reply = home.destination.hold_reply_at(TELEGRAM, 200, "{}");
    let discord_reply = home.destination.hold_reply_at(DISCORD, 204, "");
    home.blocked("a", "SYNTHETIC first alert");
    let telegram = telegram_reply.connection();
    let discord = discord_reply.connection();
    home.status("b", "done", "", "SYNTHETIC queued alert");
    settle();

    home.run(&["alerts", "off", "telegram"]);
    telegram.assert_cancelled();
    discord.assert_pending();
    assert_eq!(home.worker(), Some(worker));
    assert_eq!(home.starts(), streams);
    drop(telegram_reply);
    home.run(&["alerts", "on", "telegram"]);
    discord.assert_pending();
    assert_eq!(home.worker(), Some(worker));
    assert_eq!(home.starts(), streams);
    drop(discord_reply);

    assert!(soon(|| home.destination.texts_at(DISCORD).len() == 2));
    home.status("c", "error", "", "SYNTHETIC after re-enabling");
    assert!(home.received(5));
    settle();
    assert_eq!(
        home.destination.texts_at(TELEGRAM),
        ["Rex: <code>synthetic</code> is waiting for permission", "Rex: <code>synthetic</code> reported an error"]
    );
    assert_eq!(
        home.destination.texts_at(DISCORD),
        ["`synthetic` **is waiting for permission**", "`synthetic` **finished**", "`synthetic` **reported an error**"]
    );
}

#[test]
fn pause_does_not_wait_for_an_in_flight_alert_or_for_the_optional_pause_notice() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.run(&["alerts", "notices", "on", "telegram"]);
    home.run(&["alerts", "on"]);
    assert!(home.received(1));
    let alert_reply = home.destination.hold_reply_at(TELEGRAM, 503, "{}");
    home.blocked("a", "SYNTHETIC with no reply");
    assert!(home.received(2));
    let alert_connection = alert_reply.connection();
    let pause_reply = home.destination.hold_reply_at(TELEGRAM, 204, "");
    home.status("b", "done", "", "SYNTHETIC queued before pause");

    let began = Instant::now();
    home.run(&["alerts", "off"]);
    assert!(began.elapsed() < Duration::from_secs(8), "pause waited on a blocked HTTP reply");
    assert_eq!((home.supervisor(), home.worker()), (None, None));
    alert_connection.assert_cancelled();
    drop(alert_reply);
    drop(pause_reply);
    home.status("c", "error", "", "SYNTHETIC after pause");
    home.assert_stays_paused();
    let texts = home.destination.texts_at(TELEGRAM);
    assert_eq!(texts[..2], [ENABLED, "Rex: <code>synthetic</code> is waiting for permission"]);
    // The final notice is best effort. No ordinary alert or retry may follow.
    assert!(texts.len() == 2 || texts[2..] == [PAUSED], "{texts:?}");
}

#[test]
fn opting_out_and_forgetting_cancel_in_flight_lifecycle_notices() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.save_discord();
    for provider in ["telegram", "discord"] {
        home.run(&["alerts", "notices", "on", provider]);
    }
    let telegram_reply = home.destination.hold_reply_at(TELEGRAM, 200, "{}");
    home.run(&["alerts", "on", "telegram"]);
    let telegram = telegram_reply.connection();
    let discord_reply = home.destination.hold_reply_at(DISCORD, 204, "");
    home.run(&["alerts", "on", "discord"]);
    let discord = discord_reply.connection();
    assert_eq!(home.destination.texts_at(TELEGRAM), [ENABLED]);
    assert_eq!(home.destination.texts_at(DISCORD), [ENABLED]);
    telegram.assert_pending();

    home.run(&["alerts", "notices", "off", "telegram"]);
    telegram.assert_cancelled();
    discord.assert_pending();
    home.run(&["alerts", "forget", "discord"]);
    discord.assert_cancelled();
    drop(telegram_reply);
    drop(discord_reply);

    // Cancelling a notice does not affect ordinary forwarding to a saved target.
    home.wait_for_health("Listening");
    home.status("a", "done", "", "SYNTHETIC after cancellation");
    assert!(home.received(3));
    home.run(&["alerts", "off"]);
    settle();
    assert_eq!(home.destination.texts_at(TELEGRAM), [ENABLED, "Rex: <code>synthetic</code> finished"]);
    assert_eq!(home.destination.texts_at(DISCORD), [ENABLED]);
}

#[test]
fn a_destination_that_limits_the_rate_or_fails_is_asked_again_and_the_alert_arrives_once() {
    let Some(home) = Home::new() else { return };
    home.save_discord();
    home.destination.answer(&[
        (429, "{\"message\":\"You are being rate limited.\",\"retry_after\":0.4,\"global\":false}"),
        (500, "{}"),
        (204, ""),
    ]);
    home.run(&["alerts", "on"]);
    home.blocked("a", "SYNTHETIC approve?");
    assert!(home.received(3));
    home.status("b", "error", "", "SYNTHETIC failed");
    assert!(home.received(4));
    settle();
    let texts = home.destination.texts();
    assert_eq!(texts[..3], ["`synthetic` **is waiting for permission**"; 3]);
    assert_eq!(texts[3..], ["`synthetic` **reported an error**"]);
    assert!(home.run(&["alerts"]).contains("The last alert was delivered."));
}

#[test]
fn an_alert_goes_to_both_destinations_and_one_retry_does_not_delay_the_other() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.save_discord();
    home.run(&["alerts", "on", "telegram"]);
    home.run(&["alerts", "on", "discord"]);
    home.blocked("a", "SYNTHETIC approve?");
    assert!(home.received(2));
    let telegram = home.destination.requests_at("/bot");
    let discord = home.destination.requests_at("/api/v10/webhooks/");
    assert_eq!(telegram.len(), 1);
    assert_eq!(discord.len(), 1);
    assert_eq!(text_of(&telegram[0].body), "Rex: <code>synthetic</code> is waiting for permission");
    assert_eq!(text_of(&discord[0].body), "`synthetic` **is waiting for permission**");

    // Discord must get both new alerts while the Telegram worker is still
    // waiting to retry the first one, without counting it as a Discord drop.
    home.destination.answer_at("/bot", &[(429, "{\"parameters\":{\"retry_after\":60}}")]);
    home.status("b", "error", "", "SYNTHETIC failed");
    assert!(soon(|| home.destination.requests_at("/bot").len() == 2));
    home.status("c", "done", "", "SYNTHETIC done");
    assert!(soon(|| home.destination.requests_at("/api/v10/webhooks/").len() == 3));
    let discord = home.destination.requests_at("/api/v10/webhooks/");
    assert_eq!(text_of(&discord[1].body), "`synthetic` **reported an error**");
    assert_eq!(text_of(&discord[2].body), "`synthetic` **finished**");
    assert_eq!(home.destination.requests_at("/bot").len(), 2);
    assert!(soon(|| {
        let status = home.run(&["alerts"]);
        status.contains("Telegram: Trying again: Telegram is limiting the rate")
            && status.contains("Discord: The last alert was delivered.")
    }));
}

#[test]
fn an_alert_a_failing_destination_never_takes_is_dropped_and_the_next_says_so() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.destination.answer(&[(503, "{}"), (503, "{}"), (503, "{}")]);
    home.run(&["alerts", "on"]);
    home.blocked("a", "SYNTHETIC approve?");
    assert!(home.received(3));
    home.status("b", "done", "", "SYNTHETIC done");
    assert!(home.received(4));
    assert_eq!(home.destination.texts()[3], "Rex: <code>synthetic</code> finished\n(1 earlier alert was not sent.)");
}

#[test]
fn a_destination_that_asks_for_a_long_quiet_is_left_alone_until_it_is_over() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    let flood = "{\"ok\":false,\"error_code\":429,\"description\":\"Too Many Requests: retry after 4\",\
                 \"parameters\":{\"retry_after\":4}}";
    home.destination.answer(&[(429, flood), (429, flood), (429, flood)]);
    home.run(&["alerts", "on"]);
    home.blocked("a", "SYNTHETIC approve?");
    // Asked, told to wait four seconds, asked again after them, told the same, and once more.
    let began = Instant::now();
    assert!(home.received(3));
    assert!(began.elapsed() >= Duration::from_secs(7), "{:?}", began.elapsed());
    // An alert raised inside the last four seconds is not sent at all.
    home.status("b", "error", "", "SYNTHETIC failed");
    settle();
    assert_eq!(home.destination.requests().len(), 3);
    std::thread::sleep(Duration::from_secs(3));
    home.status("c", "done", "", "SYNTHETIC done");
    assert!(home.received(4));
    assert_eq!(home.destination.texts()[3], "Rex: <code>synthetic</code> finished\n(2 earlier alerts were not sent.)");
}

#[test]
fn one_message_a_destination_turns_down_does_not_silence_the_next() {
    let Some(home) = Home::new() else { return };
    home.save_discord();
    home.destination.answer(&[(400, "{\"message\":\"Invalid Form Body\",\"code\":50035}")]);
    home.run(&["alerts", "on"]);
    home.blocked("a", "SYNTHETIC approve?");
    assert!(home.received(1));
    assert!(soon(|| home.run(&["alerts"]).contains("The last alert was not delivered: Discord refused the request")));
    home.status("b", "done", "", "SYNTHETIC done");
    assert!(home.received(2));
    assert_eq!(home.destination.texts()[1], "`synthetic` **finished**\n(1 earlier alert was not sent.)");
}

#[test]
fn a_destination_that_refuses_the_secret_is_not_asked_again() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.save_discord();
    home.destination.answer_at("/bot", &[(401, "{\"ok\":false,\"error_code\":401,\"description\":\"Unauthorized\"}")]);
    home.run(&["alerts", "on", "telegram"]);
    home.run(&["alerts", "on", "discord"]);
    home.blocked("a", "SYNTHETIC approve?");
    assert!(soon(|| home.destination.requests_at("/bot").len() == 1));
    assert!(soon(|| home.run(&["alerts"]).contains("Paused: Telegram refused the saved secret")));
    home.blocked("b", "SYNTHETIC approve?");
    home.status("c", "error", "", "SYNTHETIC failed");
    assert!(soon(|| home.destination.requests_at("/api/v10/webhooks/").len() == 3));
    settle();
    assert_eq!(home.destination.requests_at("/bot").len(), 1);
    // Saving the destination again starts afresh.
    home.checkpoint();
    home.save_telegram();
    home.status("d", "done", "", "SYNTHETIC done");
    assert!(soon(|| home.destination.requests_at("/bot").iter().any(|r| text_of(&r.body).contains("finished"))));
}

#[test]
fn a_program_s_text_is_sent_only_when_chosen_and_can_mention_nobody() {
    let Some(home) = Home::new() else { return };
    home.save_discord();
    home.run(&["alerts", "events", "blocked,notification"]);
    home.run(&["alerts", "on"]);
    home.blocked("a", "SYNTHETIC approve @everyone?");
    assert!(home.received(1));
    assert!(!home.destination.requests()[0].body.contains("SYNTHETIC"));
    home.checkpoint();

    let disclosed = home.run(&["alerts", "content", "text"]);
    assert!(disclosed.contains("up to 300 characters of the text the program gave Rex"), "{disclosed}");
    home.emit("b", "desktop_notification", "{\"title\":\"SYNTHETIC title\",\"body\":\"hello @here <@123>\"}");
    home.status("b", "done", "", "SYNTHETIC done");
    assert!(soon(|| home.destination.texts().iter().any(|text| text.contains("SYNTHETIC title"))));
    settle();
    let texts = home.destination.texts();
    let sent = texts.last().unwrap();
    assert_eq!(sent, "a program **sent a notification**\n`SYNTHETIC title: hello @\u{200b}here <@\u{200b}123>`");
    // "done" was not among the chosen kinds.
    assert!(!texts.iter().any(|text| text.contains("finished")), "{texts:?}");
}

#[test]
fn a_connection_no_relay_can_revoke_is_forgotten_only_on_this_machine_and_only_when_asked() {
    let Some(home) = Home::new() else { return };
    let (saved, why) = home.run_with(&["alerts", "telegram", "42"], &format!("{TOKEN}\n"));
    assert!(saved, "{why}");
    home.save_discord();
    home.run(&["alerts", "on", "discord"]);
    home.connect_telegram_through_a_lost_relay("42");

    let (forgotten, why) = home.run_with(&["alerts", "forget", "telegram"], "");
    assert!(!forgotten && why.contains("milo alerts forget telegram --local-only"), "{why}");
    assert!(home.run(&["alerts"]).contains("Telegram: saved"));
    let (misused, why) = home.run_with(&["alerts", "forget", "discord", "--local-only"], "");
    assert!(!misused && why.contains("unexpected arguments"), "{why}");

    let report = home.run(&["alerts", "forget", "telegram", "--local-only"]);
    assert!(report.contains("nothing proves that it is revoked") && report.contains("send /stop"), "{report}");
    let status = home.run(&["alerts"]);
    assert!(status.contains("Telegram: not saved; forwarding off"), "{status}");
    assert!(status.contains("Discord: saved; forwarding on"), "{status}");
    home.blocked("a", "SYNTHETIC approve?");
    assert!(home.received(1));
    assert!(home.destination.requests()[0].path.starts_with("/api/v10/webhooks/"));
}

#[test]
fn a_change_that_cannot_be_saved_leaves_mirroring_running() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    home.run(&["alerts", "on"]);
    home.wait_for_health("Listening");
    let release = home.destination.hold_reply_at(TELEGRAM, 429, "{\"parameters\":{\"retry_after\":1}}");
    home.blocked("a", "SYNTHETIC approve?");
    assert!(home.received(1));
    home.checkpoint();
    let worker = home.worker().unwrap();
    // The settings file may be kept read-only, as by a dotfile manager.
    let settings = home.root.join(".config/milo/alerts.conf");
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o400)).unwrap();
    let (saved, why) = home.run_with(&["alerts", "events", "blocked"], "");
    let (resaved, resave_error) = home.run_with(&["alerts", "telegram", "-1001234567890"], &format!("{TOKEN}\n"));
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!saved && why.contains("Could not save"), "{why}");
    assert!(!resaved && resave_error.contains("Could not save"), "{resave_error}");
    // The first alert is still in flight when the save is rejected.
    drop(release);

    assert!(home.health("Listening"));
    assert_eq!(home.worker(), Some(worker));
    home.status("b", "done", "", "SYNTHETIC done");
    assert!(home.received(2));
    assert_eq!(
        home.destination.texts()[1],
        "Rex: <code>synthetic</code> is waiting for permission",
        "the failed re-save lost the pending alert even though the mirror reports running",
    );
    assert!(home.received(3));
    assert_eq!(home.destination.texts()[2], "Rex: <code>synthetic</code> finished");
}

#[test]
fn pause_can_finish_during_a_test_and_forget_can_cancel_its_orphaned_transport() {
    let Some(home) = Home::new() else { return };
    home.save_discord();
    home.run(&["alerts", "on", "discord"]);
    let held = home.destination.hold_reply_at(DISCORD, 204, "");
    let mut client = home.milo(&["alerts", "test", "discord"]).stderr(Stdio::null()).spawn().unwrap();
    let socket = held.connection();
    home.run(&["alerts", "off"]);
    assert!(home.health("Paused"));
    assert!(client.try_wait().unwrap().is_none(), "pause waited for the test's network timeout");
    let transport = home.lock_holder("alerts-discord-test.lock").expect("curl should own the test lease");
    client.kill().unwrap();
    client.wait().unwrap();
    socket.assert_pending();
    assert_eq!(home.lock_holder("alerts-discord-test.lock"), Some(transport));
    home.run(&["alerts", "forget", "discord"]);
    socket.assert_cancelled();
}
