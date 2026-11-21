//! The mirroring process: follows Rex's event stream and sends each alert once.
//!
//! One process mirrors at a time, whatever number of tabs, panes and Rex
//! apps are open, because it holds a lock for as long as it lives. It only
//! listens to Rex, so Rex's own notifications are neither delayed nor changed.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, Read as _};
use std::path::Path;
use std::process::Child;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::event::{self, Alert, Seen};
use super::send::{self, Outcome};
use super::{Alerts, Settings, message};
use crate::setup::lock;
use crate::{preferences, rex};

/// The longest line of the stream that is read. Rex's largest alert is far smaller.
const LINE_LIMIT: u64 = 1 << 16;
/// Alerts waiting while a message is being sent. More than this are dropped.
const WAITING: usize = 64;
/// How often the settings are read again while nothing happens.
const TICK: Duration = Duration::from_secs(2);
/// Both destinations ask for about a message a second at most to one place.
const SPACING: Duration = Duration::from_millis(1100);
/// Telegram allows a bot twenty messages a minute in a group.
const PER_MINUTE: usize = 20;
const MINUTE: Duration = Duration::from_secs(60);
const ATTEMPTS: u32 = 3;
/// A destination that asks to wait longer than this loses the alert instead.
const LONGEST_WAIT: Duration = Duration::from_secs(60);
/// Messages in a row a destination may turn down, each for a reason of its
/// own, before it is no longer asked.
const REFUSALS: u32 = 5;
/// How long Rex may stay unreachable before the process gives up. Rex's
/// listener starts another when the server next loads its settings.
const GIVE_UP: Duration = Duration::from_secs(600);
const SLOWEST_RETRY: Duration = Duration::from_secs(30);
const STATUS_LIMIT: u64 = 1 << 10;

/// The programs to end with this process: `rex events`, then `curl`. Each
/// slot is cleared before its program is waited for.
pub(super) static CHILDREN: [AtomicI32; 2] = [AtomicI32::new(0), AtomicI32::new(0)];

/// Ends the stream and any request in flight, then this process, at once.
extern "C" fn stop(_: libc::c_int) {
    for child in &CHILDREN {
        let pid = child.load(Ordering::Relaxed);
        if pid > 1 {
            // SAFETY: The ID is that of a child this process started and has
            // not waited for when the slot was read. The only race is a wait
            // that finishes, and the ID being used again, before the signal.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
    // SAFETY: Ending the process without running destructors is safe from a signal handler.
    unsafe { libc::_exit(0) };
}

fn stop_on(signal: libc::c_int) {
    // SAFETY: A zeroed sigaction has no flags and no handler; both the handler
    // and the mask are filled in before it is passed to sigaction.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = stop as *const () as usize;
    // SAFETY: The mask is writable storage for an empty signal set.
    unsafe { libc::sigemptyset(&raw mut action.sa_mask) };
    // SAFETY: `action` holds the handler and its mask; no old action is requested.
    unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) };
}

/// Why following the stream ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// Mirroring was turned off, or can no longer be done.
    Off,
    /// The settings changed and must be read again.
    Changed,
    /// Rex closed the stream or could not be reached.
    Stream,
}

/// How long Rex has been away, across attempts to reach it.
pub(super) struct Away {
    since: Option<Instant>,
    retry: Duration,
}

impl Away {
    pub(super) fn new() -> Self {
        Self { since: None, retry: Duration::from_secs(1) }
    }

    /// How long to wait before reaching for Rex again, after a stream that
    /// `lasted` so long ended at `now`. None once Rex has been away too long.
    pub(super) fn wait(&mut self, lasted: Duration, now: Instant) -> Option<Duration> {
        if lasted >= MINUTE {
            *self = Self::new();
        }
        if now.duration_since(*self.since.get_or_insert(now)) >= GIVE_UP {
            return None;
        }
        let wait = self.retry;
        self.retry = (wait * 2).min(SLOWEST_RETRY);
        Some(wait)
    }
}

