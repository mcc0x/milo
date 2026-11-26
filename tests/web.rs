//! Runs the real `milo web` beside a stand-in for `rex` that answers only
//! when the test lets it.
//!
//! The link is handed over while the editor is still asking Rex which tab it
//! is in, so that a browser can start meanwhile. Only a real process shows
//! what that leaves a browser with: nothing until the editor is whole, then
//! the whole state, and a clean end if it is stopped in between.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);
/// Long enough for a server that was going to answer to have done so.
const QUIET: Duration = Duration::from_millis(300);
static NEXT: AtomicU32 = AtomicU32::new(0);

/// It notes that the editor asked for its tab, then waits to be released.
const GATED_REX: &str = "#!/bin/sh\n/bin/cat >/dev/null\n\
    for arg do case \"$arg\" in\n\
    milo_operation=context)\n\
    : > \"$HOME/asked\"\n\
    tries=1000\n\
    until [ -f \"$HOME/release\" ]; do\n\
      [ \"$tries\" -gt 0 ] || exit 1\n\
      tries=$((tries - 1))\n\
      /bin/sleep 0.02\n\
    done\n\
    printf 'window:test\\nok - 120 40\\n'; exit;;\n\
    esac; done\nexit 1\n";

/// A home of this test's own and a `milo web` started in it from a Rex pane.
struct Editor {
    root: PathBuf,
    child: Child,
    link: String,
    address: String,
}

impl Editor {
    /// Returns once the link is printed, which must not wait for Rex.
    fn start(forwarded: bool) -> Self {
        let name = format!("milo-web-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let root = std::env::temp_dir().join(name);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/rex"), GATED_REX).unwrap();
        fs::set_permissions(root.join("bin/rex"), fs::Permissions::from_mode(0o700)).unwrap();
        let mut path = root.join("bin").into_os_string();
        path.push(":/usr/bin:/bin");
        let mut child = Command::new(env!("CARGO_BIN_EXE_milo"))
            .args(["web", "--port", "0"])
            .args(if forwarded { &["--forwarded-port", "80"][..] } else { &["--no-open"][..] })
            .env_clear()
            .current_dir(&root)
            .env("PATH", path)
            .env("HOME", &root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("REX_CONFIG", root.join("init.lua"))
            .env("TERM", "xterm-rex")
            .env("REX_SESSION", "session:test")
            .env("REX_BLOCK", "block:shell")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut link = String::new();
            let mut target = String::new();
            let _ = output.read_line(&mut link);
            if forwarded {
                let _ = output.read_line(&mut target);
            }
            let _ = send.send((link, target));
        });
        let (link, target) = receive.recv_timeout(WAIT).unwrap_or_default();
        let address = if forwarded {
            target.trim_end().strip_prefix("milo web: serving a forward on ").unwrap_or_default()
        } else {
            link.strip_prefix("http://").and_then(|url| url.split_once("/#")).map_or("", |(host, _)| host)
        };
        let editor = Self { root, child, link: link.trim_end().to_owned(), address: address.to_owned() };
        assert!(editor.link.starts_with("http://127.0.0.1"), "no link while Rex was still asked");
        assert!(editor.address.starts_with("127.0.0.1:"), "no serving port while Rex was still asked");
        editor
    }

    fn wait_until_rex_is_asked(&self) {
        let until = Instant::now() + WAIT;
        while !self.root.join("asked").exists() {
            assert!(Instant::now() < until, "the editor never asked Rex for its tab");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn release_rex(&self) {
        fs::write(self.root.join("release"), "").unwrap();
    }

    /// Asks for the editor's state as the page does.
    fn ask_for_state(&self) -> TcpStream {
        let (origin, token) = self.link.split_once("/#").unwrap();
        let host = origin.strip_prefix("http://").unwrap();
        let mut socket = TcpStream::connect(&self.address).unwrap();
        write!(socket, "GET /api/state HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n\r\n").unwrap();
        socket
    }

    fn stop(&mut self) -> ExitStatus {
        // SAFETY: The child has not been waited for, so its ID still names it.
        unsafe { libc::kill(self.child.id().cast_signed(), libc::SIGTERM) };
        let until = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < until, "milo web did not end");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.release_rex();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Whether the server has sent nothing yet and still holds the connection.
fn silent(socket: &mut TcpStream) -> bool {
    socket.set_read_timeout(Some(QUIET)).unwrap();
    matches!(socket.read(&mut [0]), Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
}

#[test]
fn the_link_comes_before_rex_answers_and_the_state_only_after() {
    let mut editor = Editor::start(false);
    editor.wait_until_rex_is_asked();
    let mut early = editor.ask_for_state();
    assert!(silent(&mut early), "a state was served before the editor knew its tab");

    editor.release_rex();
    early.set_read_timeout(Some(WAIT)).unwrap();
    let mut reply = String::new();
    early.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
    // The state is the one made with Rex's answer, not a placeholder from before it.
    assert!(reply.contains(r#""inside_rex":true"#) && reply.contains(r#""capture":true"#), "{reply}");
    assert!(editor.stop().success());
}

#[test]
fn stopping_while_rex_is_asked_ends_at_once_without_serving_a_state() {
    let mut editor = Editor::start(false);
    editor.wait_until_rex_is_asked();
    let mut early = editor.ask_for_state();
    assert!(silent(&mut early));
    // Rex is never released: the editor must not need its answer to end.
    assert!(editor.stop().success());
    assert!(!editor.root.join("release").exists());
    let mut reply = Vec::new();
    // A connection the server never took is reset or closed, with nothing in it.
    let _ = early.read_to_end(&mut reply);
    assert!(reply.is_empty(), "{}", String::from_utf8_lossy(&reply));
}

#[test]
fn a_forwarded_editor_announces_both_ends_before_its_state_is_ready() {
    let mut editor = Editor::start(true);
    assert!(editor.link.starts_with("http://127.0.0.1/#"), "the link should name the browser's end of the forward");
    editor.wait_until_rex_is_asked();
    let mut early = editor.ask_for_state();
    assert!(silent(&mut early), "a state was served before the forwarded editor knew its tab");

    editor.release_rex();
    early.set_read_timeout(Some(WAIT)).unwrap();
    let mut reply = String::new();
    early.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
    assert!(reply.contains(r#""forwarded":true"#), "{reply}");
    assert!(editor.stop().success());
}
