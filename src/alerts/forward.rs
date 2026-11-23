//! Follows Rex's event stream and sends alerts to each enabled destination.
//!
//! One process mirrors at a time, whatever number of tabs, panes and Rex
//! apps are open, because it holds a lock for as long as it lives. It only
//! listens to Rex, so Rex's own notifications are neither delayed nor changed.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::event::{self, Alert, Seen};
use super::send::{self, Outcome};
use super::{Alerts, Destination, Settings, message, origin, secret};
use crate::setup::lock;
use crate::{preferences, rex};

/// The longest line of the stream that is read. Rex's largest alert is far smaller.
const LINE_LIMIT: u64 = 1 << 16;
/// Alerts waiting for each destination. More than this are dropped for it alone.
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

/// The programs to end with this process: `rex events`, then each sender's
/// `curl`. Each slot is cleared before its program is waited for.
static CHILDREN: [AtomicI32; 3] = [const { AtomicI32::new(0) }; 3];

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
    let status = Status { file: alerts.state_dir().join("alerts.status"), notes: Mutex::default() };
    let mut deliveries = Destination::ALL.map(|_| Delivery::default());
    loop {
        let Some(settings) = wanted(alerts) else { return Ok(()) };
        if mirror(alerts, &status, &settings, &mut deliveries) != Ended::Changed {
            return Ok(());
        }
    }
}

fn wanted(alerts: &Alerts) -> Option<Settings> {
    alerts
        .settings()
        .ok()
        .filter(|settings| settings.enabled && Destination::ALL.into_iter().any(|destination| settings.active(destination)))
}

fn changed(alerts: &Alerts, settings: &Settings) -> Option<Ended> {
    let Some(now) = wanted(alerts) else { return Some(Ended::Off) };
    let same = Destination::ALL.into_iter().all(|destination| now.active(destination) == settings.active(destination))
        && now.kinds == settings.kinds
        && now.content == settings.content
        && now.origin == settings.origin
        && (!now.active(Destination::Telegram) || now.telegram_chat == settings.telegram_chat);
    (!same).then_some(Ended::Changed)
}

/// Each worker has a queue that survives Rex reconnects. A destination's
/// retries or rate limits cannot delay the reader or the other destination.
fn mirror(alerts: &Alerts, status: &Status, settings: &Settings, deliveries: &mut [Delivery; 2]) -> Ended {
    let secrets = Destination::ALL.map(|destination| {
        if !settings.active(destination) {
            return None;
        }
        match alerts.target_secret(settings, destination) {
            Ok(secret) => Some(secret),
            Err(error) => {
                deliveries[destination as usize].report(status, destination, &format!("Stopped: {error}"));
                None
            }
        }
    });
    if secrets.iter().all(Option::is_none) {
        return Ended::Off;
    }
    let dropped = Destination::ALL.map(|_| AtomicU32::new(0));
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let mut senders = [None, None];
        let mut workers = [None, None];
        for ((destination, delivery), secret) in Destination::ALL.into_iter().zip(deliveries).zip(secrets) {
            let Some(secret) = secret else { continue };
            let index = destination as usize;
            let (sender, waiting) = mpsc::sync_channel(WAITING);
            senders[index] = Some(sender);
            let hosted_telegram = destination == Destination::Telegram && secret::telegram_binding(&secret).is_some();
            let target = Target { alerts, settings, status, destination, secret, hosted_telegram, cancelled: &cancelled };
            let dropped = &dropped[index];
            workers[index] = Some(scope.spawn(move || delivery.pump(&waiting, dropped, &target)));
        }
        let ended = follow(alerts, status, settings, &senders, &dropped);
        cancelled.store(true, Ordering::Relaxed);
        drop(senders);
        for worker in workers.iter().flatten() {
            worker.thread().unpark();
        }
        ended
    })
}

fn follow(
    alerts: &Alerts,
    status: &Status,
    settings: &Settings,
    senders: &[Option<mpsc::SyncSender<Queued>>; 2],
    dropped: &[AtomicU32; 2],
) -> Ended {
    let mut away = Away::new();
    loop {
        if let Some(ended) = changed(alerts, settings) {
            return ended;
        }
        let began = Instant::now();
        let server = settings.origin.then(rex::AlertServer::local).flatten();
        if let Ok(mut stream) = rex::events(server.as_ref()) {
            CHILDREN[0].store(stream.id().try_into().unwrap_or(0), Ordering::Relaxed);
            status.stream("Listening for Rex alerts.");
            let ended = read_stream(&mut stream, alerts, settings, server.as_ref(), senders, dropped);
            CHILDREN[0].store(0, Ordering::Relaxed);
            let _ = stream.wait();
            if ended != Ended::Stream {
                return ended;
            }
        }
        let Some(wait) = away.wait(began.elapsed(), Instant::now()) else {
            status.stream("Rex could not be reached, so mirroring stopped. It starts again with Rex.");
            return Ended::Off;
        };
        status.stream("Waiting for Rex. Alerts raised meanwhile are not mirrored.");
        std::thread::sleep(wait);
    }
}

