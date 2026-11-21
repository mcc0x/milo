//! Opens the settings editor of another Rex host on this computer.
//!
//! The editor keeps running on the host, listening on the host's loopback
//! address. A Rex forward listens on this computer's loopback address and
//! carries each connection there. The host's editor is told that address
//! before it starts and accepts no other, so the page, its origin and its
//! session key are checked exactly as they are for a local editor.

use std::ffi::OsStr;
use std::io::{self, PipeReader, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::process::Child;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::launch::Launch;
use super::{FORWARD_TARGET, STOP};
use crate::rex::{self, RemoteEditor};

/// How long the host's editor may take to print its link.
const START_WAIT: Duration = Duration::from_secs(15);
/// How long the forward may take to listen.
const FORWARD_WAIT: Duration = Duration::from_secs(10);
const CHECK: Duration = Duration::from_millis(200);
const TOKEN_LEN: usize = 64;

pub(super) struct Request<'a> {
    pub host: &'a OsStr,
    /// A folder on the host to open the editor for.
    pub project: Option<&'a OsStr>,
    pub open_browser: bool,
    /// The port to listen on here, or 0 for a free one.
    pub port: u16,
}

/// The editor on the host and what carries it here. Dropping it ends both,
/// whichever way `run` leaves.
struct Carried {
    editor: RemoteEditor,
    host: String,
    forward: Option<Child>,
    watch: Option<Child>,
}

impl Drop for Carried {
    fn drop(&mut self) {
        if let Some(mut forward) = self.forward.take() {
            interrupt(&mut forward);
        }
        if let Some(mut watch) = self.watch.take() {
            let _ = watch.kill();
            let _ = watch.wait();
        }
        // Ending the session ends the editor and removes the forward's block with it.
        if self.editor.end() {
            eprintln!("milo web: closed the forward and the editor on {}", self.host);
        } else {
            eprintln!(
                "milo web: closed the forward, but {} did not confirm that its editor ended. \
                 It ends on its own after 15 minutes without use; `rex -S {} ls` shows a session named milo-settings until then.",
                self.host, self.host
            );
        }
    }
}

/// Lets `rex forward` close its own block, and ends it if it does not.
fn interrupt(child: &mut Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: The child has not been waited for, so its ID still names it.
        unsafe { libc::kill(pid, libc::SIGINT) };
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The session key and the host's port, from the two lines the host's editor
/// printed. The link must be the one this computer asked for, to the letter.
fn handoff(screen: &str, local_port: u16) -> Option<(&str, u16)> {
    let prefix = format!("http://127.0.0.1:{local_port}/#");
    let mut lines = screen.lines().map(str::trim_end);
    let token = lines.find_map(|line| {
        line.strip_prefix(&prefix)
            .filter(|token| token.len() == TOKEN_LEN && token.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    })?;
    let port = lines.find_map(|line| line.strip_prefix(FORWARD_TARGET))?.parse().ok().filter(|&port| port != 0)?;
    Some((token, port))
}

/// A loopback port nothing is listening on at this moment.
fn local_port(wanted: u16) -> Result<u16, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, wanted))
        .map_err(|error| format!("could not listen on 127.0.0.1:{wanted}: {error}; use --port 0 for a free port"))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| format!("could not read the loopback address: {error}"))
}

