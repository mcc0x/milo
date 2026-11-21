//! Serves the settings editor on an ephemeral loopback address for this invocation only.

mod assets;
mod http;
mod launch;

use std::fs::File;
use std::io::{self, PipeReader, Read, Write};
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use self::http::{Access, Input, Request, Response, Route};
use self::launch::Launch;
use crate::setup::web::Editor;

pub(crate) const DEFAULT_PORT: u16 = 6110;
const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CLIENTS: usize = 16;
static STOP: AtomicBool = AtomicBool::new(false);
static WAKE_WRITER: AtomicI32 = AtomicI32::new(-1);

// Browsers omit the default port in both Host and Origin.
fn http_authority(address: std::net::SocketAddr) -> String {
    if address.port() == 80 { address.ip().to_string() } else { address.to_string() }
}

pub(crate) fn run(open_browser: bool, port: u16) -> Result<(), String> {
    STOP.store(false, Ordering::Relaxed);
    let wake = WakePipe::open().map_err(|error| format!("could not create a termination pipe: {error}"))?;
    let install =
        |signal| Signal::install(signal).map_err(|error| format!("could not handle termination signals: {error}"));
    let _signals = (install(libc::SIGINT)?, install(libc::SIGTERM)?, install(libc::SIGHUP)?, install(libc::SIGQUIT)?);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|error| format!("could not listen on 127.0.0.1:{port}: {error}; use --port 0 for a free port"))?;
    listener.set_nonblocking(true).map_err(|error| format!("could not configure the listener: {error}"))?;
    let address = listener.local_addr().map_err(|error| format!("could not read the loopback address: {error}"))?;
    let host = http_authority(address);
    let origin = format!("http://{host}");
    let token = random_token().map_err(|error| format!("could not generate a session token: {error}"))?;
    let bearer = format!("Bearer {token}");
    let url = format!("{origin}/#{token}");
    let access = Access { host: &host, origin: &origin, bearer: &bearer };
    let editor = Editor::open();
    if STOP.load(Ordering::Relaxed) {
        return Ok(());
    }
    {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{url}")
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("could not print the editor URL: {error}"))?;
    }
    let launcher = if open_browser && !STOP.load(Ordering::Relaxed) {
        match Launch::start(&url) {
            Ok(launcher) => Some(launcher),
            Err(error) => {
                eprintln!("milo web: could not open the browser ({error}); open the URL above");
                None
            }
        }
    } else {
        None
    };
    serve(listener, &wake.0, &access, editor, launcher).map_err(|error| format!("settings server: {error}"))
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
    listener: TcpListener,
    wake: &PipeReader,
    access: &Access<'_>,
    mut editor: Editor,
    launcher: Option<Launch>,
) -> io::Result<()> {
    let snapshot = Arc::new(editor.state());
    let (send, jobs) = mpsc::sync_channel::<EditorJob>(1);
    let (replies, receive) = mpsc::sync_channel(1);
    let (ready, mut notify) = io::pipe()?;
    let mut worker = EditorWorker { send, receive, ready };

    let thread = std::thread::Builder::new().spawn(move || {
        let mut hosted = Vec::new();
        while let Ok(job) = jobs.recv() {
            if STOP.load(Ordering::Relaxed) {
                break;
            }
            let reply = job.apply(&mut editor, &mut hosted);
            if replies.send(reply).is_err() || notify.write_all(&[0]).is_err() {
                break;
            }
        }
    })?;
    let result = serve_clients(listener, wake, access, snapshot, &mut worker, launcher);
    // Pending requests belong to the socket loop and have already been dropped.
    // Finish only the admitted job, then let the editor undo any active trial.
    drop(worker.send);
    thread.join().map_err(|_| io::Error::other("editor worker panicked"))?;
    result
}

struct EditorWorker {
    send: SyncSender<EditorJob>,
    receive: Receiver<Completion>,
    ready: PipeReader,
}

impl EditorWorker {
    fn submit(&self, job: EditorJob) -> io::Result<()> {
        // The poll loop submits only when no other job is in flight.
        self.send.try_send(job).map_err(|_| io::Error::other("editor worker could not accept a job"))
    }

    fn complete(&mut self) -> io::Result<Completion> {
        self.ready.read_exact(&mut [0])?;
        self.receive.try_recv().map_err(|_| io::Error::other("editor worker did not return a result"))
    }
}