fn read_stream(
    stream: &mut Child,
    alerts: &Alerts,
    settings: &Settings,
    server: Option<&rex::AlertServer>,
    senders: &[Option<mpsc::SyncSender<Queued>>; 2],
    dropped: &[AtomicU32; 2],
) -> Ended {
    let Some(output) = stream.stdout.take() else { return Ended::Stream };
    let (reading, ended) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            read(output, settings, server, |alert| fan_out(alert, senders, dropped));
            drop(reading);
        });
        let ended = loop {
            match ended.recv_timeout(TICK) {
                Err(RecvTimeoutError::Disconnected) => break Ended::Stream,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(ended) = changed(alerts, settings) {
                        break ended;
                    }
                }
                Ok(()) => unreachable!(),
            }
        };
        // The reader ends with the stream, so end the stream before joining it.
        let _ = stream.kill();
        ended
    })
}

enum Queued {
    Only(Alert),
    Shared(Arc<Alert>),
}

fn fan_out(alert: Alert, senders: &[Option<mpsc::SyncSender<Queued>>; 2], dropped: &[AtomicU32; 2]) {
    let queue = |index: usize, sender: &mpsc::SyncSender<Queued>, alert| {
        if sender.try_send(alert).is_err() {
            dropped[index].fetch_add(1, Ordering::Relaxed);
        }
    };
    match senders {
        [Some(first), Some(second)] => {
            let alert = Arc::new(alert);
            queue(0, first, Queued::Shared(Arc::clone(&alert)));
            queue(1, second, Queued::Shared(alert));
        }
        [Some(first), None] => queue(0, first, Queued::Only(alert)),
        [None, Some(second)] => queue(1, second, Queued::Only(alert)),
        [None, None] => {}
    }
}

/// The shared configuration for one destination's worker.
struct Target<'a> {
    alerts: &'a Alerts,
    settings: &'a Settings,
    status: &'a Status,
    destination: Destination,
    secret: String,
    hosted_telegram: bool,
    cancelled: &'a AtomicBool,
}

impl Target<'_> {
    fn stopped(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn pause(&self, wait: Duration) {
        let until = Instant::now() + wait;
        while !self.stopped() {
            let Some(remaining) = until.checked_duration_since(Instant::now()) else { break };
            std::thread::park_timeout(remaining);
        }
    }
}

#[derive(Default)]
struct Delivery {
    /// When each of the last minute's messages was sent or may have been sent.
    sent_at: VecDeque<Instant>,
    sent: u32,
    /// Alerts known to have been dropped since the last confirmed delivery.
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

impl Delivery {
    fn pump(&mut self, waiting: &Receiver<Queued>, dropped: &AtomicU32, target: &Target<'_>) {
        while let Ok(queued) = waiting.recv() {
            if target.stopped() {
                break;
            }
            self.unsent += dropped.swap(0, Ordering::Relaxed);
            let alert = match &queued {
                Queued::Only(alert) => alert,
                Queued::Shared(alert) => alert.as_ref(),
            };
            self.deliver(alert, target);
        }
    }

    fn note(&self, target: &Target<'_>, note: &str) {
        self.report(target.status, target.destination, note);
    }