/// Waits without missing a signal: the wake pipe closes when one arrives.
fn pause(wake: &PipeReader, wait: Duration) -> bool {
    let mut fds = [libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
    crate::term::wait_ready(&mut fds, crate::term::milliseconds(wait));
    !STOP.load(Ordering::Relaxed)
}

fn listening(port: u16) -> bool {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok()
}

pub(super) fn run(request: &Request<'_>, wake: &PipeReader) -> Result<(), String> {
    let host = request.host.to_string_lossy().into_owned();
    let there = rex::server(Some(request.host)).ok_or_else(|| {
        format!("could not reach a Rex server at {host}. `rex hosts check` shows which hosts answer from here")
    })?;
    if rex::server(None).is_some_and(|here| here.host_id == there.host_id) {
        return Err(format!("{host} is this computer's own Rex server; run milo web without --host"));
    }
    eprintln!("milo web: {host} runs Rex {} (API {}); starting its settings editor…", there.version, there.api);
    let port = local_port(request.port)?;
    let editor = RemoteEditor::start(request.host, request.project, port).ok_or_else(|| {
        format!("could not start a session on {host}. Check the folder given with --project and that the host allows new sessions")
    })?;
    let mut carried = Carried { editor, host: host.clone(), forward: None, watch: None };

    let started = Instant::now();
    let (token, host_port) = loop {
        if let Some(screen) = carried.editor.screen()
            && let Some((token, host_port)) = handoff(&screen, port)
        {
            break (token.to_owned(), host_port);
        }
        if started.elapsed() >= START_WAIT {
            return Err(format!(
                "Milo on {host} did not start its editor. Check that this version of milo is installed there and on the search path of its login shell"
            ));
        }
        if !pause(wake, CHECK) {
            return Ok(());
        }
    };

    let failed = |error: io::Error| format!("could not start the Rex forward: {error}");
    carried.forward = Some(carried.editor.forward(port, host_port).map_err(failed)?);
    carried.watch = Some(carried.editor.watch().map_err(failed)?);
    let started = Instant::now();
    while !listening(port) {
        let ended = carried.forward.as_mut().is_some_and(|forward| !matches!(forward.try_wait(), Ok(None)));
        if ended || started.elapsed() >= FORWARD_WAIT {
            return Err(format!("the Rex forward to {host} did not start listening on 127.0.0.1:{port}"));
        }
        if !pause(wake, CHECK) {
            return Ok(());
        }
    }

    let url = format!("http://127.0.0.1:{port}/#{token}");
    {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{url}")
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("could not print the editor URL: {error}"))?;
    }
    eprintln!(
        "milo web: these are the settings of {host}, carried over a Rex forward on 127.0.0.1:{port}. \
         Choose Done in the page or press Ctrl-C here to close both."
    );
    let mut launcher = if request.open_browser {
        Launch::start(&url)
            .inspect_err(|error| eprintln!("milo web: could not open the browser ({error}); open the URL above"))
            .ok()
    } else {
        None
    };

    while pause(wake, CHECK) {
        if launcher.as_mut().is_some_and(|launch| launch.poll(Instant::now()).is_none()) {
            launcher = None;
        }
        let ended =
            |child: &mut Option<Child>| child.as_mut().is_some_and(|child| !matches!(child.try_wait(), Ok(None)));
        if ended(&mut carried.watch) {
            eprintln!("milo web: the editor on {host} ended");
            break;
        }
        if ended(&mut carried.forward) {
            eprintln!("milo web: the Rex forward to {host} ended");
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FORWARD_TARGET, handoff};

    #[test]
    fn only_the_link_this_computer_asked_for_is_taken_from_the_hosts_terminal() {
        let token = "0123456789abcdef".repeat(4);
        let screen = format!("http://127.0.0.1:7001/#{token}\n{FORWARD_TARGET}51234\n");
        assert_eq!(handoff(&screen, 7001), Some((token.as_str(), 51234)));
        // A link to another port, another host or with another key shape is not ours.
        assert_eq!(handoff(&screen, 7002), None);
        assert_eq!(handoff(&screen.replace("127.0.0.1:7001", "evil.example:7001"), 7001), None);
        assert_eq!(handoff(&screen.replace(&token, &token[1..]), 7001), None);
        assert_eq!(handoff(&screen.replace(&token, &token.to_uppercase()), 7001), None);
        // The port line must follow the link, and name a real port.
        assert_eq!(handoff(&format!("{FORWARD_TARGET}51234\nhttp://127.0.0.1:7001/#{token}\n"), 7001), None);
        assert_eq!(handoff(&screen.replace("51234", "0"), 7001), None);
        assert_eq!(handoff(&screen.replace("51234", "99999"), 7001), None);
        assert_eq!(handoff(&format!("http://127.0.0.1:7001/#{token}\n"), 7001), None);
    }
}
