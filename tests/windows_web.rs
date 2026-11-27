//! Runs the packaged `milo web` on native Windows: the link it prints, a save made the way the
//! page makes one, and the same setting read back by a second server started afterwards.

#![cfg(windows)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

/// A home of the test's own, with Windows' settings folders inside it.
fn home(test: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("milo-windows-web-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for dir in ["roaming", "local", "work"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    root
}

fn milo(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_milo"));
    command
        .args(args)
        .current_dir(root.join("work"))
        .env("APPDATA", root.join("roaming"))
        .env("LOCALAPPDATA", root.join("local"))
        .env("USERPROFILE", root)
        .stdin(Stdio::null());
    for name in
        ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "REX_SESSION", "REX_BLOCK", "REX_CONFIG", "CLAUDE_CONFIG_DIR"]
    {
        command.env_remove(name);
    }
    command
}

struct Server {
    child: Child,
    address: String,
    bearer: String,
}

impl Server {
    fn start(root: &Path) -> Self {
        let mut child = milo(root, &["web", "--no-open", "--port", "0"]).stdout(Stdio::piped()).spawn().unwrap();
        let mut link = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut link).unwrap();
        let (origin, token) = link.trim_end().split_once("/#").expect("milo web prints its private link first");
        let address = origin.strip_prefix("http://127.0.0.1:").map(|port| format!("127.0.0.1:{port}")).unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        Self { child, address, bearer: format!("Bearer {token}") }
    }

    fn exchange(&self, method: &str, path: &str, body: &str) -> String {
        let mut socket = TcpStream::connect(&self.address).unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        let address = &self.address;
        write!(
            socket,
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            self.bearer,
            body.len()
        )
        .unwrap();
        let mut reply = String::new();
        socket.read_to_string(&mut reply).unwrap();
        reply
    }

    /// Done, as the page sends it; the server ends by itself.
    fn close(mut self, version: u64) {
        let reply = self.exchange("POST", "/api/close", &format!(r#"{{"version":{version}}}"#));
        assert!(reply.starts_with("HTTP/1.1 204 No Content\r\n"), "{reply}");
        let until = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                return;
            }
            assert!(Instant::now() < until, "milo web did not end after Done");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_setting_saved_in_the_browser_is_there_when_milo_web_starts_again() {
    let root = home("restart");
    let first = Server::start(&root);
    let state = first.exchange("GET", "/api/state", "");
    assert!(state.starts_with("HTTP/1.1 200 OK\r\n"), "{state}");
    assert!(state.contains(r#""appearance":{"icons":"plain""#), "{state}");
    assert!(state.contains(r#""alerts":null"#), "alerts are shown as unavailable: {state}");
    let unauthorized = TcpStream::connect(&first.address)
        .and_then(|mut socket| {
            write!(socket, "GET /api/state HTTP/1.1\r\nHost: {}\r\n\r\n", first.address)?;
            let mut reply = String::new();
            socket.read_to_string(&mut reply).map(|_| reply)
        })
        .unwrap();
    assert!(!unauthorized.starts_with("HTTP/1.1 200"), "{unauthorized}");

    let set = r#"{"version":1,"action":"set","field":"appearance.icons","value":"off"}"#;
    assert!(first.exchange("POST", "/api/action", set).starts_with("HTTP/1.1 200 OK\r\n"));
    let preferences = root.join("roaming").join("milo").join("preferences.conf");
    assert!(!preferences.exists(), "a draft writes nothing");
    let save = r#"{"version":2,"action":"save","group":"preferences","scope":"global"}"#;
    let saved = first.exchange("POST", "/api/action", save);
    assert!(saved.starts_with("HTTP/1.1 200 OK\r\n") && saved.contains(r#""dirty":false"#), "{saved}");
    assert_eq!(fs::read_to_string(&preferences).unwrap(), "icons = off\n");
    first.close(3);

    let second = Server::start(&root);
    let state = second.exchange("GET", "/api/state", "");
    assert!(state.contains(r#""appearance":{"icons":"off""#), "the save was lost on restart: {state}");
    second.close(1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn other_hosts_and_forwards_are_not_supported_on_windows_yet() {
    let root = home("unsupported");
    for (args, message) in [
        (&["web", "--host", "example"][..], "milo web --host is not supported on Windows yet."),
        (&["web", "--forwarded-port", "80"][..], "milo web --forwarded-port is not supported on Windows yet."),
    ] {
        let output = milo(&root, args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "no link is printed for {args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains(message), "{output:?}");
    }
    let _ = fs::remove_dir_all(&root);
}