    fn deliver(&mut self, alert: &Alert, target: &Target<'_>) {
        let Target { alerts, settings, destination, secret, .. } = target;
        let destination = *destination;
        if self.refused || !self.within_rate(target) {
            self.unsent += 1;
            return;
        }
        let text = message::text(alert, settings.content, self.unsent, destination);
        let Some(request) = alerts.message_request(destination, secret, &settings.telegram_chat, &text, Some(alert.kind)) else {
            self.refused = true;
            self.note(target, "Stopped: the saved destination secret is invalid. Save it again.");
            return;
        };
        for attempt in 0..ATTEMPTS {
            // Recheck the settings before each attempt: the request's verified
            // Telegram chat must still match the current forwarding settings.
            if target.stopped() || changed(alerts, settings).is_some() {
                return;
            }
            let outcome = send::post(&request, Some(&CHILDREN[1 + destination as usize]));
            let wait = match outcome {
                Outcome::Sent => {
                    self.sent_at.push_back(Instant::now());
                    (self.sent, self.unsent, self.refusals) = (self.sent + 1, 0, 0);
                    self.note(target, "The last alert was delivered.");
                    return;
                }
                Outcome::Refused(code) => {
                    self.unsent += 1;
                    self.refusals += 1;
                    let reason = outcome.reason(destination, target.hosted_telegram);
                    if matches!(code, 401 | 403 | 404) || self.refusals >= REFUSALS {
                        self.refused = true;
                        let resume = if code == 401 && target.hosted_telegram {
                            ""
                        } else {
                            " Mirroring resumes when the destination is saved again or milo alerts on is run."
                        };
                        self.note(target, &format!("Paused: {reason}.{resume}"));
                    } else {
                        self.note(target, &format!("The last alert was not delivered: {reason}."));
                    }
                    return;
                }
                Outcome::Uncertain(_) => {
                    // Reserve rate-limit capacity without counting a possible delivery as missed.
                    self.sent_at.push_back(Instant::now());
                    self.refusals = 0;
                    let reason = outcome.reason(destination, target.hosted_telegram);
                    self.note(target, &format!("The last alert was not retried: {reason}."));
                    return;
                }
                Outcome::Wait(wait) | Outcome::Unavailable(wait) => {
                    self.not_before = Some(Instant::now() + wait);
                    if wait > LONGEST_WAIT {
                        break;
                    }
                    wait
                }
                Outcome::Failed(_) => Duration::from_secs(1) * 3_u32.pow(attempt),
            };
            if attempt + 1 == ATTEMPTS {
                break;
            }
            self.note(target, &format!("Trying again: {}.", outcome.reason(destination, target.hosted_telegram)));
            target.pause(wait);
        }
        self.unsent += 1;
        self.note(target, "The last alert could not be delivered and was dropped.");
    }

    /// Whether another message may be sent now. Waits out the gap between
    /// two messages. An alert that comes while the destination asked for
    /// quiet, or past the minute's allowance, is not sent at all.
    fn within_rate(&mut self, target: &Target<'_>) -> bool {
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
            target.pause(gap);
        }
        true
    }

    fn report(&self, status: &Status, destination: Destination, note: &str) {
        status.delivery(destination, note, self.sent, self.unsent);
    }
}

/// Keep one worker's note from hiding another's, using the same status file.
struct Status {
    file: PathBuf,
    notes: Mutex<Notes>,
}

#[derive(Default)]
struct Notes {
    stream: &'static str,
    providers: [String; 2],
}

impl Status {
    fn stream(&self, note: &'static str) {
        let mut notes = self.notes.lock().unwrap();
        notes.stream = note;
        self.write(&notes);
    }

    fn delivery(&self, destination: Destination, note: &str, sent: u32, unsent: u32) {
        let mut notes = self.notes.lock().unwrap();
        let note_text = &mut notes.providers[destination as usize];
        note_text.clear();
        let _ = writeln!(note_text, "{}: {note}", destination.title());
        if sent > 0 || unsent > 0 {
            let _ = writeln!(note_text, "Delivered since it started: {sent}. Not sent since the last: {unsent}.");
        }
        self.write(&notes);
    }

    fn write(&self, notes: &Notes) {
        let [telegram, discord] = &notes.providers;
        let text = if notes.stream.is_empty() {
            format!("{telegram}{discord}")
        } else {
            format!("{}\n{telegram}{discord}", notes.stream)
        };
        let _ = preferences::locked(&self.file, |dir| preferences::replace(dir, &self.file, &text));
    }
}

/// What the mirroring process last noted, or nothing.
pub(crate) fn note(state: &Path) -> String {
    preferences::read(&state.join("alerts.status"), STATUS_LIMIT).ok().flatten().unwrap_or_default().trim().to_owned()
}

pub(crate) fn forget_note(state: &Path) {
    let _ = std::fs::remove_file(state.join("alerts.status"));
}

/// Passes on each chosen kind with only the chosen content. A line
/// too long to be an alert is skipped whole. What each terminal last said is
/// kept only for this stream: Rex repeats nothing to a listener that joins
/// late, so after the stream is opened again nothing remembered can be trusted.
pub(super) fn read(
    stream: impl io::Read,
    settings: &Settings,
    server: Option<&rex::AlertServer>,
    mut queue: impl FnMut(Alert),
) {
    let mut stream = BufReader::new(stream);
    let mut seen = Seen::default();
    let server = server.filter(|_| settings.origin);
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
                let Some(envelope) = event::Envelope::read(line.trim_ascii_end()) else { continue };
                let alert =
                    event::parse(&envelope, settings.content).and_then(|event| seen.alert(event, Instant::now()));
                if let Some(mut alert) = alert.filter(|alert| settings.mirrors(alert.kind)) {
                    if let Some(server) = server {
                        alert.origin = origin::label(&envelope, &server.host, |id| server.session(id));
                    }
                    queue(alert);
                }
            }
        }
    }
}