/// Runs until mirroring is turned off or Rex stays away. Returns at once if
/// another process is already mirroring.
pub(crate) fn run(alerts: &Alerts) -> io::Result<()> {
    let Some(_lock) = lock::hold_alerts(alerts.state_dir())? else { return Ok(()) };
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        stop_on(signal);
    }
    let mut forwarder = Forwarder::new(alerts);
    let mut away = Away::new();
    loop {
        let began = Instant::now();
        match forwarder.follow() {
            Ended::Off => return Ok(()),
            Ended::Changed => continue,
            Ended::Stream => {}
        }
        let Some(wait) = away.wait(began.elapsed(), Instant::now()) else {
            forwarder.report("Rex could not be reached, so mirroring stopped. It starts again with Rex.");
            return Ok(());
        };
        forwarder.report("Waiting for Rex. Alerts raised meanwhile are not mirrored.");
        std::thread::sleep(wait);
    }
}

struct Forwarder<'a> {
    alerts: &'a Alerts,
    /// When each of the last minute's messages was sent.
    sent_at: VecDeque<Instant>,
    sent: u32,
    /// Alerts dropped since the last message that was sent.
    unsent: u32,
    /// Until when the destination asked not to be sent anything.
    not_before: Option<Instant>,
    /// Messages in a row the destination turned down.
    refusals: u32,
    /// Set when the destination refuses the secret or keeps refusing
    /// messages. Asking again and again could have the bot or this address
    /// banned, so nothing more is sent until this process is started again.
    refused: bool,
}

impl<'a> Forwarder<'a> {
    fn new(alerts: &'a Alerts) -> Self {
        Self { alerts, sent_at: VecDeque::new(), sent: 0, unsent: 0, not_before: None, refusals: 0, refused: false }
    }

    /// The settings, while they still ask for mirroring.
    fn wanted(&self) -> Option<Settings> {
        self.alerts.settings().ok().filter(|settings| settings.enabled && settings.destination.is_some())
    }

    /// How following must end if the settings are no longer `settings`.
    fn changed(&self, settings: &Settings) -> Option<Ended> {
        match self.wanted() {
            Some(now) if now == *settings => None,
            Some(_) => Some(Ended::Changed),
            None => Some(Ended::Off),
        }
    }

    fn follow(&mut self) -> Ended {
        let Some(settings) = self.wanted() else { return Ended::Off };
        let Some(secret) = settings.destination.and_then(|destination| self.alerts.secrets.get(destination).ok()?)
        else {
            self.report("Stopped: the destination's secret is no longer saved.");
            return Ended::Off;
        };
        let Ok(mut stream) = rex::events() else { return Ended::Stream };
        CHILDREN[0].store(stream.id().try_into().unwrap_or(0), Ordering::Relaxed);
        if !self.refused {
            self.report("Listening for Rex alerts.");
        }
        let ended = self.mirror(&mut stream, &settings, &secret);
        CHILDREN[0].store(0, Ordering::Relaxed);
        let _ = stream.wait();
        ended
    }

    /// Reads the stream on a thread of its own, so that a slow destination
    /// never keeps Rex waiting to write. That thread also decides what is an
    /// alert, as each event arrives, so that a wait here cannot lose the
    /// reports in between that tell a new alert from a repeated one.
    fn mirror(&mut self, stream: &mut Child, settings: &Settings, secret: &str) -> Ended {
        let Some(output) = stream.stdout.take() else { return Ended::Stream };
        let (alerts, waiting) = mpsc::sync_channel(WAITING);
        let dropped = &AtomicU32::new(0);
        std::thread::scope(|scope| {
            // The reader owns the sending end, so that the end of the stream is seen here.
            scope.spawn(move || read(output, settings.kinds, &alerts, dropped));
            let ended = self.pump(&waiting, dropped, settings, secret);
            // The reader ends with the stream, so end the stream first.
            let _ = stream.kill();
            ended
        })
    }

    fn pump(&mut self, waiting: &Receiver<Alert>, dropped: &AtomicU32, settings: &Settings, secret: &str) -> Ended {
        loop {
            let alert = match waiting.recv_timeout(TICK) {
                Ok(alert) => alert,
                Err(RecvTimeoutError::Disconnected) => return Ended::Stream,
                Err(RecvTimeoutError::Timeout) => match self.changed(settings) {
                    Some(ended) => return ended,
                    None => continue,
                },
            };
            self.unsent += dropped.swap(0, Ordering::Relaxed);
            if let Some(ended) = self.deliver(&alert, settings, secret) {
                return ended;
            }
        }
    }

