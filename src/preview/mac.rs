use std::ffi::{CStr, OsStr};
use std::fs::{self, DirBuilder};
use std::io::{self, BufRead as _, Read, Write};
use std::num::NonZeroU32;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::Anchor;
use crate::fs_ops;

const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/Milo Preview"));
const FOCUS_FD_ENV: &str = "MILO_PREVIEW_FOCUS_FD";
const PIN_REFUSED: i32 = 2; // PinRefused in macos/Preview/main.m.
const PIN_REFUSAL_PREFIX: &[u8] = b"refused: ";
const PIN_REASON_MAX: usize = 160;
const PIN_REPLY_MAX: usize = PIN_REFUSAL_PREFIX.len() + PIN_REASON_MAX + 1; // Includes the newline.
const ABNORMAL_PIN_EXIT: &str = "the preview window check ended unexpectedly; no pane was opened";
const INVALID_PIN_SUCCESS: &str = "the preview window check returned an invalid window ID; no pane was opened";

fn focus_pipe() -> io::Result<(io::PipeReader, io::PipeWriter)> {
    let (reader, writer) = io::pipe()?;
    // SAFETY: writer owns this open pipe. A helper that has already closed
    // must not terminate the launcher when a synchronous Rex reply arrives.
    if unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETNOSIGPIPE, 1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((reader, writer))
}

fn inherit_focus_reader(command: &mut Command, reader: io::PipeReader) {
    command.env(FOCUS_FD_ENV, reader.as_raw_fd().to_string());
    inherit_pipe(command, reader);
}

fn inherit_pipe(command: &mut Command, pipe: impl AsRawFd + Send + Sync + 'static) {
    let inherit = move || {
        // SAFETY: the closure owns this open pipe endpoint.
        if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    };
    // SAFETY: the hook runs only async-signal-safe fcntl and captures errno on failure.
    unsafe { command.pre_exec(inherit) };
}

/// Install independently of the checkout, PATH, HOME or shell's temporary directory.
/// The OS may evict this cache; this location does not establish a privacy identity.
fn cache() -> io::Result<PathBuf> {
    let mut bytes = [0u8; 1024];
    // SAFETY: bytes is writable for the full size passed to confstr.
    let len = unsafe { libc::confstr(libc::_CS_DARWIN_USER_CACHE_DIR, bytes.as_mut_ptr().cast(), bytes.len()) };
    let path = bytes
        .get(..len)
        .and_then(|s| CStr::from_bytes_with_nul(s).ok())
        .map(|s| Path::new(OsStr::from_bytes(s.to_bytes())))
        .filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::other("no private macOS user cache"))?;
    let private = |p: &Path| -> io::Result<()> {
        let meta = fs::symlink_metadata(p)?;
        // SAFETY: geteuid takes no arguments and does not access any caller-supplied memory.
        if meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0 {
            Ok(())
        } else {
            Err(io::Error::other("preview needs a private user cache directory"))
        }
    };
    private(path)?;
    // Never replace the experimental executable a user may have listed in Accessibility.
    let dir = path.join("net.usemilo.preview.tiled");
    match DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => (),
    }
    private(&dir)?;
    Ok(dir)
}

