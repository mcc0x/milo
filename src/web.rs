//! Serves the settings editor on an ephemeral loopback address for this invocation only.

#[cfg(feature = "web-dev")]
mod dev;
mod http;

use std::fs::File;
use std::io::{self, PipeReader, Read, Write};
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use self::http::{Access, Input, Request, Response, Route};
use crate::setup::web::Editor;

#[cfg(not(feature = "web-dev"))]
fn asset(name: Asset) -> Response {
    let bytes: &[u8] = match name {
        Asset::Index => include_bytes!("web/index.html"),
        Asset::Script => include_bytes!("web/app.js"),
        Asset::Style => include_bytes!("web/app.css"),
        Asset::Mascot => include_bytes!("web/milo.webp"),
    };
    Response::asset(name.mime(), bytes)
}

#[cfg(feature = "web-dev")]
use dev::asset;

#[derive(Clone, Copy)]
enum Asset {
    Index,
    Script,
    Style,
    Mascot,
}

impl Asset {
    fn mime(self) -> &'static str {
        match self {
            Self::Index => "text/html; charset=utf-8",
            Self::Script => "text/javascript; charset=utf-8",
            Self::Style => "text/css; charset=utf-8",
            Self::Mascot => "image/webp",
        }
    }
}

pub(crate) const DEFAULT_PORT: u16 = 6110;
const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CLIENTS: usize = 16;
const LAUNCHER_CHECK: Duration = Duration::from_millis(100);
static STOP: AtomicBool = AtomicBool::new(false);
static WAKE_WRITER: AtomicI32 = AtomicI32::new(-1);

pub(crate) fn run(open_browser: bool, port: u16) -> Result<(), String> {
    STOP.store(false, Ordering::Relaxed);
    let wake = WakePipe::open().map_err(|error| format!("could not create a termination pipe: {error}"))?;
    let install =
        |signal| Signal::install(signal).map_err(|error| format!("could not handle termination signals: {error}"));
    let _signals = (install(libc::SIGINT)?, install(libc::SIGTERM)?, install(libc::SIGHUP)?, install(libc::SIGQUIT)?);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|error| format!("could not listen on 127.0.0.1:{port}: {error}; use --port 0 for a free port"))?;
    listener.set_nonblocking(true).map_err(|error| format!("could not configure the listener: {error}"))?;
    let host =
        listener.local_addr().map_err(|error| format!("could not read the loopback address: {error}"))?.to_string();
    let origin = format!("http://{host}");
    let token = random_token().map_err(|error| format!("could not generate a session token: {error}"))?;
    let bearer = format!("Bearer {token}");
    let url = format!("{origin}/#{token}");
    let access = Access { host: &host, origin: &origin, bearer: &bearer };
    let mut editor = Editor::open();
    if STOP.load(Ordering::Relaxed) {
        return Ok(());
    }
    {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{url}")
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("could not print the editor URL: {error}"))?;
    }
    let launcher = if open_browser {
        match launch(&url) {
            Ok(child) => Some(child),
            Err(error) => {
                eprintln!("milo web: could not open the browser ({error}); open the URL above");
                None
            }
        }
    } else {
        None
    };
    serve(&listener, &wake.0, &access, &mut editor, launcher).map_err(|error| format!("settings server: {error}"))
}

fn random_token() -> io::Result<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        token.push(char::from(HEX[usize::from(byte >> 4)]));
        token.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(token)
}

fn launch(url: &str) -> io::Result<Child> {
    #[cfg(target_os = "macos")]
    let program = "/usr/bin/open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    Command::new(program).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
}

fn check_launcher(launcher: &mut Option<Child>) {
    let Some(child) = launcher else { return };
    match child.try_wait() {
        Ok(None) => return,
        Ok(Some(status)) if status.success() => {}
        Ok(Some(status)) => eprintln!("milo web: could not open the browser ({status}); open the URL above"),
        Err(error) => eprintln!("milo web: could not check the browser launcher ({error}); open the URL above"),
    }
    *launcher = None;
}

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
    close_wake_writer();
}

