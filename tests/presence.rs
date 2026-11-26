//! Runs the real `milo presence` and its helper against a stand-in for the
//! Discord app and a stand-in for `rex`.
//!
//! Each test has a home of its own, and in it the only folder the helper
//! may look for Discord's socket in. The stand-in listens there and speaks
//! Discord's side of the protocol, so the frames checked here are the ones a
//! real Discord would receive. No test can reach the Discord app of whoever
//! runs it: every variable Discord's socket is found by names the test's folder.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Milo's own Discord application, which presence uses unless a person gives another.
const APPLICATION: &str = "1573574411418599586";
const READY: &str = r#"{"cmd":"DISPATCH","data":{"v":1,"config":{},"user":{"id":"1","username":"stand-in"}},"evt":"READY","nonce":null}"#;
/// Longer than the helper waits before looking for Discord again.
const RETRY: Duration = Duration::from_secs(20);
static NEXT: AtomicU32 = AtomicU32::new(0);

/// A `rex` that prints what the test writes to a file, as the helper's
/// script prints what Rex tells it, and that does nothing as a listener.
/// It keeps what the script was asked to look at, for the test to read.
const REX: &str = "#!/bin/sh\ncase \" $* \" in\n*\" -e \"*) echo $$ > \"$HOME/rex-pid\"; printf '%s' \"$6\" > \"$HOME/rex-asked\"; exec tail -n +1 -f \"$HOME/rex-says\" ;;\nesac\n";

struct Home {
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        // A socket's path must be short, so this is not under the system's temporary folder.
        let name = format!("/tmp/milo-pt-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let home = Self { root: PathBuf::from(name) };
        let _ = fs::remove_dir_all(&home.root);
        for folder in ["bin", "run", ".config/milo"] {
            fs::create_dir_all(home.root.join(folder)).unwrap();
        }
        fs::write(home.root.join(".config/milo/listener.lua"), "").unwrap();
        fs::write(home.root.join("rex-says"), "").unwrap();
        let rex = home.root.join("bin/rex");
        fs::write(&rex, REX).unwrap();
        fs::set_permissions(&rex, fs::Permissions::from_mode(0o700)).unwrap();
        // A test thread's fork can briefly inherit the file open for writing.
        assert!(soon(|| Command::new(&rex).status().is_ok_and(|status| status.success())));
        home
    }