#[cfg(test)]
#[test]
fn a_full_queue_drops_only_that_destinations_copy() {
    use super::event::Kind;

    let (telegram, _telegram_waiting) = mpsc::sync_channel(1);
    let (discord, discord_waiting) = mpsc::sync_channel(2);
    let senders = [Some(telegram), Some(discord)];
    let dropped = Destination::ALL.map(|_| AtomicU32::new(0));
    for kind in [Kind::Bell, Kind::Blocked] {
        fan_out(Alert { kind, waiting: None, app: None, text: String::new(), origin: None }, &senders, &dropped);
    }
    let kinds: Vec<_> = discord_waiting
        .try_iter()
        .map(|queued| match queued {
            Queued::Shared(alert) => alert.kind,
            Queued::Only(alert) => alert.kind,
        })
        .collect();
    assert_eq!(kinds, [Kind::Bell, Kind::Blocked]);
    assert_eq!(dropped.map(AtomicU32::into_inner), [1, 0]);
}

#[cfg(test)]
#[test]
fn a_relay_outage_retries_only_telegram_and_only_up_to_the_limit() {
    use super::event::Kind;
    use super::relay::{
        Origin,
        tests::{CAPABILITY, with_replies},
    };
    use crate::fixture::Fixture;

    let retry = (503, r#"{"retry_after":1}"#);
    let replies =
        [(503, r#"{"retry_after":3}"#), (204, ""), (204, ""), retry, retry, retry, (503, r#"{"retry_after":61}"#)];
    let (_, wire) = with_replies(&replies, |origin| {
        let fixture = Fixture::folder();
        let mut alerts = Alerts::at(&fixture.root.join("config"), &fixture.root.join("state"));
        alerts.relay = Some(Origin::Test(origin.to_owned()));
        alerts.stand_in = Some(origin.to_owned());
        alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
        alerts.set_discord("https://discord.com/api/webhooks/1234567891/synthetic_SYNTHETICsynthetic-01").unwrap();
        for destination in Destination::ALL {
            alerts.set_forwarding(destination, true).unwrap();
        }
        let settings = alerts.settings().unwrap();
        let status = Status { file: alerts.state_dir().join("alerts.status"), notes: Mutex::default() };
        let cancelled = AtomicBool::new(false);
        let [telegram, discord] = Destination::ALL.map(|destination| Target {
            alerts: &alerts,
            settings: &settings,
            status: &status,
            destination,
            secret: alerts.target_secret(&settings, destination).unwrap(),
            hosted_telegram: destination == Destination::Telegram,
            cancelled: &cancelled,
        });
        let alert = Alert { kind: Kind::Blocked, waiting: None, app: None, text: String::new(), origin: None };
        let mut telegram_delivery = Delivery::default();
        let began = Instant::now();
        std::thread::scope(|scope| {
            let sender = scope.spawn(|| telegram_delivery.deliver(&alert, &telegram));
            while !note(alerts.state_dir()).contains("Trying again: the Telegram relay is not ready") {
                assert!(began.elapsed() < Duration::from_secs(2), "Telegram did not begin waiting");
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut discord_delivery = Delivery::default();
            discord_delivery.deliver(&alert, &discord);
            assert_eq!(discord_delivery.sent, 1);
            sender.join().unwrap();
        });
        assert!(began.elapsed() >= Duration::from_secs(3));
        assert_eq!((telegram_delivery.sent, telegram_delivery.unsent), (1, 0));

        telegram_delivery.deliver(&alert, &telegram);
        assert_eq!((telegram_delivery.sent, telegram_delivery.unsent), (1, 1));
        assert!(note(alerts.state_dir()).contains("The last alert could not be delivered and was dropped."));
        assert!(!telegram_delivery.within_rate(&telegram));

        let mut long_wait = Delivery::default();
        long_wait.deliver(&alert, &telegram);
        assert_eq!((long_wait.sent, long_wait.unsent), (0, 1));
        assert!(!long_wait.within_rate(&telegram));
    });
    assert!(wire[0].starts_with("POST /v1/send "));
    assert!(wire[1].starts_with("POST /api/v10/webhooks/"));
    assert_eq!(wire[2], wire[0]);
    assert!(wire[3..].iter().all(|request| request == &wire[0]));
}

#[cfg(test)]
#[test]
fn a_relay_connection_failure_is_retried_and_counted_but_a_lost_reply_is_not() {
    use super::event::Kind;
    use super::relay::{
        Origin,
        tests::{CAPABILITY, with_replies},
    };
    use crate::fixture::Fixture;

    let fixture = Fixture::folder();
    let mut alerts = Alerts::at(&fixture.root.join("config"), &fixture.root.join("state"));
    // Port zero cannot have a listening server and avoids racing for a free port.
    alerts.relay = Some(Origin::Test("http://127.0.0.1:0".into()));
    let destination = Destination::Telegram;
    alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
    alerts.set_forwarding(destination, true).unwrap();
    let settings = alerts.settings().unwrap();
    let status = Status { file: alerts.state_dir().join("alerts.status"), notes: Mutex::default() };
    let cancelled = AtomicBool::new(false);
    let mut delivery = Delivery::default();
    let deliver = |alerts: &Alerts, delivery: &mut Delivery, kind| {
        let target = Target {
            alerts,
            settings: &settings,
            status: &status,
            destination,
            secret: alerts.target_secret(&settings, destination).unwrap(),
            hosted_telegram: true,
            cancelled: &cancelled,
        };
        delivery.deliver(&Alert { kind, waiting: None, app: None, text: String::new(), origin: None }, &target);
    };

    let began = Instant::now();
    deliver(&alerts, &mut delivery, Kind::Blocked);
    assert!(began.elapsed() >= Duration::from_secs(4), "expected both retry delays");
    assert_eq!((delivery.sent, delivery.unsent, delivery.sent_at.len()), (0, 1, 0));
    assert!(note(alerts.state_dir()).contains("The last alert could not be delivered and was dropped."));

    // The relay receives the next request but closes the connection without a reply.
    let (_, wire) = with_replies(&[(0, ""), (204, "")], |origin| {
        alerts.relay = Some(Origin::Test(origin.into()));
        deliver(&alerts, &mut delivery, Kind::Error);
        assert_eq!((delivery.sent, delivery.unsent, delivery.sent_at.len()), (0, 1, 1));
        assert!(note(alerts.state_dir()).contains("The last alert was not retried"));

        deliver(&alerts, &mut delivery, Kind::Done);
        assert_eq!((delivery.sent, delivery.unsent, delivery.sent_at.len()), (1, 0, 2));
    });
    for (request, text) in wire.iter().zip([
        "Rex: a program reported an error\n(1 earlier alert was not sent.)",
        "Rex: a program finished\n(1 earlier alert was not sent.)",
    ]) {
        assert_eq!(request.split_once("\r\n\r\n").unwrap().1, format!("{{\"text\":{}}}", rex::json(text)));
    }
}

#[cfg(test)]
#[test]
fn a_page_from_in_front_of_the_relay_does_not_pause_mirroring() {
    use super::event::Kind;
    use super::relay::{
        Origin,
        tests::{CAPABILITY, with_replies},
    };
    use crate::fixture::Fixture;

    // What a proxy answers while no relay is deployed behind it. The relay itself gives a
    // well-formed send neither status, so neither says anything about the saved connection.
    let replies = [(404, "<html>Application not found</html>"), (204, ""), (403, "<html>Forbidden</html>"), (204, "")];
    let (_, wire) = with_replies(&replies, |origin| {
        let fixture = Fixture::folder();
        let mut alerts = Alerts::at(&fixture.root.join("config"), &fixture.root.join("state"));
        alerts.relay = Some(Origin::Test(origin.to_owned()));
        alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
        alerts.set_forwarding(Destination::Telegram, true).unwrap();
        let settings = alerts.settings().unwrap();
        let status = Status { file: alerts.state_dir().join("alerts.status"), notes: Mutex::default() };
        let cancelled = AtomicBool::new(false);
        let target = Target {
            alerts: &alerts,
            settings: &settings,
            status: &status,
            destination: Destination::Telegram,
            secret: alerts.target_secret(&settings, Destination::Telegram).unwrap(),
            hosted_telegram: true,
            cancelled: &cancelled,
        };
        let mut delivery = Delivery::default();
        for kind in [Kind::Blocked, Kind::Done] {
            delivery.deliver(&Alert { kind, waiting: None, app: None, text: String::new(), origin: None }, &target);
            assert!(!delivery.refused, "{}", note(alerts.state_dir()));
        }
        assert_eq!((delivery.sent, delivery.unsent), (2, 0));
    });
    assert_eq!(wire.len(), 4);
}