struct WakePipe(PipeReader);

impl WakePipe {
    fn open() -> io::Result<Self> {
        let (reader, writer) = io::pipe()?;
        WAKE_WRITER.store(writer.into_raw_fd(), Ordering::Relaxed);
        Ok(Self(reader))
    }
}

impl Drop for WakePipe {
    fn drop(&mut self) {
        close_wake_writer();
    }
}

fn close_wake_writer() {
    // Whichever path takes the writer owns it. Closing it wakes poll with POLLHUP,
    // even for a signal delivered on a different thread or just before poll starts.
    let fd = WAKE_WRITER.swap(-1, Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: The atomic holds the sole write end of the pipe, and close is async-signal-safe.
        unsafe { libc::close(fd) };
    }
}

struct Signal(libc::c_int, libc::sigaction);

impl Signal {
    fn install(number: libc::c_int) -> io::Result<Self> {
        // SAFETY: Zeroed flags and a mask initialized by sigemptyset are valid for sigaction.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = stop as *const () as usize;
        // SAFETY: sa_mask is writable storage for the empty signal set.
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        let mut previous = MaybeUninit::uninit();
        // SAFETY: action is set up for the real handler; previous points to writable storage.
        if unsafe { libc::sigaction(number, &action, previous.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: sigaction succeeded and wrote the previous action into the provided storage.
        Ok(Self(number, unsafe { previous.assume_init() }))
    }
}

impl Drop for Signal {
    fn drop(&mut self) {
        // SAFETY: The saved action came from sigaction for the same signal number.
        unsafe { libc::sigaction(self.0, &self.1, std::ptr::null_mut()) };
    }
}

fn serve(
    listener: &TcpListener,
    wake: &PipeReader,
    access: &Access<'_>,
    editor: &mut Editor,
    mut launcher: Option<Child>,
) -> io::Result<()> {
    let mut clients: Vec<Client> = Vec::with_capacity(MAX_CLIENTS);
    let mut fds = [libc::pollfd { fd: -1, events: libc::POLLIN, revents: 0 }; MAX_CLIENTS + 2];
    fds[1].fd = wake.as_raw_fd();
    let mut last_activity = Instant::now();
    let mut closing = false;
    while !STOP.load(Ordering::Relaxed) && last_activity.elapsed() < IDLE_TIMEOUT {
        check_launcher(&mut launcher);
        let now = Instant::now();
        clients.retain(|client| client.deadline > now);
        if closing && clients.is_empty() {
            break;
        }
        let mut deadline = last_activity + IDLE_TIMEOUT;
        if launcher.is_some() {
            deadline = deadline.min(now + LAUNCHER_CHECK);
        }
        fds[0].fd = if closing || clients.len() == MAX_CLIENTS { -1 } else { listener.as_raw_fd() };
        for (fd, client) in fds[2..].iter_mut().zip(&clients) {
            deadline = deadline.min(client.deadline);
            *fd = libc::pollfd { fd: client.socket.as_raw_fd(), events: client.interest(), revents: 0 };
        }
        let timeout = crate::term::milliseconds(deadline.saturating_duration_since(Instant::now()));
        let ready = crate::term::wait_ready(&mut fds[..clients.len() + 2], timeout);
        if STOP.load(Ordering::Relaxed) || fds[1].revents != 0 {
            break;
        }
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        // Removing a client by swapping with the tail cannot invalidate a readiness entry
        // we still need; that client has already been visited.
        for index in (0..clients.len()).rev() {
            if STOP.load(Ordering::Relaxed) {
                break;
            }
            if fds[index + 2].revents == 0 {
                continue;
            }
            if clients[index].deadline <= Instant::now() {
                clients.swap_remove(index);
                continue;
            }
            match clients[index].step(access, editor) {
                Ok(Progress::Wait) => {}
                Ok(Progress::Reply { authenticated, close }) => {
                    if authenticated {
                        last_activity = Instant::now();
                    }
                    if close {
                        // Only flush the close response. No queued request may change the draft after this.
                        let client = clients.swap_remove(index);
                        clients.clear();
                        clients.push(client);
                        closing = true;
                        break;
                    }
                }
                Ok(Progress::Done) | Err(_) => {
                    clients.swap_remove(index);
                }
            }
        }
        if !closing && fds[0].revents != 0 {
            match listener.accept() {
                Ok((socket, peer)) if peer.ip().is_loopback() => clients.push(Client::new(socket)?),
                Ok(_) => {}
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

struct Client {
    socket: TcpStream,
    exchange: Exchange,
    deadline: Instant,
}

enum Exchange {
    Reading(Request),
    Writing(Response),
}

enum Progress {
    Wait,
    Reply { authenticated: bool, close: bool },
    Done,
}

impl Client {
    fn new(socket: TcpStream) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        Ok(Self { socket, exchange: Exchange::Reading(Request::default()), deadline: Instant::now() + IO_TIMEOUT })
    }

    fn interest(&self) -> libc::c_short {
        match self.exchange {
            Exchange::Reading(_) => libc::POLLIN,
            Exchange::Writing(_) => libc::POLLOUT,
        }
    }

    fn step(&mut self, access: &Access<'_>, editor: &mut Editor) -> io::Result<Progress> {
        let (response, authenticated, close) = match &mut self.exchange {
            Exchange::Reading(input) => match input.read_from(&mut self.socket, access)? {
                Input::Wait => return Ok(Progress::Wait),
                Input::Disconnected => return Ok(Progress::Done),
                Input::Rejected(rejection) => (rejection.response(), false, false),
                Input::Ready(route, body) => {
                    let authenticated = route.authenticated();
                    let (response, close) = respond(route, body, editor);
                    (response, authenticated, close)
                }
            },
            Exchange::Writing(output) => {
                return Ok(if output.write_to(&mut self.socket)? { Progress::Done } else { Progress::Wait });
            }
        };
        self.exchange = Exchange::Writing(response);
        self.deadline = Instant::now() + IO_TIMEOUT;
        Ok(Progress::Reply { authenticated, close })
    }
}

fn respond(route: Route, body: &[u8], editor: &mut Editor) -> (Response, bool) {
    let response = match route {
        Route::Index => asset(Asset::Index),
        Route::Script => asset(Asset::Script),
        Route::Style => asset(Asset::Style),
        Route::Mascot => asset(Asset::Mascot),
        Route::Favicon => Response::asset("image/png", include_bytes!("web/favicon.png")),
        Route::RexIcon => Response::asset("image/png", include_bytes!("web/rex.png")),
        #[cfg(feature = "web-dev")]
        Route::DevScript => Response::asset("text/javascript; charset=utf-8", include_bytes!("web/dev.js")),
        #[cfg(feature = "web-dev")]
        Route::DevVersion => dev::version(),
        Route::State => Response::json(editor.state()),
        Route::Action => match editor.action(body) {
            Ok(state) => Response::json(state),
            Err(error) => Response::bad_request(&error),
        },
        Route::Close => {
            use crate::setup::web::CloseError;

            let Some(version) = http::close_version(body) else {
                return (
                    Response::bad_request("Invalid draft version. Refresh the editor state and try Done again."),
                    false,
                );
            };
            match editor.close(version) {
                Ok(()) => return (Response::no_content(), true),
                Err(CloseError::Stale) => Response::conflict(
                    "The draft changed in another request. Refresh the editor state and review it before trying Done again.",
                ),
                Err(CloseError::Trial(why)) => Response::conflict(&format!(
                    "The editor is still open. {why} Check the original Rex tab before trying Done again."
                )),
            }
        }
    };
    (response, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn close(editor: &mut Editor, body: &[u8], status: &[u8], should_close: bool) -> Vec<u8> {
        let (mut response, close) = respond(Route::Close, body, editor);
        let mut wire = Vec::new();
        while !response.write_to(&mut wire).unwrap() {}
        assert!(wire.starts_with(status), "{}", String::from_utf8_lossy(&wire));
        assert_eq!(close, should_close);
        wire
    }

    #[test]
    fn only_the_current_draft_version_can_close_the_session() {
        let folder = Fixture::folder();
        let mut editor = Editor::test_outside(&folder);
        let seen = editor.version();
        let edit = format!(r#"{{"version":{seen},"action":"set","field":"layout.width","value":0.41}}"#);
        let newer_draft = editor.action(edit.as_bytes()).unwrap();
        assert!(newer_draft.contains(r#""dirty":true"#));

        close(&mut editor, b"{}", b"HTTP/1.1 400 Bad Request\r\n", false);
        let stale = format!(r#"{{"version":{seen}}}"#);
        let conflict = close(&mut editor, stale.as_bytes(), b"HTTP/1.1 409 Conflict\r\n", false);
        assert!(String::from_utf8_lossy(&conflict).contains("Refresh the editor state"));
        assert_eq!(editor.state(), newer_draft);

        let current = format!(r#"{{"version":{}}}"#, editor.version());
        close(&mut editor, current.as_bytes(), b"HTTP/1.1 204 No Content\r\n", true);
    }

    #[test]
    fn close_waits_for_the_active_trial_to_undo() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        // Give the Rex CLI probe an isolated environment without changing it for other tests.
        const CHILD: &str = "MILO_TEST_WEB_TRIAL_CLOSE";
        if std::env::var_os(CHILD).is_none() {
            let folder = Fixture::folder();
            let rex = folder.root.join("rex");
            fs::write(
                &rex,
                concat!(
                    "#!/bin/sh\n/bin/cat >/dev/null\n",
                    "for arg do case \"$arg\" in\n",
                    "milo_operation=context) printf 'window:test\\nok - 120 40\\n'; exit;;\n",
                    "milo_operation=trial) echo 'ok window:test tiled outline=block:test'; exit;;\n",
                    "milo_operation=undo) echo undo >> undo.log; if [ -f allow-undo ]; then echo ok; ",
                    "else echo blocked; fi; exit;;\n",
                    "esac; done\nexit 1\n",
                ),
            )
            .unwrap();
            fs::set_permissions(rex, fs::Permissions::from_mode(0o700)).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "web::tests::close_waits_for_the_active_trial_to_undo", "--nocapture"])
                .env(CHILD, "1")
                .env("HOME", &folder.root)
                .env("XDG_CONFIG_HOME", folder.root.join("config"))
                .env("XDG_STATE_HOME", folder.root.join("state"))
                .env("PATH", &folder.root)
                .env("REX_CONFIG", folder.root.join("init.lua"))
                .env("REX_SESSION", "session:test")
                .env("REX_BLOCK", "block:shell")
                .current_dir(&folder.root)
                .output()
                .unwrap();
            assert!(
                output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }

        let mut editor = Editor::open();
        let seen = editor.version();
        let request = format!(r#"{{"version":{seen},"action":"try"}}"#);
        let active = editor.action(request.as_bytes()).unwrap();
        assert!(active.contains(r#""trial":true"#));

        let stale = format!(r#"{{"version":{seen}}}"#);
        close(&mut editor, stale.as_bytes(), b"HTTP/1.1 409 Conflict\r\n", false);
        assert!(matches!(fs::metadata("undo.log"), Err(error) if error.kind() == io::ErrorKind::NotFound));

        let current = format!(r#"{{"version":{}}}"#, editor.version());
        let rejected = close(&mut editor, current.as_bytes(), b"HTTP/1.1 409 Conflict\r\n", false);
        assert!(String::from_utf8_lossy(&rejected).contains("The editor is still open. Could not undo every pane"));
        assert_eq!(editor.state(), active);
        assert_eq!(fs::read_to_string("undo.log").unwrap(), "undo\n");

        fs::write("allow-undo", "").unwrap();
        close(&mut editor, current.as_bytes(), b"HTTP/1.1 204 No Content\r\n", true);
        assert_eq!(fs::read_to_string("undo.log").unwrap(), "undo\nundo\n");
        drop(editor);
        assert_eq!(fs::read_to_string("undo.log").unwrap(), "undo\nundo\n");
    }
}
