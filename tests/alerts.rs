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

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const TOKEN: &str = "123456:SYNTHETICsyntheticSYNTHETIC00";
const LINK: &str = "https://discord.com/api/webhooks/1234567890/SYNTHETIC_synthetic-SYNTHETIC0";
/// A connection in the form the development relay gives. The production
/// relay, which a build speaks to unless told otherwise, never issued it, so
/// Milo asks no relay about it: for these tests its relay is gone for good.
const LOST_CONNECTION: &str = "milorelay_SYNTHETICsyntheticSYNTHETICsyntheticSYNTHET";
/// Says each time it is started, then prints what the test writes, and stays.
/// With `ends-once` in place the stream ends after what is written so far, as
/// it does when the Rex server stops; the next one prints what comes after.
const REX: &str = "#!/bin/sh\n[ \"$1\" = ready ] && exit 0\necho \"$*\" >> \"$HOME/started\"\n\
                   if [ -f \"$HOME/ends-once\" ]; then\n  rm \"$HOME/ends-once\"\n  \
                   cat \"$HOME/stream\" > \"$HOME/shown\"\n  cat \"$HOME/shown\"\n  exit 0\nfi\n\
                   from=1\n[ -f \"$HOME/shown\" ] && from=$(($(wc -c < \"$HOME/shown\") + 1))\n\
                   exec tail -c \"+$from\" -f \"$HOME/stream\"\n";
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
    gate: Option<mpsc::Receiver<()>>,
}

/// Stands in for Telegram and Discord. It answers with the first queued
/// reply matching the request's path, or with success when none matches.
struct Destination {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
    answers: Arc<Mutex<VecDeque<Reply>>>,
}

impl Destination {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let answers = Arc::new(Mutex::new(VecDeque::<Reply>::new()));
        let (seen, queued) = (Arc::clone(&requests), Arc::clone(&answers));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Some(request) = read_request(&mut stream) else { continue };
                let reply = {
                    let mut queued = queued.lock().unwrap();
                    let index = queued.iter().position(|reply| request.path.starts_with(reply.path_prefix));
                    index.and_then(|index| queued.remove(index))
                };
                seen.lock().unwrap().push(request);
                if let Some(gate) = reply.as_ref().and_then(|reply| reply.gate.as_ref()) {
                    let _ = gate.recv_timeout(Duration::from_secs(30));
                }
                let (code, body) = reply.map_or((200, "{\"ok\":true}"), |reply| (reply.status, reply.body));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {code} Answer\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self { port, requests, answers }
    }

    fn answer(&self, answers: &[(u16, &'static str)]) {
        self.answer_at("", answers);
    }

    fn answer_at(&self, path_prefix: &'static str, answers: &[(u16, &'static str)]) {
        let replies = answers.iter().map(|&(status, body)| Reply { path_prefix, status, body, gate: None });
        self.answers.lock().unwrap().extend(replies);
    }

    /// Hold the next reply until the caller drops the returned gate.
    fn hold_reply(&self, status: u16, body: &'static str) -> mpsc::Sender<()> {
        let (release, gate) = mpsc::channel();
        self.answers.lock().unwrap().push_back(Reply { path_prefix: "", status, body, gate: Some(gate) });
        release
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    fn requests_at(&self, path_prefix: &str) -> Vec<Request> {
        self.requests.lock().unwrap().iter().filter(|request| request.path.starts_with(path_prefix)).cloned().collect()
    }

    /// The text of every message received, whichever destination it was for.
    fn texts(&self) -> Vec<String> {
        self.requests().iter().map(|request| text_of(&request.body)).collect()
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Request> {
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
        let name = format!("milo-alerts-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let root = std::env::temp_dir().canonicalize().unwrap().join(name);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
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
        let path = std::env::var("PATH").unwrap_or_default();
        let mut milo = Command::new(env!("CARGO_BIN_EXE_milo"));
        milo.args(args)
            .env("HOME", &self.root)
            .env("PATH", format!("{}:{path}", self.root.join("bin").display()))
            .env("MILO_ALERTS_TEST_ENDPOINT", format!("http://127.0.0.1:{}", self.destination.port))
            .env("MILO_ALERTS_KEYCHAIN", self.keychain())
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("MILO_RELAY_ORIGIN")
            .stdin(Stdio::null());
        milo
    }

    /// Runs one command to its end, giving it `input`, and returns whether it
    /// succeeded and everything it printed.
    fn run_with(&self, args: &[&str], input: &str) -> (bool, String) {
        let mut child =
            self.milo(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
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

    fn received(&self, count: usize) -> bool {
        soon(|| self.destination.requests().len() >= count)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.milo(&["alerts", "off"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
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
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Long enough for a message to arrive that should not.
fn settle() {
    std::thread::sleep(Duration::from_millis(2500));
}

#[test]
fn mirroring_is_off_until_turned_on_and_ends_at_once_when_turned_off() {
    let Some(home) = Home::new() else { return };
    assert!(home.run(&["alerts"]).contains("Rex alert mirroring: off"));
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
    let status = home.run(&["alerts"]);
    assert!(status.contains("Rex alert mirroring: on\n") && status.contains("test stand-in"), "{status}");
    assert!(!status.contains("SYNTHETIC"), "{status}");

    assert!(home.run(&["alerts", "off"]).contains("Nothing more is sent"));
    home.status("b", "error", "", "SYNTHETIC after off");
    settle();
    assert_eq!(home.destination.requests().len(), 2);
    assert!(home.run(&["alerts"]).contains("Rex alert mirroring: off"));

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
        assert!(home.milo(&["alerts", "run"]).status().unwrap().success());
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
    fs::write(config.join("listener.lua"), "").unwrap();
    assert!(home.run(&["alerts"]).contains("on, but not running"));

    // Rex starts the listener each time its server loads its settings.
    let mut listener = home.milo(&["listen"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    home.blocked("a", "SYNTHETIC approve?");
    let arrived = home.received(1);
    let _ = listener.kill();
    let _ = listener.wait();
    assert!(arrived);
    assert!(home.run(&["alerts"]).contains("Rex alert mirroring: on\n"));
}

#[test]
fn mirroring_follows_rex_again_after_its_stream_ends() {
    let Some(home) = Home::new() else { return };
    home.save_telegram();
    fs::write(home.root.join("ends-once"), "").unwrap();
    home.blocked("a", "SYNTHETIC approve?");
    home.run(&["alerts", "on"]);
    assert!(home.received(1));
    // The stream ended; Milo opens it again by itself.
    assert!(soon(|| home.starts() == 2));
    // The same terminal blocks again: nothing remembered from before stands in the way.
    home.blocked("a", "SYNTHETIC approve?");
    assert!(home.received(2));
    settle();
    assert_eq!(home.destination.texts(), ["Rex: <code>synthetic</code> is waiting for permission"; 2]);
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
    // A real stream never replays an old event to a replacement mirror.
    fs::write(home.root.join("ends-once"), "").unwrap();
    home.blocked("a", "SYNTHETIC approve?");
    let release = home.destination.hold_reply(429, "{\"parameters\":{\"retry_after\":1}}");
    home.run(&["alerts", "on"]);
    assert!(home.received(1));
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

    let status = home.run(&["alerts"]);
    assert!(status.contains("Rex alert mirroring: on\n"), "{status}");
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