    /// Sends one alert. Says how following must end if, meanwhile, mirroring
    /// was turned off or its settings changed.
    fn deliver(&mut self, alert: &Alert, settings: &Settings, secret: &str) -> Option<Ended> {
        let destination = settings.destination?;
        if self.refused || !self.within_rate() {
            self.unsent += 1;
            return None;
        }
        let text = message::text(alert, settings.content, self.unsent, destination);
        let body = message::body(destination, &settings.telegram_chat, &text);
        let address = self.alerts.address(destination, secret);
        for attempt in 0..ATTEMPTS {
            // Checked before every request, so that turning mirroring off
            // holds even if this process was not told to stop.
            if let Some(ended) = self.changed(settings) {
                return Some(ended);
            }
            let outcome = send::post(&address, &body, Some(&CHILDREN[1]));
            let wait = match outcome {
                Outcome::Sent => {
                    self.sent_at.push_back(Instant::now());
                    (self.sent, self.unsent, self.refusals) = (self.sent + 1, 0, 0);
                    self.report("The last alert was delivered.");
                    return None;
                }
                Outcome::Refused(code) => {
                    self.unsent += 1;
                    self.refusals += 1;
                    let reason = outcome.reason(destination);
                    if matches!(code, 401 | 403 | 404) || self.refusals >= REFUSALS {
                        self.refused = true;
                        self.report(&format!(
                            "Paused: {reason}. Mirroring resumes when the destination is saved again or milo alerts \
                             on is run."
                        ));
                    } else {
                        self.report(&format!("The last alert was not delivered: {reason}."));
                    }
                    return None;
                }
                Outcome::Wait(wait) => {
                    self.not_before = Some(Instant::now() + wait);
                    if wait > LONGEST_WAIT {
                        break;
                    }
                    wait
                }
                Outcome::Failed(_) => Duration::from_secs(1) * 3_u32.pow(attempt),
            };
            self.report(&format!("Trying again: {}.", outcome.reason(destination)));
            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(wait);
            }
        }
        self.unsent += 1;
        self.report("The last alert could not be delivered and was dropped.");
        None
    }

    /// Whether another message may be sent now. Waits out the gap between
    /// two messages. An alert that comes while the destination asked for
    /// quiet, or past the minute's allowance, is not sent at all.
    fn within_rate(&mut self) -> bool {
        let now = Instant::now();
        if self.not_before.is_some_and(|until| now < until) {
            return false;
        }
        while self.sent_at.front().is_some_and(|&at| now.duration_since(at) >= MINUTE) {
            self.sent_at.pop_front();
        }
        if self.sent_at.len() >= PER_MINUTE {
            return false;
        }
        if let Some(gap) = self.sent_at.back().and_then(|&at| SPACING.checked_sub(now.duration_since(at))) {
            std::thread::sleep(gap);
        }
        true
    }

    /// Notes for `milo alerts` what the process last did. Never an alert's text.
    fn report(&self, note: &str) {
        let mut text = format!("{note}\n");
        if self.sent > 0 || self.unsent > 0 {
            let _ =
                writeln!(text, "Delivered since it started: {}. Not sent since the last: {}.", self.sent, self.unsent);
        }
        let file = self.alerts.state_dir().join("alerts.status");
        let _ = preferences::locked(&file, |dir| preferences::replace(dir, &file, &text));
    }
}

/// What the mirroring process last noted, or nothing.
pub(crate) fn note(state: &Path) -> String {
    preferences::read(&state.join("alerts.status"), STATUS_LIMIT).ok().flatten().unwrap_or_default().trim().to_owned()
}

pub(crate) fn forget_note(state: &Path) {
    let _ = std::fs::remove_file(state.join("alerts.status"));
}

/// Passes on each alert of a kind in `mirrors` until the stream ends. A line
/// too long to be an alert is skipped whole. What each terminal last said is
/// kept only for this stream: Rex repeats nothing to a listener that joins
/// late, so after the stream is opened again nothing remembered can be trusted.
pub(super) fn read(stream: impl io::Read, mirrors: [bool; 5], alerts: &mpsc::SyncSender<Alert>, dropped: &AtomicU32) {
    let mut stream = BufReader::new(stream);
    let mut seen = Seen::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        match stream.by_ref().take(LINE_LIMIT).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(read) if read as u64 == LINE_LIMIT && line.last() != Some(&b'\n') => {
                if stream.skip_until(b'\n').is_err() {
                    return;
                }
            }
            Ok(_) => {
                let alert = event::parse(line.trim_ascii_end()).and_then(|event| seen.alert(event, Instant::now()));
                if let Some(alert) = alert.filter(|alert| mirrors[alert.kind as usize])
                    && alerts.try_send(alert).is_err()
                {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}