fn serve_clients(
    listener: TcpListener,
    wake: &PipeReader,
    access: &Access<'_>,
    mut snapshot: Arc<String>,
    worker: &mut EditorWorker,
    mut launcher: Option<Launch>,
) -> io::Result<()> {
    let mut clients: Vec<Client> = Vec::with_capacity(MAX_CLIENTS);
    let mut fds = [libc::pollfd { fd: -1, events: libc::POLLIN, revents: 0 }; MAX_CLIENTS + 3];
    fds[1].fd = wake.as_raw_fd();
    fds[2].fd = worker.ready.as_raw_fd();
    let mut active: Option<Client> = None;
    let mut pending: Option<(Client, EditorJob)> = None;
    let mut last_activity = Instant::now();
    let mut closing = false;
    while !STOP.load(Ordering::Relaxed) && last_activity.elapsed() < IDLE_TIMEOUT {
        let now = Instant::now();
        clients.retain(|client| client.deadline > now);
        if closing && clients.is_empty() {
            break;
        }
        let mut deadline = last_activity + IDLE_TIMEOUT;
        if let Some((client, _)) = &pending {
            deadline = deadline.min(client.deadline);
        }
        if let Some(launch) = &mut launcher {
            if let Some(check) = launch.poll(now) {
                deadline = deadline.min(check);
            } else {
                launcher = None;
            }
        }
        let polled_clients = clients.len();
        let sockets = polled_clients + usize::from(active.is_some()) + usize::from(pending.is_some());
        fds[0].fd = if closing || sockets == MAX_CLIENTS { -1 } else { listener.as_raw_fd() };
        for (fd, client) in fds[3..].iter_mut().zip(&clients) {
            deadline = deadline.min(client.deadline);
            *fd = libc::pollfd { fd: client.socket.as_raw_fd(), events: client.interest(), revents: 0 };
        }
        let timeout = crate::term::milliseconds(deadline.saturating_duration_since(Instant::now()));
        let ready = crate::term::wait_ready(&mut fds[..polled_clients + 3], timeout);
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
        if pending.as_ref().is_some_and(|(client, _)| client.deadline <= Instant::now()) {
            let (mut client, _) = pending.take().expect("expired pending request");
            client.reply(Response::busy());
            clients.push(client);
        }
        if ready == 0 {
            continue;
        }
        if fds[2].revents != 0 {
            let completion = worker.complete()?;
            let mut client = active.take().expect("completion follows a job");
            client.reply(completion.response);
            last_activity = Instant::now();
            if completion.close {
                // Only flush the close response; never submit another job.
                clients.clear();
                clients.push(client);
                pending = None;
                closing = true;
                continue;
            }
            if let Some(state) = completion.snapshot {
                snapshot = state;
            }
            clients.push(client);
            if let Some((client, job)) = pending.take()
                && !STOP.load(Ordering::Relaxed)
            {
                worker.submit(job)?;
                active = Some(client);
            }
        }
        // A tail entry moved by swap_remove was either already visited or appended after poll.
        for index in (0..polled_clients).rev() {
            if STOP.load(Ordering::Relaxed) {
                break;
            }
            if fds[index + 3].revents == 0 {
                continue;
            }
            if clients[index].deadline <= Instant::now() {
                clients.swap_remove(index);
                continue;
            }
            let progress = clients[index].step(access, &snapshot);
            if matches!(progress, Ok(Progress::Reply { authenticated: true } | Progress::Job(_))) {
                last_activity = Instant::now();
                if let Some(launcher) = &mut launcher {
                    launcher.connected();
                }
            }
            match progress {
                Ok(Progress::Wait | Progress::Reply { .. }) => {}
                Ok(Progress::Job(job)) => {
                    if active.is_none() {
                        worker.submit(job)?;
                        active = Some(clients.swap_remove(index));
                    } else if pending.is_none() {
                        // One queued job leaves space for independent asset and state reads.
                        let mut client = clients.swap_remove(index);
                        client.deadline = Instant::now() + IO_TIMEOUT;
                        pending = Some((client, job));
                    } else {
                        clients[index].reply(Response::busy());
                    }
                }
                Ok(Progress::Done) | Err(_) => {
                    clients.swap_remove(index);
                }
            }
        }
        if !closing && !STOP.load(Ordering::Relaxed) && fds[0].revents != 0 {
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
    Reply { authenticated: bool },
    Job(EditorJob),
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

    fn step(&mut self, access: &Access<'_>, snapshot: &Arc<String>) -> io::Result<Progress> {
        let (response, authenticated) = match &mut self.exchange {
            Exchange::Reading(input) => match input.read_from(&mut self.socket, access)? {
                Input::Wait => return Ok(Progress::Wait),
                Input::Disconnected => return Ok(Progress::Done),
                Input::Rejected(rejection) => (rejection.response(), false),
                Input::Ready(route @ (Route::Action | Route::Close | Route::TelegramChats | Route::HostedConnect)) => {
                    return Ok(Progress::Job(EditorJob { route, request: std::mem::take(input) }));
                }
                Input::Ready(route) => {
                    let authenticated = route.authenticated();
                    (respond_read(route, snapshot), authenticated)
                }
            },
            Exchange::Writing(output) => {
                return Ok(if output.write_to(&mut self.socket)? { Progress::Done } else { Progress::Wait });
            }
        };
        self.reply(response);
        Ok(Progress::Reply { authenticated })
    }

    fn reply(&mut self, response: Response) {
        self.exchange = Exchange::Writing(response);
        self.deadline = Instant::now() + IO_TIMEOUT;
    }
}

fn respond_read(route: Route, snapshot: &Arc<String>) -> Response {
    match route {
        Route::Static(asset) => asset.response(),
        Route::State => Response::json(Arc::clone(snapshot)),
        Route::Action | Route::Close | Route::TelegramChats | Route::HostedConnect => {
            unreachable!("route runs on the editor worker")
        }
    }
}

struct EditorJob {
    route: Route,
    request: Request,
}

struct Completion {
    response: Response,
    snapshot: Option<Arc<String>>,
    close: bool,
}

impl EditorJob {
    fn apply(self, editor: &mut Editor, hosted: &mut Vec<HostedReceipt>) -> Completion {
        let body = self.request.body();
        if self.route == Route::Close {
            let (response, close) = respond_close(body, editor);
            return Completion { response, snapshot: None, close };
        }
        if self.route == Route::TelegramChats {
            return Completion { response: respond_telegram_chats(body, editor), snapshot: None, close: false };
        }
        if self.route == Route::HostedConnect {
            return respond_hosted_connect(body, editor, hosted);
        }
        let (response, snapshot) = match editor.action(body) {
            Ok(state) => {
                let snapshot = Arc::new(state);
                (Response::json(Arc::clone(&snapshot)), snapshot)
            }
            // A failed action may still change what the editor reports.
            Err(error) => (Response::bad_request(&error), Arc::new(editor.state())),
        };
        Completion { response, snapshot: Some(snapshot), close: false }
    }
}

fn respond_close(body: &[u8], editor: &mut Editor) -> (Response, bool) {
    use crate::setup::web::CloseError;

    let Some(version) = http::close_version(body) else {
        return (Response::bad_request("Invalid draft version. Refresh the editor state and try Done again."), false);
    };
    let response = match editor.close(version) {
        Ok(()) => return (Response::no_content(), true),
        Err(CloseError::Stale) => Response::conflict(
            "The draft changed in another request. Refresh the editor state and review it before trying Done again.",
        ),
        Err(CloseError::Trial(why)) => Response::conflict(&format!(
            "The editor is still open. {why} Check the original Rex tab before trying Done again."
        )),
    };
    (response, false)
}

fn respond_telegram_chats(body: &[u8], editor: &Editor) -> Response {
    use crate::alerts::TelegramChatsError::{InvalidToken, Unavailable};

    match http::telegram_token(body).ok_or(InvalidToken).and_then(|token| editor.telegram_chats(&token)) {
        Ok(chats) => Response::json_owned(chats),
        Err(InvalidToken) => Response::bad_request("Enter a valid Telegram bot token from BotFather."),
        Err(Unavailable) => {
            Response::bad_gateway("Could not look up Telegram chats. Check the bot token or enter the chat ID manually.")
        }
    }
}

struct HostedReceipt {
    key: String,
    end: Instant,
    outcome: HostedOutcome,
}

#[derive(Clone, Copy)]
enum HostedOutcome {
    Saved,
    Discarded { cleanup_confirmed: bool },
}

fn respond_hosted_connect(body: &[u8], editor: &mut Editor, receipts: &mut Vec<HostedReceipt>) -> Completion {
    use crate::alerts::relay::{ConnectError, Connected, ConnectionRequest};
    use crate::setup::web::HostedSaveError;

    let plain = |response| Completion { response, snapshot: None, close: false };
    let request = match ConnectionRequest::parse(body) {
        Ok(request) => request,
        Err(error) => return plain(hosted_error(error)),
    };
    let Some((version, key)) = request.save() else {
        if request.provider() == crate::alerts::Destination::Telegram
            && let Err(error) = editor.check_telegram_reconnect()
        {
            return Completion {
                response: Response::conflict(&error),
                snapshot: Some(Arc::new(editor.state())),
                close: false,
            };
        }
        return plain(match request.run() {
            Ok(Connected::Ready(reply)) => Response::json_owned(reply),
            Ok(Connected::Pending) => Response::pairing_pending(),
            Ok(Connected::Claimed(_)) => hosted_error(ConnectError::Invalid),
            Err(error) => hosted_error(error),
        });
    };
    // A retry after a lost local response must not apply the same change twice,
    // or re-save a Discord webhook deleted after a failed local write.
    let now = Instant::now();
    if let Some(reply) = hosted_replay(receipts, key, now, editor) {
        return reply;
    }
    let outcome = match editor.save_hosted_connection(version, &request) {
        Ok(state) => (HostedOutcome::Saved, hosted_saved(state)),
        Err(HostedSaveError::Pending) => return plain(Response::pairing_pending()),
        Err(HostedSaveError::Stale) => {
            return plain(Response::conflict("Settings changed. Review the destination and try Save again."));
        }
        Err(HostedSaveError::Relay(error)) => return plain(hosted_error(error)),
        Err(HostedSaveError::Local(error)) => {
            return Completion {
                response: Response::error("500 Internal Server Error", &error),
                snapshot: Some(Arc::new(editor.state())),
                close: false,
            };
        }
        Err(HostedSaveError::Discarded { cleanup_confirmed }) => (
            HostedOutcome::Discarded { cleanup_confirmed },
            Completion {
                response: Response::pairing_discarded(cleanup_confirmed),
                snapshot: Some(Arc::new(editor.state())),
                close: false,
            },
        ),
    };
    // This covers the entire relay receipt lifetime, which began before Save.
    let (outcome, reply) = outcome;
    receipts.push(HostedReceipt { key: key.into(), end: now + Duration::from_secs(600), outcome });
    reply
}

fn hosted_replay(receipts: &mut Vec<HostedReceipt>, key: &str, now: Instant, editor: &Editor) -> Option<Completion> {
    receipts.retain(|receipt| receipt.end > now);
    let receipt = receipts.iter().find(|receipt| receipt.key == key)?;
    Some(match receipt.outcome {
        HostedOutcome::Saved => hosted_saved(editor.state()),
        HostedOutcome::Discarded { cleanup_confirmed } => {
            Completion { response: Response::pairing_discarded(cleanup_confirmed), snapshot: None, close: false }
        }
    })
}

fn hosted_saved(state: String) -> Completion {
    let snapshot = Arc::new(state);
    Completion { response: Response::json(Arc::clone(&snapshot)), snapshot: Some(snapshot), close: false }
}

fn hosted_error(error: crate::alerts::relay::ConnectError) -> Response {
    use crate::alerts::relay::ConnectError;

    match error {
        ConnectError::Invalid => Response::bad_request("Invalid hosted connection request."),
        ConnectError::Proof => Response::bad_request("The finish code did not match. Check the code and try again."),
        ConnectError::Expired => Response::error("404 Not Found", "This connection expired or was already claimed."),
        ConnectError::Busy => Response::error("429 Too Many Requests", "Too many connection requests. Try again shortly."),
        // The relay answers 503 for a provider it has switched off, and equally for a passing
        // limit: Telegram still catching up, or too many pairings waiting.
        ConnectError::Disabled => Response::error(
            "503 Service Unavailable",
            "Milo's connection service cannot set up this provider right now. Try again later.",
        ),
        ConnectError::Unavailable => Response::bad_gateway("Could not reach Milo's connection service."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn close(editor: &mut Editor, body: &[u8], status: &[u8], should_close: bool) -> Vec<u8> {
        let (mut response, close) = respond_close(body, editor);
        let mut wire = Vec::new();
        while !response.write_to(&mut wire).unwrap() {}
        assert!(wire.starts_with(status), "{}", String::from_utf8_lossy(&wire));
        assert_eq!(close, should_close);
        wire
    }

    #[test]
    fn hosted_save_replays_are_terminal_and_expire() {
        let fixture = Fixture::folder();
        let editor = Editor::test_outside(&fixture);
        let now = Instant::now();
        let end = now + Duration::from_secs(600);
        let mut receipts = vec![
            HostedReceipt { key: "saved".into(), end, outcome: HostedOutcome::Saved },
            HostedReceipt {
                key: "discarded".into(),
                end,
                outcome: HostedOutcome::Discarded { cleanup_confirmed: true },
            },
        ];
        let saved = hosted_replay(&mut receipts, "saved", now, &editor).unwrap();
        assert!(saved.snapshot.is_some());
        let discarded = hosted_replay(&mut receipts, "discarded", now, &editor).unwrap();
        assert!(discarded.snapshot.is_none());
        let mut response = discarded.response;
        let mut wire = Vec::new();
        while !response.write_to(&mut wire).unwrap() {}
        assert!(wire.starts_with(b"HTTP/1.1 410 Gone\r\n"));
        assert!(wire.ends_with(br#"{"cleanupConfirmed":true}"#));
        assert!(hosted_replay(&mut receipts, "discarded", end, &editor).is_none());
        assert!(receipts.is_empty());
    }

    #[test]
    fn a_relay_503_is_reported_as_temporary_not_as_a_provider_switched_off() {
        use crate::alerts::relay::ConnectError;

        // The relay also answers 503 while Telegram catches up or pairings are at their limit.
        let mut response = hosted_error(ConnectError::Disabled);
        let mut wire = Vec::new();
        while !response.write_to(&mut wire).unwrap() {}
        let reply = String::from_utf8(wire).unwrap();
        assert!(reply.starts_with("HTTP/1.1 503 Service Unavailable\r\n"), "{reply}");
        let body = reply.split_once("\r\n\r\n").unwrap().1.to_ascii_lowercase();
        assert!(body.contains("try again"), "{body}");
        for claim in ["not enabled", "disabled", "not configured", "unconfigured"] {
            assert!(!body.contains(claim), "{body}");
        }
    }

    #[test]
    fn session_authority_matches_browser_origin_serialization() {
        assert_eq!(http_authority((Ipv4Addr::LOCALHOST, 80).into()), "127.0.0.1");
        assert_eq!(http_authority((Ipv4Addr::LOCALHOST, 6110).into()), "127.0.0.1:6110");
    }

    #[test]
    fn expired_queued_actions_receive_busy_without_being_submitted() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let host = address.to_string();
        let origin = format!("http://{host}");
        let access = Access { host: &host, origin: &origin, bearer: "Bearer test" };
        let (wake, stop) = io::pipe().unwrap();
        let (ready, _notify) = io::pipe().unwrap();
        let (send, jobs) = mpsc::sync_channel(1);
        let (_replies, receive) = mpsc::sync_channel(1);
        let mut worker = EditorWorker { send, receive, ready };
        std::thread::scope(|scope| {
            let server = scope.spawn(|| serve_clients(listener, &wake, &access, Arc::default(), &mut worker, None));
            let _stop_on_exit = stop;
            let request = || {
                let mut socket = TcpStream::connect(address).unwrap();
                write!(socket, "POST /api/action HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nAuthorization: Bearer test\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}").unwrap();
                socket
            };
            let _active = request();
            jobs.recv_timeout(IO_TIMEOUT).unwrap();
            let mut pending = request();
            pending.set_read_timeout(Some(IO_TIMEOUT * 2)).unwrap();
            let mut reply = String::new();
            pending.read_to_string(&mut reply).unwrap();
            assert!(reply.starts_with("HTTP/1.1 503 Service Unavailable\r\n"), "{reply}");
            assert!(jobs.try_recv().is_err());
            drop(_stop_on_exit);
            server.join().unwrap().unwrap();
        });
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
            let output = std::process::Command::new(std::env::current_exe().unwrap())
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

    #[test]
    fn reads_serve_the_committed_snapshot_while_an_action_waits_for_rex() {
        use std::fs;
        use std::net::SocketAddr;
        use std::os::unix::fs::PermissionsExt as _;
        use std::thread;

        const TEST: &str = "web::tests::reads_serve_the_committed_snapshot_while_an_action_waits_for_rex";
        const CHILD: &str = "MILO_TEST_WEB_SLOW_ACTION";
        if std::env::var_os(CHILD).is_none() {
            let folder = Fixture::folder();
            let rex = folder.root.join("rex");
            folder.write(
                "rex",
                concat!(
                    "#!/bin/sh\n/bin/cat >/dev/null\n",
                    "for arg do case \"$arg\" in\n",
                    "milo_operation=context)\n",
                    "if [ -f pause-context ]; then\n",
                    "  : > context-entered\n",
                    "  retries=200\n",
                    "  until [ -f release-context ]; do\n",
                    "    [ \"$retries\" -gt 0 ] || exit 1\n",
                    "    retries=$((retries - 1))\n",
                    "    /bin/sleep 0.05\n",
                    "  done\n",
                    "fi\n",
                    "printf 'window:test\\nok - 120 40\\n'; exit;;\n",
                    "esac; done\nexit 1\n",
                ),
            );
            fs::set_permissions(rex, fs::Permissions::from_mode(0o700)).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
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

        const TEST_WAIT: Duration = Duration::from_secs(2);
        const BEARER: &str = "Bearer gated-doctor";

        fn request(address: SocketAddr, method: &str, path: &str, body: &str) -> TcpStream {
            let mut socket = TcpStream::connect_timeout(&address, TEST_WAIT).unwrap();
            socket.set_read_timeout(Some(TEST_WAIT)).unwrap();
            socket.set_write_timeout(Some(TEST_WAIT)).unwrap();
            write!(
                socket,
                "{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: {BEARER}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            socket
        }

        fn reply(mut socket: TcpStream, status: &str) -> String {
            let mut wire = String::new();
            socket.read_to_string(&mut wire).unwrap();
            assert!(wire.starts_with(status), "{wire}");
            wire.split_once("\r\n\r\n").unwrap().1.to_owned()
        }

        let editor = Editor::open();
        let version = editor.version();
        let original = editor.state();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let host = address.to_string();
        let origin = format!("http://{host}");
        let access = Access { host: &host, origin: &origin, bearer: BEARER };
        let (wake, writer) = io::pipe().unwrap();
        thread::scope(|scope| {
            let browser = scope.spawn(move || {
                let _stop_on_exit = writer;
                fs::write("pause-context", "").unwrap();
                let doctor = format!(r#"{{"version":{version},"action":"doctor"}}"#);
                let active = request(address, "POST", "/api/action", &doctor);
                let deadline = Instant::now() + TEST_WAIT;
                while !std::path::Path::new("context-entered").exists() {
                    assert!(Instant::now() < deadline, "Doctor did not reach the Rex gate");
                    thread::sleep(Duration::from_millis(10));
                }
                // Both reads must complete before the Rex probe is allowed to finish.
                let state = || reply(request(address, "GET", "/api/state", ""), "HTTP/1.1 200 OK\r\n");
                assert_eq!(state(), original);
                let index = reply(request(address, "GET", "/", ""), "HTTP/1.1 200 OK\r\n");
                assert!(index.contains("Milo"));

                let stale = format!(r#"{{"version":{version}}}"#);
                let a = request(address, "POST", "/api/close", &stale);
                let b = request(address, "POST", "/api/close", &stale);
                let mut fds =
                    [&a, &b].map(|socket| libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLIN, revents: 0 });
                let ready = crate::term::wait_ready(&mut fds, crate::term::milliseconds(TEST_WAIT));
                assert_eq!(ready, 1, "only an overflow request may finish while Doctor is gated");
                let (busy, pending) = if fds[0].revents != 0 { (a, b) } else { (b, a) };
                reply(busy, "HTTP/1.1 503 Service Unavailable\r\n");

                fs::write("release-context", "").unwrap();
                let updated = reply(active, "HTTP/1.1 200 OK\r\n");
                assert_ne!(updated, original);
                assert_eq!(state(), updated);
                reply(pending, "HTTP/1.1 409 Conflict\r\n");
                let current = format!(r#"{{"version":{}}}"#, version + 1);
                reply(request(address, "POST", "/api/close", &current), "HTTP/1.1 204 No Content\r\n");
            });
            let result = serve(listener, &wake, &access, editor, None);
            browser.join().unwrap();
            result.unwrap();
        });
    }
}