fn matches_build(path: &Path) -> io::Result<bool> {
    let mut file = match fs_ops::open_read(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        other => other?,
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("unexpected file in preview cache"));
    }
    if meta.len() != HELPER.len() as u64 || meta.mode() & 0o777 != 0o500 {
        return Ok(false);
    }
    let mut buf = [0u8; 8192];
    for chunk in HELPER.chunks(buf.len()) {
        let part = &mut buf[..chunk.len()];
        file.read_exact(part)?;
        if part != chunk {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Keep the lock through spawn: two Milo versions must each launch the helper
/// they built. Publish a fresh inode rather than modifying a running executable.
fn spawn(args: &[&str], state: Stdio, focus: Option<io::PipeReader>) -> io::Result<Child> {
    let dir = cache()?;
    let lock = fs_ops::open_dir(&dir)?;
    lock.lock()?;
    let path = dir.join("Milo Preview");
    if !matches_build(&path)? {
        let (staged, mut file) = fs_ops::Temp::create(dir.join(format!(".preview-{}", std::process::id())))?;
        file.write_all(HELPER)?;
        file.set_permissions(fs::Permissions::from_mode(0o500))?;
        drop(file);
        staged.replace(&path)?;
    }
    // Only fixed protocol records belong on stdout; do not relay page diagnostics to the shell.
    let mut command = Command::new(path);
    command.args(args).stdin(state).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(reader) = focus {
        inherit_focus_reader(&mut command, reader);
    }
    command.spawn()
}

pub(crate) fn pin_window(owner_pid: u32) -> Result<NonZeroU32, String> {
    let mut helper = spawn(&["--pin-window", &owner_pid.to_string()], Stdio::null(), None)
        .map_err(|e| format!("could not run the preview window check: {e}"))?;
    let mut stdout = helper.stdout.take().expect("helper stdout is piped");
    let mut reply = [0; PIN_REPLY_MAX + 1];
    let mut size = 0;
    // Read one byte beyond the longest valid record to distinguish it from a
    // truncated one. Close the pipe before waiting even if it had more data.
    let read = loop {
        if size == reply.len() {
            break Ok(());
        }
        match stdout.read(&mut reply[size..]) {
            Ok(0) => break Ok(()),
            Ok(n) => size += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => break Err(e),
        }
    };
    drop(stdout);
    let status = helper.wait().map_err(|_| ABNORMAL_PIN_EXIT)?;
    read.map_err(|_| ABNORMAL_PIN_EXIT)?;
    parse_pin_reply(status.code(), &reply[..size])
}

fn parse_pin_reply(code: Option<i32>, reply: &[u8]) -> Result<NonZeroU32, String> {
    let line = reply.strip_suffix(b"\n");
    match code {
        Some(0) => line
            .filter(|id| id.len() <= 10 && matches!(id.first().copied(), Some(b'1'..=b'9')))
            .and_then(|id| std::str::from_utf8(id).ok()?.parse().ok())
            .ok_or_else(|| INVALID_PIN_SUCCESS.into()),
        Some(PIN_REFUSED) => {
            let reason = line
                .and_then(|line| line.strip_prefix(PIN_REFUSAL_PREFIX))
                .filter(|reason| {
                    !reason.is_empty()
                        && reason.len() <= PIN_REASON_MAX
                        && !reason.starts_with(b" ")
                        && !reason.ends_with(b" ")
                        && reason.iter().all(|byte| (b' '..=b'~').contains(byte))
                })
                .and_then(|reason| std::str::from_utf8(reason).ok());
            Err(match reason {
                Some(reason) => format!("Preview could not pin a Rex window: {reason}. No pane was opened."),
                None => ABNORMAL_PIN_EXIT.into(),
            })
        }
        _ => Err(ABNORMAL_PIN_EXIT.into()),
    }
}

const STARTUP_WAIT: Duration = Duration::from_secs(15);

#[repr(u8)]
#[derive(Clone, Copy)]
enum StartupStatus {
    Ready,
    ObserverScope,
    ObserverPipe,
    FocusPipe,
    HelperLaunch,
    HelperInit,
    ObserverLaunch,
    ObserverInit,
}

impl StartupStatus {
    fn from_reply(reply: u8) -> Result<(), String> {
        let problem = match reply {
            n if n == Self::Ready as u8 => return Ok(()),
            n if n == Self::ObserverScope as u8 => "couldn't confirm the preview's local Rex session; reopen from the local working terminal",
            n if n == Self::ObserverPipe as u8 => "couldn't open the Rex preview observer pipe",
            n if n == Self::FocusPipe as u8 => "couldn't open the preview focus acknowledgment pipe",
            n if n == Self::HelperLaunch as u8 => "couldn't launch the local WebKit helper",
            n if n == Self::HelperInit as u8 => "the WebKit helper couldn't initialize; check the Rex window is still visible and reopen the preview",
            n if n == Self::ObserverLaunch as u8 => "couldn't start the local Rex preview observer",
            n if n == Self::ObserverInit as u8 => "Rex couldn't verify the preview terminal and its viewer; reopen from the local working terminal",
            _ => "the local preview returned an invalid startup response",
        };
        Err(problem.into())
    }
}

/// The directory and socket have no URL content. The endpoint exists only
/// until this opener accepts its one peer or leaves the startup path.
pub(crate) struct StartupSocket {
    dir: PathBuf,
    path: PathBuf,
    listener: Option<UnixListener>,
}

impl StartupSocket {
    pub(crate) fn new() -> io::Result<Self> {
        let mut name = *b"/private/tmp/milo-preview.XXXXXX\0";
        // SAFETY: This mutable NUL-terminated template is valid for mkdtemp;
        // it creates the directory with mode 0700 and retains no reference.
        if unsafe { libc::mkdtemp(name.as_mut_ptr().cast()) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        let dir = PathBuf::from(OsStr::from_bytes(&name[..name.len() - 1]));
        let path = dir.join("start");
        let mut socket = Self { dir, path, listener: None };
        let listener = UnixListener::bind(&socket.path)?;
        listener.set_nonblocking(true)?;
        socket.listener = Some(listener);
        Ok(socket)
    }

    pub(crate) fn path(&self) -> &str {
        self.path.to_str().expect("startup socket path is ASCII")
    }

    pub(crate) fn start(
        mut self,
        url: &str,
        authorize: impl FnOnce(u32) -> Result<(), String>,
        publish: impl FnOnce(u32) -> Result<(), String>,
    ) -> Result<(), String> {
        let listener = self.listener.take().expect("startup listener was initialized");
        let mut fds = [libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        let mut peer = if crate::term::wait_ready(&mut fds, 3_000) > 0 {
            listener.accept().ok().map(|(peer, _)| peer)
        } else {
            None
        }
        .ok_or("the quiet preview terminal didn't start; check that Rex supports direct command launch")?;
        drop(self);
        drop(listener);
        configure_peer(&peer).map_err(|_| "couldn't verify the quiet preview startup connection")?;
        let pid = peer_pid(&peer).map_err(|_| "couldn't identify the quiet preview terminal process")?;
        authorize(pid)?;
        let mut reply = [0];
        write_address(&mut peer, url)
            .and_then(|()| peer.read_exact(&mut reply))
            .map_err(|_| "the quiet preview didn't initialize; check the Rex window and run preview again")?;
        StartupStatus::from_reply(reply[0])?;
        // Keep the peer on its startup lease until Rex confirms publication.
        // An ambiguous failure drops the socket, so even a late label change
        // cannot leave behind a running helper or observer.
        publish(pid)?;
        peer.write_all(b"P").map_err(|_| "the preview terminal closed during publication".into())
    }
}

impl Drop for StartupSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.dir);
    }
}

fn configure_peer(peer: &UnixStream) -> io::Result<()> {
    let fd = peer.as_raw_fd();
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: fd is a connected Unix stream and both output pointers are valid.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: geteuid has no preconditions.
    if uid != unsafe { libc::geteuid() } {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    // SAFETY: fd is still owned by this connected socket.
    if unsafe { libc::fcntl(fd, libc::F_SETNOSIGPIPE, 1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // Darwin's accepted socket can inherit O_NONBLOCK from the listener.
    // Use blocking I/O with explicit timeouts for the framed exchange.
    peer.set_nonblocking(false)?;
    peer.set_read_timeout(Some(STARTUP_WAIT))?;
    peer.set_write_timeout(Some(STARTUP_WAIT))
}

fn peer_pid(peer: &UnixStream) -> io::Result<u32> {
    let mut pid: libc::pid_t = 0;
    let expected = libc::socklen_t::try_from(std::mem::size_of_val(&pid)).expect("pid_t fits in socklen_t");
    let mut size = expected;
    // SAFETY: pid is valid for the indicated output size and peer owns fd.
    if unsafe {
        libc::getsockopt(peer.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut size)
    } != 0 {
        return Err(io::Error::last_os_error());
    }
    if size != expected || pid < 2 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    u32::try_from(pid).map_err(|_| io::ErrorKind::InvalidData.into())
}

fn write_address(mut channel: impl Write, url: &str) -> io::Result<()> {
    let size = u32::try_from(url.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    channel.write_all(&size.to_be_bytes())?;
    channel.write_all(url.as_bytes())
}

fn read_address(channel: &mut UnixStream, buf: &mut [u8; super::URL_MAX]) -> io::Result<usize> {
    let mut size = [0; 4];
    channel.read_exact(&mut size)?;
    let size = usize::try_from(u32::from_be_bytes(size)).map_err(|_| io::ErrorKind::InvalidData)?;
    if size == 0 || size > buf.len() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    channel.read_exact(&mut buf[..size])?;
    Ok(size)
}

/// The direct child never writes to the PTY, including when Rex restores
/// this command later and its one-time startup socket no longer exists.
pub(crate) fn anchor(startup: &Path, anchor: Anchor<'_>) -> Result<(), String> {
    let _quiet = QuietInput::disable_echo();
    let mut peer = UnixStream::connect(startup).map_err(|_| "the preview startup request expired")?;
    configure_peer(&peer).map_err(|_| "couldn't verify the preview startup connection")?;
    let mut address = [0; super::URL_MAX];
    let count = read_address(&mut peer, &mut address).map_err(|_| "couldn't read the preview address")?;
    let address = std::str::from_utf8(&address[..count]).map_err(|_| "invalid preview address encoding")?;
    let address = super::url(address)?;
    let processes = match start_anchor(address, anchor) {
        Ok(processes) => processes,
        Err(status) => {
            let _ = peer.write_all(&[status as u8]);
            return StartupStatus::from_reply(status as u8);
        }
    };
    let (mut helper, mut watcher, reply) = processes;
    let mut published = [0];
    if peer.write_all(&[StartupStatus::Ready as u8]).is_err()
        || peer.read_exact(&mut published).is_err()
        || published != *b"P"
    {
        let _ = helper.kill();
        let _ = watcher.kill();
        let _ = helper.wait();
        let _ = watcher.wait();
        return Err("the preview launcher closed during startup".into());
    }
    drop(peer);
    run_anchor(helper, watcher, reply, anchor)
}

struct QuietInput(libc::termios);

impl QuietInput {
    fn disable_echo() -> Option<Self> {
        let fd = libc::STDIN_FILENO;
        let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr initializes the struct on success and retains no pointer.
        if unsafe { libc::tcgetattr(fd, mode.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: tcgetattr succeeded above.
        let original = unsafe { mode.assume_init() };
        let mut mode = original;
        mode.c_lflag &= !(libc::ECHO | libc::ECHONL);
        // SAFETY: mode came from this same terminal; the only change is to echo.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &mode) } == 0 { Some(Self(original)) } else { None }
    }
}

impl Drop for QuietInput {
    fn drop(&mut self) {
        // SAFETY: restore the exact settings for the same inherited descriptor.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0) };
    }
}

fn start_anchor(url: &str, anchor: Anchor<'_>) -> Result<(Child, Child, io::PipeWriter), StartupStatus> {
    let (input, mut state) = io::pipe().map_err(|_| StartupStatus::ObserverPipe)?;
    let (mut watch_ready, send_ready) = io::pipe().map_err(|_| StartupStatus::ObserverPipe)?;
    let mut watch =
        crate::rex::preview_observer_command(anchor, send_ready.as_raw_fd()).map_err(|_| StartupStatus::ObserverScope)?;
    inherit_pipe(&mut watch, send_ready);
    let (reader, reply) = focus_pipe().map_err(|_| StartupStatus::FocusPipe)?;
    let mut helper = spawn(&["--address-stdin", &anchor.native_window_id.to_string()], input.into(), Some(reader))
        .map_err(|_| StartupStatus::HelperLaunch)?;
    let watcher = write_address(&mut state, url)
        .map_err(|_| StartupStatus::HelperInit)
        .and_then(|()| {
            // No observation may precede the helper's clock baseline.
            after_clock_ready(&mut helper).map_err(|_| StartupStatus::HelperInit)?;
            let mut watcher = watch.stdout(state).spawn().map_err(|_| StartupStatus::ObserverLaunch)?;
            drop(watch); // Rust must not keep the write end alive across a refused startup.
            if !read_ready(&mut watch_ready, 3_000) {
                let _ = watcher.kill();
                let _ = watcher.wait();
                return Err(StartupStatus::ObserverInit);
            }
            Ok(watcher)
        });
    match watcher {
        Ok(watcher) => Ok((helper, watcher, reply)),
        Err(status) => {
            let _ = helper.kill();
            let _ = helper.wait();
            Err(status)
        }
    }
}

fn run_anchor(helper: Child, mut watcher: Child, reply: io::PipeWriter, anchor: Anchor<'_>) -> Result<(), String> {
    let result = serve_actions(helper, reply, || crate::rex::return_preview_focus(anchor).is_ok());
    // EOF can reach the helper before Rex's exit status is waitable. Give it
    // a short grace period, but report nothing from the Rex process itself.
    let deadline = Instant::now() + Duration::from_millis(80);
    let status = loop {
        match watcher.try_wait() {
            Ok(None) if result.is_ok() && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            status => break status.ok().flatten(),
        }
    };
    if let Some(status) = status {
        if result.is_ok() && !status.success() {
            return Err("the Rex preview observer failed; the preview was closed".into());
        }
    } else {
        let _ = watcher.kill();
        let _ = watcher.wait();
    }
    result
}

fn after_clock_ready(helper: &mut Child) -> Result<(), &'static str> {
    let output = helper.stdout.as_mut().expect("helper stdout is piped");
    if read_ready(output, 8_000) {
        Ok(())
    } else {
        Err("the WebKit helper did not initialize the preview observer; no page was shown")
    }
}

fn read_ready(pipe: &mut (impl Read + AsRawFd), timeout_ms: i32) -> bool {
    let mut fds = [libc::pollfd { fd: pipe.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
    let mut receipt = [0];
    crate::term::wait_ready(&mut fds, timeout_ms) > 0 && pipe.read_exact(&mut receipt).is_ok() && receipt == *b"R"
}

fn serve_actions(
    mut helper: Child,
    mut reply: io::PipeWriter,
    mut return_focus: impl FnMut() -> bool,
) -> Result<(), String> {
    let mut actions = io::BufReader::new(helper.stdout.take().expect("helper stdout is piped"));
    let mut action = String::new();
    loop {
        action.clear();
        match actions.read_line(&mut action) {
            Ok(0) => break,
            Ok(_) if action == "focus\n" => {
                // A tab may disappear while Rex handles the request. Never
                // activate a different terminal or discard the existing page.
                let result = if return_focus() { b"1" } else { b"0" };
                if reply.write_all(result).is_err() {
                    // The helper may have closed while Rex was handling the
                    // request. EOF makes any surviving helper close as well.
                    break;
                }
            }
            _ => {
                let _ = helper.kill();
                break;
            }
        }
    }
    drop(reply);
    match helper.wait().map_err(|_| "could not wait for the WebKit helper")?.code() {
        Some(0) => Ok(()),
        Some(3) => Err("the Mac viewer didn't claim the preview tile; reopen from a Mac-created working terminal".into()),
        _ => Err("WebKit preview stopped".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_sends_the_literal_address_only_after_peer_authorization() {
        let socket = StartupSocket::new().unwrap();
        let path = PathBuf::from(socket.path());
        assert_eq!(path.parent().unwrap().metadata().unwrap().permissions().mode() & 0o777, 0o700);
        let url = "https://example.com/private/a%2Fb&$HOME";
        std::thread::scope(|scope| {
            let path = &path;
            let child = scope.spawn(move || {
                let mut peer = UnixStream::connect(path).unwrap();
                configure_peer(&peer).unwrap();
                let mut buf = [0; super::super::URL_MAX];
                let size = read_address(&mut peer, &mut buf).unwrap();
                assert_eq!(&buf[..size], url.as_bytes());
                peer.write_all(&[StartupStatus::ObserverScope as u8]).unwrap();
            });
            let error = socket
                .start(
                    url,
                    |pid| {
                        assert_eq!(pid, std::process::id());
                        assert!(!path.exists(), "an accepted socket must not remain discoverable");
                        Ok(())
                    },
                    |_| panic!("a failed helper cannot be published"),
                )
                .unwrap_err();
            assert!(error.contains("local working terminal") && !error.contains(url));
            child.join().unwrap();
        });
    }

    #[test]
    fn refused_socket_peer_receives_no_address() {
        let socket = StartupSocket::new().unwrap();
        let path = PathBuf::from(socket.path());
        std::thread::scope(|scope| {
            let path = &path;
            let peer = scope.spawn(move || {
                let mut peer = UnixStream::connect(path).unwrap();
                peer.set_read_timeout(Some(STARTUP_WAIT)).unwrap();
                assert_eq!(peer.read(&mut [0]).unwrap(), 0);
            });
            assert_eq!(
                socket.start("http://localhost/a", |_| Err("wrong terminal".into()), |_| unreachable!()).unwrap_err(),
                "wrong terminal"
            );
            peer.join().unwrap();
        });
    }

    #[test]
    fn readiness_keeps_the_peer_until_publication_is_confirmed() {
        for published in [true, false] {
            let socket = StartupSocket::new().unwrap();
            let path = PathBuf::from(socket.path());
            std::thread::scope(|scope| {
                let peer = scope.spawn(|| {
                    let mut peer = UnixStream::connect(&path).unwrap();
                    configure_peer(&peer).unwrap();
                    read_address(&mut peer, &mut [0; super::super::URL_MAX]).unwrap();
                    peer.write_all(&[StartupStatus::Ready as u8]).unwrap();
                    let mut confirmation = [0];
                    if published {
                        peer.read_exact(&mut confirmation).unwrap();
                        assert_eq!(confirmation, *b"P");
                    } else {
                        assert_eq!(peer.read(&mut confirmation).unwrap(), 0);
                    }
                });
                let result = socket.start(
                    "http://localhost/a",
                    |_| Ok(()),
                    |_| if published { Ok(()) } else { Err("publication unconfirmed".into()) },
                );
                assert_eq!(result.is_ok(), published);
                peer.join().unwrap();
            });
        }
    }

    #[test]
    fn a_helper_that_exits_during_startup_cannot_start_the_observer() {
        let mut helper = Command::new("/usr/bin/true").stdout(Stdio::piped()).spawn().unwrap();
        assert!(after_clock_ready(&mut helper).is_err());
        helper.wait().unwrap();
    }

    #[test]
    fn pin_success_requires_a_single_canonical_window_id() {
        assert_eq!(parse_pin_reply(Some(0), b"4294967295\n").unwrap().get(), u32::MAX);
        for reply in [
            b"".as_slice(),
            b"42",
            b"0\n",
            b"042\n",
            b"+42\n",
            b"42 \n",
            b"42\n43\n",
            b"4294967296\n",
            b"refused: another window overlaps Rex\n",
        ] {
            assert_eq!(parse_pin_reply(Some(0), reply).unwrap_err(), INVALID_PIN_SUCCESS);
        }
    }

    #[test]
    fn pin_refusal_requires_the_refusal_status_and_bounded_printable_ascii() {
        let refusal = b"refused: another window overlaps Rex\n";
        assert_eq!(
            parse_pin_reply(Some(PIN_REFUSED), refusal).unwrap_err(),
            "Preview could not pin a Rex window: another window overlaps Rex. No pane was opened."
        );
        for code in [None, Some(1)] {
            assert_eq!(parse_pin_reply(code, refusal).unwrap_err(), ABNORMAL_PIN_EXIT);
        }
        for reply in [
            b"42\n".as_slice(),
            b"refused: \n",
            b"refused:  overlap\n",
            b"refused: overlap \n",
            b"refused: overlap",
            b"refused: overlap\r\n",
            b"refused: overlap\nextra\n",
            b"refused: \x1b[2J\n",
            b"refused: \xff\n",
        ] {
            assert_eq!(parse_pin_reply(Some(PIN_REFUSED), reply).unwrap_err(), ABNORMAL_PIN_EXIT);
        }
        let max = format!("refused: {}\n", "x".repeat(PIN_REASON_MAX));
        assert_eq!(max.len(), PIN_REPLY_MAX);
        assert!(parse_pin_reply(Some(PIN_REFUSED), max.as_bytes()).unwrap_err().contains(&"x".repeat(PIN_REASON_MAX)));
        let over_max = format!("refused: {}\n", "x".repeat(PIN_REASON_MAX + 1));
        assert_eq!(parse_pin_reply(Some(PIN_REFUSED), over_max.as_bytes()).unwrap_err(), ABNORMAL_PIN_EXIT);
    }

    #[test]
    fn slow_focus_acknowledges_only_after_the_rpc_approves() {
        let (reader, reply) = focus_pipe().unwrap();
        let monitor = reader.try_clone().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", r#"printf 'Rfocus\n'; test "$(/usr/bin/head -c 1 "/dev/fd/$MILO_PREVIEW_FOCUS_FD")" = 1"#])
            .stdout(Stdio::piped());
        inherit_focus_reader(&mut command, reader);
        let mut helper = command.spawn().unwrap();
        drop(command);
        after_clock_ready(&mut helper).unwrap();
        let mut attempts = 0;
        serve_actions(helper, reply, || {
            attempts += 1;
            let mut fds = [libc::pollfd { fd: monitor.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            // Simulate a Rex RPC that takes long enough for the child to wait on the pipe.
            assert_eq!(crate::term::wait_ready(&mut fds, 25), 0);
            true
        })
        .unwrap();
        assert_eq!(attempts, 1);
    }

    #[test]
    fn refused_focus_preserves_the_helper_for_a_later_explicit_return() {
        let (reader, reply) = focus_pipe().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", r#"
                for expected in 0 1; do
                    printf 'focus\n'
                    test "$(/usr/bin/head -c 1 "/dev/fd/$MILO_PREVIEW_FOCUS_FD")" = "$expected" || exit 1
                done
            "#])
            .stdout(Stdio::piped());
        inherit_focus_reader(&mut command, reader);
        let helper = command.spawn().unwrap();
        drop(command);
        let mut attempts = 0;
        serve_actions(helper, reply, || {
            attempts += 1;
            attempts == 2
        })
        .unwrap();
        assert_eq!(attempts, 2);
    }

    #[test]
    fn helper_can_end_during_a_successful_focus_rpc() {
        let helper = Command::new("/bin/sh").args(["-c", "printf 'focus\\n'"]).stdout(Stdio::piped()).spawn().unwrap();
        let (reader, reply) = focus_pipe().unwrap();
        drop(reader);
        serve_actions(helper, reply, || true).unwrap();
    }
}