    fn milo(&self, args: &[&str]) -> Output {
        let path = std::env::var("PATH").unwrap_or_default();
        let run = self.root.join("run");
        Command::new(env!("CARGO_BIN_EXE_milo"))
            .args(args)
            .env("HOME", &self.root)
            .env("PATH", format!("{}:{path}", self.root.join("bin").display()))
            .env("XDG_RUNTIME_DIR", &run)
            .env("TMPDIR", &run)
            .env("TMP", &run)
            .env("TEMP", &run)
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// Runs a command that must succeed and returns what it printed.
    fn say(&self, args: &[&str]) -> String {
        let output = self.milo(args);
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    fn enable(&self) -> String {
        self.say(&["presence", "enable", "--yes"])
    }

    /// What the helper's script was told it may ask Rex about.
    fn rex_asked(&self) -> String {
        fs::read_to_string(self.root.join("rex-asked")).unwrap_or_default()
    }

    /// Rex reports the session in front by its label and the folder its focused pane works in.
    fn rex_shows(&self, session: Option<&str>, directory: Option<&std::path::Path>) {
        let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let session = session.map_or("-".into(), |label| hex(label.as_bytes()));
        let directory = directory.map_or("-".into(), |dir| hex(dir.to_str().unwrap().as_bytes()));
        self.rex_says(&format!("active {session} {directory}"));
    }

    /// A Git project in a folder of that name, somewhere under a path that must never be shown.
    fn project(&self, name: &str) -> PathBuf {
        let dir = self.root.join("private-clients").join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        let git = Command::new("git").args(["init", "-q"]).arg(&dir).env("GIT_CONFIG_NOSYSTEM", "1").status().unwrap();
        assert!(git.success());
        let remote = ["remote", "add", "origin", "git@example.com:secret-owner/secret-repo.git"];
        assert!(Command::new("git").arg("-C").arg(&dir).args(remote).status().unwrap().success());
        dir
    }

    /// What Rex would report: `active` once an app has a session open, `idle` once none has.
    fn rex_says(&self, report: &str) {
        let mut file = fs::OpenOptions::new().append(true).open(self.root.join("rex-says")).unwrap();
        writeln!(file, "milo-rex {report}").unwrap();
    }

    fn rex_pid(&self) -> Option<i32> {
        fs::read_to_string(self.root.join("rex-pid")).ok()?.trim().parse().ok()
    }

    fn discord(&self) -> Discord {
        Discord::listen(self.root.join("run/discord-ipc-0"))
    }

    fn status_says(&self, expected: &str) -> bool {
        soon(|| self.say(&["presence", "status"]).contains(expected))
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.milo(&["presence", "disable"]);
        if let Some(pid) = self.rex_pid() {
            let _ = Command::new("kill").arg(pid.to_string()).stderr(Stdio::null()).status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn within(limit: Duration, mut happens: impl FnMut() -> bool) -> bool {
    let began = Instant::now();
    loop {
        if happens() {
            return true;
        }
        if began.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait with a generous limit for a busy test machine.
fn soon(happens: impl FnMut() -> bool) -> bool {
    within(Duration::from_secs(30), happens)
}

/// The stand-in for the Discord app: its socket, and the side of the protocol Discord speaks.
struct Discord {
    listener: UnixListener,
}

impl Discord {
    fn listen(path: PathBuf) -> Self {
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self { listener }
    }

    fn pending(&self) -> Option<Client> {
        match self.listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                Some(Client { stream })
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => None,
            Err(error) => panic!("{error}"),
        }
    }

    fn accept_within(&self, limit: Duration) -> Client {
        let mut client = None;
        assert!(
            within(limit, || {
                client = self.pending();
                client.is_some()
            }),
            "nothing connected to the stand-in for Discord"
        );
        client.unwrap()
    }

    /// Takes a connection through the handshake, as Discord does for an application it knows.
    fn welcome(&self, limit: Duration) -> Client {
        self.handshake(limit, APPLICATION, 1, READY)
    }

    fn handshake(&self, limit: Duration, app: &str, opcode: u32, reply: &str) -> Client {
        let mut client = self.accept_within(limit);
        assert_eq!(client.frame(), Some((0, format!(r#"{{"v":1,"client_id":"{app}"}}"#))));
        client.send(opcode, reply);
        client
    }

    // The CLI waits for the helper's status; the helper gives Discord only two
    // seconds to answer. Serve the handshake while the CLI is still running.
    fn handshake_during(
        &self,
        app: &str,
        opcode: u32,
        reply: &str,
        command: impl FnOnce() -> String,
    ) -> (String, Client) {
        std::thread::scope(|scope| {
            let peer = scope.spawn(|| self.handshake(Duration::from_secs(30), app, opcode, reply));
            (command(), peer.join().unwrap())
        })
    }

    fn welcome_during(&self, command: impl FnOnce() -> String) -> (String, Client) {
        self.handshake_during(APPLICATION, 1, READY, command)
    }

    /// Whether nothing connects for a moment.
    fn quiet(&self) -> bool {
        std::thread::sleep(Duration::from_millis(400));
        self.pending().is_none()
    }
}

struct Client {
    stream: UnixStream,
}

impl Client {
    /// The next frame as its opcode and JSON, or `None` once the connection has closed.
    fn frame(&mut self) -> Option<(u32, String)> {
        let mut header = [0; 8];
        match self.stream.read_exact(&mut header) {
            Ok(()) => {}
            // Linux resets a connection whose other end closed with something still unread, such as
            // this stand-in's last reply, where macOS only ends it.
            Err(error) if matches!(error.kind(), ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset) => return None,
            Err(error) => panic!("{error}"),
        }
        let opcode = u32::from_le_bytes(header[..4].try_into().unwrap());
        let mut payload = vec![0; u32::from_le_bytes(header[4..].try_into().unwrap()) as usize];
        self.stream.read_exact(&mut payload).unwrap();
        Some((opcode, String::from_utf8(payload).unwrap()))
    }

    fn send(&mut self, opcode: u32, payload: &str) {
        let mut frame = opcode.to_le_bytes().to_vec();
        frame.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
        frame.extend_from_slice(payload.as_bytes());
        self.stream.write_all(&frame).unwrap();
    }

    /// The activity that was set, checked to be the fixed line and nothing more. Returns the helper's process ID.
    fn activity(&mut self, request: u32) -> i32 {
        self.shows(request, None)
    }

    /// The activity that was set, checked to hold the fixed line, exactly
    /// these names as its second line, and nothing else.
    fn shows(&mut self, request: u32, names: Option<&str>) -> i32 {
        let (opcode, payload) = self.frame().expect("an activity");
        assert_eq!(opcode, 1);
        let pid: i32 = payload.split_once(r#""pid":"#).unwrap().1.split_once(',').unwrap().0.parse().unwrap();
        let state = names.map(|names| format!(r#","state":"{names}""#)).unwrap_or_default();
        assert_eq!(
            payload,
            format!(
                r#"{{"cmd":"SET_ACTIVITY","args":{{"pid":{pid},"activity":{{"details":"In a Rex session"{state}}}}},"nonce":"milo-{request}"}}"#
            )
        );
        self.send(1, &format!(r#"{{"cmd":"SET_ACTIVITY","data":{{}},"evt":null,"nonce":"milo-{request}"}}"#));
        pid
    }

    /// The activity is taken down by setting none, and then the connection closes.
    fn cleared(&mut self, pid: i32, request: u32) {
        let expected = format!(r#"{{"cmd":"SET_ACTIVITY","args":{{"pid":{pid}}},"nonce":"milo-{request}"}}"#);
        assert_eq!(self.frame(), Some((1, expected)));
        assert_eq!(self.frame(), None);
    }
}

#[test]
fn the_activity_follows_rex_from_the_first_app_to_the_last_and_goes_when_turned_off() {
    let home = Home::new();
    let discord = home.discord();
    assert!(home.say(&["presence"]).starts_with("Discord presence: off\n"));

    home.rex_says("idle");
    assert!(home.enable().starts_with("Discord presence is on."));
    assert!(discord.quiet(), "no Rex app has a session open, so Discord is not contacted");
    assert!(home.status_says("Nothing is shown until a Rex app opens a session."));

    home.rex_says("active");
    let mut first = discord.welcome(Duration::from_secs(30));
    let helper = first.activity(1);
    assert!(home.status_says("Discord is showing “In a Rex session”."));
    // Discord checks that the connection is alive, and is answered.
    first.send(3, r#"{"check":1}"#);
    assert_eq!(first.frame(), Some((4, r#"{"check":1}"#.into())));

    home.rex_says("idle");
    first.cleared(helper, 2);
    assert!(home.status_says("Nothing is shown until a Rex app opens a session."));

    home.rex_says("active");
    let mut second = discord.welcome(Duration::from_secs(30));
    assert_eq!(second.activity(1), helper, "the same helper connects again");

    let rex = home.rex_pid().unwrap();
    let asked = Instant::now();
    assert!(home.say(&["presence", "disable"]).starts_with("Discord presence is off."));
    second.cleared(helper, 2);
    assert!(asked.elapsed() < Duration::from_secs(5), "turning presence off clears it at once");
    assert!(soon(|| !alive(helper) && !alive(rex)), "the helper and its watch on Rex are gone");
    assert!(home.say(&["presence"]).starts_with(&format!(
        "Discord presence: off\nApplication: Milo's, {APPLICATION}\nDetails: generic, which shows the fixed line and no names\n"
    )));
    assert_eq!(home.rex_asked(), r#"{"session":false,"directory":false}"#, "Rex was asked about no session");
    home.rex_says("active");
    assert!(discord.quiet(), "nothing connects once presence is off");
}

#[test]
fn one_helper_at_a_time_sets_the_activity() {
    let home = Home::new();
    let discord = home.discord();
    home.rex_says("active");
    let (_, mut first) = discord.welcome_during(|| home.enable());
    let helper = first.activity(1);

    // Rex starts `milo listen` each time it loads its settings, and each asks for a helper.
    for _ in 0..3 {
        assert!(home.milo(&["listen"]).status.success());
    }
    assert!(home.milo(&["presence-helper", "rex"]).status.success());
    std::thread::sleep(Duration::from_secs(4));
    assert!(discord.pending().is_none(), "a second helper never connects while the first holds the lock");
    assert!(alive(helper));
    assert!(home.status_says("Discord is showing “In a Rex session”."));

    // Turning it on again replaces the helper: the old one clears before the new one shows.
    let (_, mut second) = discord.welcome_during(|| home.enable());
    first.cleared(helper, 2);
    assert_ne!(second.activity(1), helper);
    assert!(discord.quiet());
}

#[test]
fn a_discord_that_is_missing_or_restarts_is_waited_for() {
    let home = Home::new();
    home.rex_says("active");
    assert!(home.enable().contains("the Discord app is not running here"), "Discord is not running yet");
    assert!(home.status_says("the Discord app is not running here"));

    let discord = home.discord();
    let mut first = discord.welcome(RETRY);
    let helper = first.activity(1);
    assert!(home.status_says("Discord is showing"));

    // Discord quits: its connections close and its socket goes.
    drop(first);
    drop(discord);
    fs::remove_file(home.root.join("run/discord-ipc-0")).unwrap();
    assert!(home.status_says("the Discord app is not running here"));
    assert!(alive(helper), "losing Discord does not end the helper");

    let discord = home.discord();
    let mut second = discord.welcome(RETRY);
    assert_eq!(second.activity(1), helper);
}

#[test]
fn an_application_discord_does_not_know_is_not_asked_about_again() {
    let home = Home::new();
    let discord = home.discord();
    home.rex_says("active");
    let _peer =
        discord.handshake_during(APPLICATION, 2, r#"{"code":4000,"message":"Invalid Client ID"}"#, || home.enable());
    assert!(home.status_says("Discord does not know this application ID"));
    home.rex_says("idle");
    home.rex_says("active");
    assert!(discord.quiet(), "the refused application is not offered to Discord again");
    assert!(home.status_says("Discord does not know this application ID"));
}

#[test]
fn the_activity_and_the_helper_end_with_rex() {
    let home = Home::new();
    let discord = home.discord();
    home.rex_says("active");
    let (_, mut client) = discord.welcome_during(|| home.enable());
    let helper = client.activity(1);

    // The Rex server stops, which ends the script that watches it.
    let rex = home.rex_pid().unwrap();
    assert!(Command::new("kill").arg(rex.to_string()).status().unwrap().success());
    client.cleared(helper, 2);
    assert!(soon(|| !alive(helper)));
    assert!(home.status_says("Milo's presence helper is not running"));

    // The next Rex starts `milo listen`, and presence comes back with it.
    let (_, mut again) = discord.welcome_during(|| home.say(&["listen"]));
    assert_ne!(again.activity(1), helper);
}

#[test]
fn while_presence_is_off_nothing_connects_to_discord_or_stays_running() {
    let home = Home::new();
    let discord = home.discord();
    home.rex_says("active");

    let refused = home.milo(&["presence", "enable"]);
    assert!(!refused.status.success(), "with nobody to ask, presence stays off");
    let said = String::from_utf8(refused.stderr).unwrap();
    assert!(said.contains("Anyone who can see your Discord activity sees it") && said.contains("--yes"), "{said}");
    assert!(home.say(&["presence"]).starts_with("Discord presence: off\nApplication: Milo's, "));
    assert!(!home.root.join(".config/milo/discord.conf").exists());

    assert!(home.milo(&["listen"]).status.success());
    assert!(home.milo(&["presence-helper", "rex"]).status.success());
    assert!(home.say(&["presence", "disable"]).starts_with("Discord presence is off."));
    std::thread::sleep(Duration::from_secs(1));
    assert!(discord.pending().is_none(), "nothing connected to Discord");
    assert_eq!(home.rex_pid(), None, "nothing started watching Rex");
    assert!(!home.root.join(".local/state/milo/presence.lock").exists(), "no helper ever took its lock");
}

#[test]
fn chosen_names_follow_what_is_in_front_and_go_when_they_are_no_longer_chosen() {
    let home = Home::new();
    let discord = home.discord();
    let (site, tool) = (home.project("client-site"), home.project("internal-tool"));
    home.rex_shows(Some("release notes"), Some(&site.join("src")));

    // With nobody to ask, names are not turned on, and the refusal says what they would show.
    let refused = home.milo(&["presence", "enable", "--details", "both"]);
    assert!(!refused.status.success());
    let said = String::from_utf8(refused.stderr).unwrap();
    assert!(said.contains("as in “Project: milo · Session: release-notes”") && said.contains("--yes"), "{said}");
    assert!(!home.root.join(".config/milo/discord.conf").exists());

    let (enabled, mut client) =
        discord.welcome_during(|| home.say(&["presence", "enable", "--details", "both", "--yes"]));
    assert!(enabled.starts_with("Discord presence is on."));
    assert_eq!(home.rex_asked(), r#"{"session":true,"directory":true}"#);
    let helper = client.shows(1, Some("Project: client-site · Session: release notes"));
    assert!(
        home.status_says("Discord is showing “In a Rex session” with “Project: client-site · Session: release notes”.")
    );

    // Another pane of the same project and session changes nothing, so Discord is told nothing.
    home.rex_shows(Some("release notes"), Some(&site));
    // Another session and project in front replace both names on the same connection.
    home.rex_shows(Some("fix-login"), Some(&tool));
    assert_eq!(client.shows(2, Some("Project: internal-tool · Session: fix-login")), helper);
    // Outside a Git project only the session is named, never the folder.
    home.rex_shows(Some("fix-login"), Some(&home.root.join("private-clients")));
    client.shows(3, Some("Session: fix-login"));
    // A label that could be a path or an address is left out whole.
    home.rex_shows(Some("~/private-clients/client-site"), Some(&tool));
    client.shows(4, Some("Project: internal-tool"));
    // When Rex cannot say which session is in front, no name of any session is shown.
    home.rex_shows(None, None);
    client.shows(5, None);
    assert!(home.status_says("with no name"));
    home.rex_shows(Some("release notes"), Some(&site));
    client.shows(6, Some("Project: client-site · Session: release notes"));

    // Discord restarts: the names in front now are shown again, once.
    drop(client);
    drop(discord);
    fs::remove_file(home.root.join("run/discord-ipc-0")).unwrap();
    let discord = home.discord();
    let mut client = discord.welcome(RETRY);
    assert_eq!(client.shows(1, Some("Project: client-site · Session: release notes")), helper);

    // Showing one name less takes the other down at once: the old activity is cleared before the new one is set.
    let (details, mut next) = discord.welcome_during(|| home.say(&["presence", "details", "project"]));
    assert!(details.contains("the folder name of the Git project in front"));
    client.cleared(helper, 2);
    assert_eq!(home.rex_asked(), r#"{"session":false,"directory":true}"#, "the session's label is no longer asked for");
    let project_only = next.shows(1, Some("Project: client-site"));
    assert_ne!(project_only, helper);

    // Back to no names: Rex is asked about no session, and none is shown even if one were reported.
    let (details, mut client) = discord.welcome_during(|| home.say(&["presence", "details", "generic"]));
    assert!(details.contains("the fixed line and no names"));
    next.cleared(project_only, 2);
    assert_eq!(home.rex_asked(), r#"{"session":false,"directory":false}"#);
    let generic = client.activity(1);
    home.rex_shows(Some("fix-login"), Some(&tool));
    assert!(discord.quiet());

    // Raising the details again needs a yes, and without one the activity stays as it is.
    let refused = home.milo(&["presence", "details", "session"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8(refused.stderr).unwrap().contains("The details were left as they were"));
    assert!(alive(generic));
    assert!(home.say(&["presence", "disable"]).starts_with("Discord presence is off."));
    client.cleared(generic, 2);
}

#[test]
fn an_application_of_your_own_is_kept_until_you_go_back_to_milos() {
    const OWN: &str = "1234567890123456789";
    let home = Home::new();
    let discord = home.discord();
    home.rex_says("active");
    // A choice saved by the release before Milo had an application of its own.
    fs::write(home.root.join(".config/milo/discord.conf"), format!("presence = false\napplication_id = {OWN}\n"))
        .unwrap();
    assert!(
        home.say(&["presence"])
            .starts_with(&format!("Discord presence: off\nApplication: your own, {OWN}\nDetails: generic"))
    );

    let (_, mut client) = discord.handshake_during(OWN, 1, READY, || home.enable());
    let helper = client.activity(1);

    let (changed, mut official) = discord.welcome_during(|| home.say(&["presence", "application", "official"]));
    assert!(changed.starts_with("Discord presence uses Milo's application."));
    client.cleared(helper, 2);
    let helper = official.activity(1);
    let saved = || fs::read_to_string(home.root.join(".config/milo/discord.conf")).unwrap();
    assert_eq!(saved(), "presence = true\n");

    // Milo's own ID is Milo's application however it is given, when turning presence on too.
    let (enabled, mut next) = discord.welcome_during(|| home.say(&["presence", "enable", APPLICATION, "--yes"]));
    assert!(enabled.starts_with("Discord presence is on."));
    official.cleared(helper, 2);
    next.activity(1);
    assert_eq!(saved(), "presence = true\n");
    assert!(home.say(&["presence"]).contains("\nApplication: Milo's, "));
}
