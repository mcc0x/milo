//! The one background process that sets the Discord activity.
//!
//! It runs only while presence is on. It sleeps until Rex says an app opened
//! or left a session, Discord says something, or it is asked to stop. The
//! only timer is the wait before looking for a missing Discord again, and
//! that runs only while a Rex app has a session open.

use std::ffi::OsStr;
use std::os::fd::AsRawFd as _;
use std::time::{Duration, Instant};

use super::discord::{self, Connection, Ended, Failure};
use super::{Control, Phase};
use crate::rex::{AppActivity, AppObserver};
use crate::setup::lock;
use crate::term;

/// How long to wait before looking for Discord again.
const RETRY: Duration = Duration::from_secs(10);
/// How long a helper that is being replaced is given to let go of the lock.
const TAKEOVER: Duration = Duration::from_secs(3);

/// Leaves the process that started the helper free to go on, then runs
/// until Rex stops or presence is turned off. `rex` is the program to watch Rex with.
pub(super) fn run(rex: &OsStr) -> i32 {
    let Some(control) = Control::real() else { return 1 };
    // While presence is off, the helper ends here, having only read the choice.
    if !control.setting().is_ok_and(|setting| setting.enabled) {
        return 0;
    }
    // SAFETY: Nothing has started a thread yet, so the child is a whole copy of this process.
    match unsafe { libc::fork() } {
        0 => {}
        -1 => return 1,
        _ => return 0,
    }
    // SAFETY: Starting a session touches no memory. It keeps a closing terminal from ending the helper.
    unsafe { libc::setsid() };
    // Do not keep the folder Milo was started in busy.
    let _ = std::env::set_current_dir("/");
    let Ok(stop) = term::termination() else { return 1 };
    // One helper at a time, so that two never set the activity in turn.
    let Ok(Some(_lock)) = lock::hold(&control.state, lock::PRESENCE_LOCK, TAKEOVER) else { return 0 };
    // A helper that was killed left its last note behind.
    control.forget_phase();
    // Read the choice again under the lock: turning presence off stops the lock's holder,
    // and a helper that takes the lock afterwards must find it off.
    let Ok(setting) = control.setting() else { return 1 };
    let Some(application) = setting.application.filter(|_| setting.enabled) else { return 0 };
    let Some(observer) = AppObserver::start(rex) else { return 1 };
    let socket_dir = discord::socket_dir(|name| std::env::var_os(name));
    let mut helper = Helper {
        control: &control,
        application,
        socket_dir: &socket_dir,
        connection: None,
        retry: Some(Instant::now()),
        phase: None,
    };
    helper.watch(observer, stop.as_raw_fd());
    if let Some(connection) = helper.connection.take() {
        connection.clear();
    }
    control.forget_phase();
    0
}

struct Helper<'a> {
    control: &'a Control,
    application: u64,
    socket_dir: &'a std::path::Path,
    connection: Option<Connection>,
    /// When to look for Discord next. `None` once Discord has refused, until the helper is replaced.
    retry: Option<Instant>,
    phase: Option<Phase>,
}

impl Helper<'_> {
    fn watch(&mut self, mut observer: AppObserver, stop: i32) {
        // Unknown until Rex first answers, so that nothing is noted or shown on a guess.
        let mut active = None;
        while !term::TERMINATED.load(std::sync::atomic::Ordering::Relaxed) {
            match active {
                Some(true) => self.show(),
                Some(false) => self.hide(),
                None => {}
            }
            let wait = match (&self.connection, self.retry) {
                (None, Some(at)) if active == Some(true) => {
                    term::milliseconds(at.saturating_duration_since(Instant::now()))
                }
                _ => -1,
            };
            let discord = self.connection.as_ref().map_or(-1, Connection::fd);
            let mut fds =
                [stop, observer.fd(), discord].map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
            if term::wait_ready(&mut fds, wait) <= 0 {
                // A timeout, or a signal that the loop condition sees.
                continue;
            }
            if fds[0].revents != 0 {
                return;
            }
            if fds[1].revents != 0 {
                match observer.read() {
                    AppActivity::Active => active = Some(true),
                    AppActivity::Idle => active = Some(false),
                    AppActivity::Unchanged => {}
                    AppActivity::Closed => return,
                }
            }
            if fds[2].revents != 0 {
                self.listen();
            }
        }
    }

    /// Puts the activity up, if it is not up and Discord may be asked.
    fn show(&mut self) {
        if self.connection.is_some() || self.retry.is_none_or(|at| Instant::now() < at) {
            return;
        }
        let opened = Connection::open(self.socket_dir, self.application).and_then(|mut connection| {
            connection.set_activity().map_err(|_| Failure::Missing)?;
            Ok(connection)
        });
        match opened {
            Ok(connection) => {
                self.connection = Some(connection);
                self.note(Phase::Showing);
            }
            Err(Failure::Missing) => {
                self.retry = Some(Instant::now() + RETRY);
                self.note(Phase::Waiting);
            }
            Err(Failure::UnknownApplication) => {
                self.retry = None;
                self.note(Phase::Refused);
            }
        }
    }

    /// Takes the activity down and lets go of Discord.
    fn hide(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.clear();
        }
        if self.retry.is_some() {
            // Look for Discord at once when a Rex app next opens a session.
            self.retry = Some(Instant::now());
            self.note(Phase::Idle);
        }
    }

    fn listen(&mut self) {
        let Some(connection) = self.connection.as_mut() else { return };
        match connection.receive() {
            Ok(()) => {}
            Err(Ended::Lost) => {
                self.connection = None;
                self.retry = Some(Instant::now() + RETRY);
                self.note(Phase::Waiting);
            }
            Err(Ended::Rejected) => {
                if let Some(connection) = self.connection.take() {
                    connection.clear();
                }
                self.retry = None;
                self.note(Phase::Rejected);
            }
        }
    }

    fn note(&mut self, phase: Phase) {
        if self.phase != Some(phase) {
            self.phase = Some(phase);
            self.control.note_phase(phase);
        }
    }
}
