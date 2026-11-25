//! Supervises one Rex alert forwarder. The worker owns its event stream and
//! delivery queues; its parent only reads local intent and manages its lifetime.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, Read as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::event::{self, Alert, Seen};
use super::send::{self, Outcome};
use super::{Alerts, Destination, Health, Settings, lifecycle, message, origin, secret};
use crate::setup::lock;
use crate::{fs_ops, preferences, rex};

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
const SLOWEST_RETRY: Duration = Duration::from_secs(30);
/// A recovered connection must stay up this long before another outage
/// can produce a new notice or reset backoff.
const SETTLE: Duration = Duration::from_secs(60);
const STATUS_LIMIT: u64 = 1 << 10;
const REFUSED_CREDENTIAL: &str =
    "Paused: the saved destination refused its credential. Save it again or turn its forwarding off and on.";

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn caught(signal: libc::c_int) {
    if signal != libc::SIGCHLD {
        STOP.store(true, Ordering::Relaxed);
    }
}

fn empty_mask() -> libc::sigset_t {
    // SAFETY: sigset_t is integer storage; initialize all words, including any unused by libc.
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `mask` is writable storage for one signal set.
    unsafe { libc::sigemptyset(&raw mut mask) };
    mask
}

/// Wait for SIGCHLD or a stop request without polling an idle worker. Keep
/// signals blocked between checking its state and atomically waiting for them.
struct Signals(libc::sigset_t);

impl Signals {
    fn watch() -> io::Result<Self> {
        let watched = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGCHLD];
        let mut set = empty_mask();
        let mut unblocked = empty_mask();
        for signal in watched {
            // SAFETY: `set` is initialized and each signal is a valid constant.
            unsafe { libc::sigaddset(&raw mut set, signal) };
        }
        // SAFETY: Both masks are valid; the old mask is written to separate storage.
        let error = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, &raw mut unblocked) };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        // SAFETY: sigaction is a C record; its handler and mask are initialized below.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_mask = empty_mask();
        action.sa_sigaction = caught as *const () as usize;
        for signal in watched {
            // SAFETY: pthread_sigmask initialized `unblocked`; `signal` is valid.
            unsafe { libc::sigdelset(&raw mut unblocked, signal) };
            action.sa_flags = if signal == libc::SIGCHLD { libc::SA_NOCLDSTOP } else { 0 };
            // SAFETY: The action has a signal-handler ABI and only sets an atomic flag.
            if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self(unblocked))
    }

    fn wait(&self, remaining: Option<Duration>) -> io::Result<()> {
        let timeout = remaining.map(|wait| libc::timespec {
            tv_sec: wait.as_secs().try_into().unwrap_or(libc::time_t::MAX),
            tv_nsec: wait.subsec_nanos() as _,
        });
        // SAFETY: No descriptors are watched; the timeout and mask are
        // valid until pselect returns and restores the previous blocked mask.
        let result = unsafe {
            libc::pselect(
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                timeout.as_ref().map_or(std::ptr::null(), std::ptr::from_ref),
                &raw const self.0,
            )
        };
        if result >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted { Ok(()) } else { Err(error) }
    }
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

/// Back off indefinitely without ever waiting more than thirty seconds.
pub(super) struct Backoff(Duration);

impl Backoff {
    pub(super) fn new() -> Self {
        Self(Duration::from_secs(1))
    }

    pub(super) fn next(&mut self) -> Duration {
        let wait = self.0;
        self.0 = (wait * 2).min(SLOWEST_RETRY);
        wait
    }
}

/// A private process group contains the worker, Rex and in-flight deliveries.
/// Keep the worker's PID reserved until the whole group has been signalled.
struct Worker(Option<Child>);

impl Worker {
    fn start(milo: &Path, signals: &Signals, recovery: Recovery) -> io::Result<Self> {
        let mut command = Command::new(milo);
        command.args(["alerts", "run"]).arg(std::process::id().to_string()).arg(recovery.to_string());
        command.process_group(0).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        let unblocked = signals.0;
        let restore = move || {
            // SAFETY: The copied mask is valid and no old mask is requested.
            let error = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const unblocked, std::ptr::null_mut()) };
            if error == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(error)) }
        };
        // SAFETY: The closure only restores the signal mask before exec; it does not allocate or acquire a lock.
        unsafe { command.pre_exec(restore) };
        fs_ops::spawn(&mut command).map(|child| Self(Some(child)))
    }

    fn reap(&mut self) -> io::Result<ExitStatus> {
        let mut child = self.0.take().expect("unreaped worker");
        // SAFETY: This direct child started a private group and has not been reaped.
        unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
        child.wait()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.0.is_some() {
            let _ = self.reap();
        }
    }
}

/// Examine a direct child without releasing its PID (and group identity).
fn exited(child: &Child) -> io::Result<bool> {
    loop {
        // SAFETY: P_PID selects only this child; info is writable and zero is
        // the no-event record when WNOHANG finds no exit.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: P_PID names our unreaped child and waitid may write one siginfo_t.
        if unsafe {
            libc::waitid(libc::P_PID, child.id(), &raw mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
        } == 0
        {
            // SAFETY: A successful WEXITED query returns the child record or the zeroed no-event result.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

pub(super) fn supervise(alerts: &Alerts) -> io::Result<()> {
    let signals = Signals::watch()?;
    let state = alerts.state_dir();
    let Some(_lock) = lock::hold_alerts_supervisor(state)? else { return Ok(()) };
    if wanted(alerts).is_none() {
        return Ok(());
    }
    // Upgrade from a build that started the worker directly, or finish an
    // orphaned worker before taking over. Only our singleton can start one.
    lock::stop_alerts_worker(state)?;
    let milo = std::env::current_exe()?;
    let mut retry = Backoff::new();
    let mut recovery = Recovery::Fresh;
    while !STOP.load(Ordering::Relaxed) && wanted(alerts).is_some() {
        forget_note(state);
        if let Ok(mut worker) = Worker::start(&milo, &signals, recovery) {
            let child = worker.0.as_ref().expect("unreaped worker");
            while !STOP.load(Ordering::Relaxed) && !exited(child)? {
                signals.wait(None)?;
            }
            let pid = child.id();
            let status = worker.reap()?;
            if STOP.load(Ordering::Relaxed) || status.success() {
                break;
            }
            if let Some(status) = snapshot(state).0.filter(|status| status.pid == pid) {
                recovery = status.recovery;
                if matches!(recovery, Recovery::Armed(_)) {
                    retry = Backoff::new();
                }
                recovery.lost();
            }
        }
        let until = Instant::now() + retry.next();
        while !STOP.load(Ordering::Relaxed) {
            let Some(left) = until.checked_duration_since(Instant::now()) else { break };
            signals.wait(Some(left))?;
        }
        // Read the latest intent again immediately before a replacement can start.
    }
    Ok(())
}

extern "C" fn end_worker(_: libc::c_int) {
    // SAFETY: PID zero targets only this process group, verified private before installing this handler.
    unsafe { libc::kill(0, libc::SIGKILL) };
}

/// Set when Rex has started its listener, which a Rex that had gone away does
/// as it comes back.
static REX_STARTED: AtomicBool = AtomicBool::new(false);

extern "C" fn rex_started(_: libc::c_int) {
    REX_STARTED.store(true, Ordering::Relaxed);
}

pub(super) fn run(alerts: &Alerts, parent: libc::pid_t, mut recovery: Recovery) -> io::Result<()> {
    // SAFETY: getppid reads this process's parent without taking any pointers.
    let actual_parent = unsafe { libc::getppid() };
    // SAFETY: getpgrp only reads this process's group.
    let group = unsafe { libc::getpgrp() };
    if parent <= 1 || actual_parent != parent || u32::try_from(group).ok() != Some(std::process::id()) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    // SAFETY: The process group check above makes end_worker safe.
    if unsafe { libc::signal(libc::SIGTERM, end_worker as *const () as libc::sighandler_t) } == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The handler only stores an atomic.
    if unsafe { libc::signal(lock::REX_STARTED, rex_started as *const () as libc::sighandler_t) } == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    let Some(_lock) = lock::hold_alerts(alerts.state_dir())? else { return Ok(()) };
    // The write end belongs only to the supervisor. This also catches a
    // supervisor killed before it could clean up its worker.
    std::thread::Builder::new().spawn(|| {
        let _ = io::stdin().read_exact(&mut [0]);
        end_worker(0);
    })?;
    let status = Status { file: alerts.state_dir().join("alerts.status"), notes: Mutex::default() };
    let mut deliveries = Destination::ALL.map(|_| Delivery::default());
    let mut retry = Backoff::new();
    status.update(Health::WaitingForRex, recovery);
    loop {
        let Some(settings) = wanted(alerts) else { return Ok(()) };
        let source = rex::AlertSource::select(settings.origin);
        if !source.available() {
            if source.replaced() {
                continue;
            }
            recovery.refresh(settings.active_revisions());
            recovery.lost();
            status.update(Health::WaitingForRex, recovery);
            wait_for_rex(alerts, &settings, &mut retry);
            continue;
        }
        if changed(alerts, &settings).is_some() {
            continue;
        }
        match mirror(alerts, &status, &settings, source, &mut deliveries, &mut recovery, &mut retry) {
            Ended::Off => return Ok(()),
            Ended::Changed => status.update(Health::WaitingForRex, recovery),
            Ended::Stream => return Err(io::Error::other("Rex event stream did not close")),
        }
    }
}

type Revisions = [Option<u64>; 2];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Recovery {
    #[default]
    Fresh,
    Armed(Revisions),
    Pending(Revisions),
    Settling,
}

impl std::fmt::Display for Recovery {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (phase, revisions) = match self {
            Self::Fresh => return out.write_str("fresh"),
            Self::Settling => return out.write_str("settling"),
            Self::Armed(revisions) => ("armed", revisions),
            Self::Pending(revisions) => ("pending", revisions),
        };
        out.write_str(phase)?;
        for revision in revisions {
            if let Some(revision) = revision {
                write!(out, ":{revision}")?;
            } else {
                out.write_str(":-")?;
            }
        }
        Ok(())
    }
}

impl Recovery {
    pub(super) fn parse(name: &str) -> Option<Self> {
        match name {
            "fresh" => return Some(Self::Fresh),
            "settling" => return Some(Self::Settling),
            _ => {}
        }
        let (phase, scopes) = name.split_once(':')?;
        let (telegram, discord) = scopes.split_once(':')?;
        let revision = |scope: &str| if scope == "-" { Some(None) } else { scope.parse::<u64>().ok().map(Some) };
        let revisions = [revision(telegram)?, revision(discord)?];
        match phase {
            "armed" => Some(Self::Armed(revisions)),
            "pending" => Some(Self::Pending(revisions)),
            _ => None,
        }
    }

    fn refresh(&mut self, current: Revisions) {
        if let Self::Armed(revisions) = self {
            *revisions = current;
        }
    }

    fn lost(&mut self) {
        if let Self::Armed(revisions) = *self {
            *self = Self::Pending(revisions);
        }
    }

    /// Carry the interrupted generations, not whatever was enabled later.
    fn connected(&mut self, current: Revisions) -> Option<Revisions> {
        match *self {
            Self::Fresh => {
                *self = Self::Armed(current);
                None
            }
            Self::Pending(revisions) => {
                *self = Self::Settling;
                Some(revisions)
            }
            _ => None,
        }
    }
}

fn wanted(alerts: &Alerts) -> Option<Settings> {
    alerts.settings().ok().filter(Settings::has_active)
}

fn same_disclosure(before: &Settings, now: &Settings) -> bool {
    before.disclosure_revision == now.disclosure_revision
        && before.kinds == now.kinds
        && before.content == now.content
        && before.origin == now.origin
}

fn changed(alerts: &Alerts, settings: &Settings) -> Option<Ended> {
    let Some(now) = wanted(alerts) else { return Some(Ended::Off) };
    (!same_disclosure(settings, &now)).then_some(Ended::Changed)
}

fn provider_secret(context: &Pump<'_>, settings: &Settings, delivery: &Delivery) -> Option<String> {
    let Pump { alerts, status, destination, .. } = *context;
    if !settings.active(destination) {
        return None;
    }
    if lifecycle::known_refused(alerts, destination) {
        delivery.report(status, destination, REFUSED_CREDENTIAL);
        return None;
    }
    match alerts.target_secret(settings, destination) {
        Ok(secret) => Some(secret),
        Err(error) => {
            delivery.report(status, destination, &format!("Stopped: {error}"));
            None
        }
    }
}

/// Each worker has a queue that survives Rex reconnects. A destination's
/// retries or rate limits cannot delay the reader or the other destination.
fn mirror(
    alerts: &Alerts,
    status: &Status,
    settings: &Settings,
    source: rex::AlertSource,
    deliveries: &mut [Delivery; 2],
    recovery: &mut Recovery,
    retry: &mut Backoff,
) -> Ended {
    let cancelled = AtomicBool::new(false);
    let contexts =
        Destination::ALL.map(|destination| Pump { alerts, settings, status, destination, cancelled: &cancelled });
    for (delivery, revision) in deliveries.iter_mut().zip(settings.active_revisions()) {
        delivery.rebind(revision);
    }
    let secrets = contexts
        .each_ref()
        .map(|context| provider_secret(context, settings, &deliveries[context.destination as usize]));
    if secrets.iter().all(Option::is_none) {
        // A newly enabled provider may have arrived during credential loading.
        if wanted(alerts).is_some_and(|now| {
            now.active_revisions() != settings.active_revisions() || !same_disclosure(settings, &now)
        }) {
            return Ended::Changed;
        }
        status.update(Health::Stopped, *recovery);
        return Ended::Off;
    }
    let dropped = Destination::ALL.map(|_| AtomicU32::new(0));
    std::thread::scope(|scope| {
        let mut senders = [None, None];
        let mut workers = [None, None];
        for ((context, delivery), secret) in contexts.into_iter().zip(deliveries).zip(secrets) {
            let index = context.destination as usize;
            let (sender, waiting) = mpsc::sync_channel(WAITING);
            senders[index] = Some(sender);
            let dropped = &dropped[index];
            workers[index] = Some(scope.spawn(move || delivery.pump(&waiting, dropped, &context, secret)));
        }
        let queues = Queues { senders: &senders, dropped: &dropped };
        let ended = follow(alerts, status, settings, source, queues, recovery, retry);
        cancelled.store(true, Ordering::Relaxed);
        drop(senders);
        for worker in workers.iter().flatten() {
            worker.thread().unpark();
        }
        ended
    })
}

struct Queues<'a> {
    senders: &'a [Option<mpsc::SyncSender<Queued>>; 2],
    dropped: &'a [AtomicU32; 2],
}

fn follow(
    alerts: &Alerts,
    status: &Status,
    settings: &Settings,
    mut source: rex::AlertSource,
    queues: Queues<'_>,
    recovery: &mut Recovery,
    retry: &mut Backoff,
) -> Ended {
    let mut available = true; // The worker already checked before reading credentials.
    loop {
        if let Some(ended) = changed(alerts, settings) {
            return ended;
        }
        if available && let Ok(mut stream) = source.events() {
            let ended = read_stream(&mut stream, alerts, settings, source.origin(), &queues, |phase, revisions| {
                let recovered = match phase {
                    StreamProgress::Opened => recovery.connected(revisions),
                    StreamProgress::Updated => {
                        recovery.refresh(revisions);
                        None
                    }
                    StreamProgress::Settled => {
                        *recovery = Recovery::Armed(revisions);
                        *retry = Backoff::new();
                        None
                    }
                };
                status.update(Health::Listening, *recovery);
                if let Some(interrupted) = recovered {
                    lifecycle::recovered(alerts, interrupted);
                }
            });
            let _ = stream.wait();
            if let Some(ended) = ended {
                if ended != Ended::Stream {
                    return ended;
                }
            } else {
                // The configured Rex program exited but descendants held its
                // pipe open. Restart the worker to clean its entire group.
                return Ended::Stream;
            }
        } else if source.replaced() {
            return Ended::Changed;
        }
        let Some(now) = wanted(alerts) else { return Ended::Off };
        if !same_disclosure(settings, &now) {
            return Ended::Changed;
        }
        recovery.refresh(now.active_revisions());
        recovery.lost();
        status.update(Health::WaitingForRex, *recovery);
        if let Some(ended) = wait_for_rex(alerts, settings, retry) {
            return ended;
        }
        source = rex::AlertSource::select(settings.origin);
        available = source.available();
    }
}

/// Waits out the next retry, or only until Rex starts its listener. Rex is
/// then back, and it repeats nothing it reports before the stream is open
/// again, so the retries start over from the shortest wait.
fn wait_for_rex(alerts: &Alerts, settings: &Settings, retry: &mut Backoff) -> Option<Ended> {
    let until = Instant::now() + retry.next();
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        if let Some(ended) = changed(alerts, settings) {
            return Some(ended);
        }
        if REX_STARTED.swap(false, Ordering::Relaxed) {
            *retry = Backoff::new();
            return None;
        }
        std::thread::sleep(left.min(TICK));
    }
    changed(alerts, settings)
}

enum StreamProgress {
    Opened,
    Updated,
    Settled,
}

fn read_stream(
    stream: &mut Child,
    alerts: &Alerts,
    settings: &Settings,
    server: Option<&rex::AlertServer>,
    queues: &Queues<'_>,
    mut connected: impl FnMut(StreamProgress, Revisions),
) -> Option<Ended> {
    let output = stream.stdout.take().expect("piped Rex event stream");
    let (reading, ended) = mpsc::channel::<()>();
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let cancellation = &cancelled;
        scope.spawn(move || {
            read(Stream { output, cancellation }, settings, |mut alert, envelope| {
                if cancellation.load(Ordering::Relaxed) {
                    return false;
                }
                let Some(now) = wanted(alerts).filter(|now| same_disclosure(settings, now)) else { return false };
                if let Some(server) = server {
                    alert.origin = origin::label(envelope, &server.host, |id| server.session(id));
                }
                if cancellation.load(Ordering::Relaxed) {
                    return false;
                }
                fan_out(alert, now.active_revisions(), queues.senders, queues.dropped);
                true
            });
            drop(reading);
        });
        let mut opened = false;
        let mut settled = false;
        let mut previous = [None; 2];
        let began = Instant::now();
        let mut exit_seen = None;
        let result = loop {
            let next = if !opened {
                Some(rex::ALERTS_CONNECT)
            } else if !settled {
                Some(SETTLE)
            } else {
                None
            };
            let wait = if let Some(next) = next
                && exit_seen.is_none()
            {
                next.saturating_sub(began.elapsed()).min(TICK)
            } else {
                TICK
            };
            match ended.recv_timeout(wait) {
                Err(RecvTimeoutError::Disconnected) => break Some(Ended::Stream),
                Err(RecvTimeoutError::Timeout) => {
                    let Some(now) = wanted(alerts) else { break Some(Ended::Off) };
                    if !same_disclosure(settings, &now) {
                        break Some(Ended::Changed);
                    }
                    match exited(stream) {
                        Ok(false) => {
                            let lasted = began.elapsed();
                            let revisions = now.active_revisions();
                            // `rex events` has no ready frame when idle. A
                            // successful local probe and a stream past Rex's
                            // subscription deadline distinguish it from a failed spawn.
                            if !opened && lasted >= rex::ALERTS_CONNECT {
                                // The CLI may have connected to a replacement between the
                                // pre-spawn check and its subscription. Repin, not recover.
                                if server.is_some_and(|server| !server.is_current()) {
                                    break Some(Ended::Changed);
                                }
                                opened = true;
                                connected(StreamProgress::Opened, revisions);
                            } else if !settled && lasted >= SETTLE {
                                settled = true;
                                connected(StreamProgress::Settled, revisions);
                            } else if opened && revisions != previous {
                                connected(StreamProgress::Updated, revisions);
                            }
                            previous = revisions;
                        }
                        Ok(true) if exit_seen.get_or_insert_with(Instant::now).elapsed() < TICK => {}
                        Ok(true) | Err(_) => break None,
                    }
                }
                Ok(()) => unreachable!(),
            }
        };
        cancelled.store(true, Ordering::Relaxed);
        // A descendant that holds stdout open must not prevent joining the reader.
        let _ = stream.kill();
        result
    })
}

struct Stream<'a> {
    output: ChildStdout,
    cancellation: &'a AtomicBool,
}

impl io::Read for Stream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut poll = [libc::pollfd { fd: self.output.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        while !self.cancellation.load(Ordering::Relaxed) {
            match crate::term::wait_ready(&mut poll, crate::term::milliseconds(TICK)) {
                0 => {}
                ready if ready > 0 => return self.output.read(buf),
                _ => {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
            }
        }
        Ok(0)
    }
}

enum Queued {
    Only(u64, Alert),
    Shared(u64, Arc<Alert>),
}

fn fan_out(
    alert: Alert,
    revisions: [Option<u64>; 2],
    senders: &[Option<mpsc::SyncSender<Queued>>; 2],
    dropped: &[AtomicU32; 2],
) {
    let queue = |index: usize, sender: &mpsc::SyncSender<Queued>, alert| {
        if sender.try_send(alert).is_err() {
            dropped[index].fetch_add(1, Ordering::Relaxed);
        }
    };
    let [first, second] = std::array::from_fn(|index| senders[index].as_ref().zip(revisions[index]));
    match (first, second) {
        (Some((first, a)), Some((second, b))) => {
            let alert = Arc::new(alert);
            queue(0, first, Queued::Shared(a, Arc::clone(&alert)));
            queue(1, second, Queued::Shared(b, alert));
        }
        (Some((first, revision)), None) => queue(0, first, Queued::Only(revision, alert)),
        (None, Some((second, revision))) => queue(1, second, Queued::Only(revision, alert)),
        (None, None) => {}
    }
}

struct Pump<'a> {
    alerts: &'a Alerts,
    settings: &'a Settings,
    status: &'a Status,
    destination: Destination,
    cancelled: &'a AtomicBool,
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

    fn matches(&self, now: &Settings) -> bool {
        self.settings
            .active_revision(self.destination)
            .is_some_and(|revision| now.active_revision(self.destination) == Some(revision))
            && same_disclosure(self.settings, now)
            && (self.destination != Destination::Telegram || now.telegram_chat == self.settings.telegram_chat)
    }

    fn current(&self) -> bool {
        !self.stopped() && self.alerts.settings().is_ok_and(|now| self.matches(&now))
    }

    fn pause(&self, wait: Duration) {
        let until = Instant::now() + wait;
        while self.current() {
            let Some(remaining) = until.checked_duration_since(Instant::now()) else { break };
            std::thread::park_timeout(remaining.min(TICK));
        }
    }
}

#[derive(Default)]
struct Delivery {
    /// Keep delivery state tied to the provider across disclosure-only reconnects.
    revision: Option<u64>,
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
    /// banned. Only a new provider generation or worker may try it again;
    /// credential refusals also persist across worker restarts.
    refused: bool,
}

impl Delivery {
    fn rebind(&mut self, revision: Option<u64>) -> bool {
        if self.revision == revision {
            return false;
        }
        *self = Self { revision, ..Self::default() };
        true
    }

    fn pump(
        &mut self,
        waiting: &Receiver<Queued>,
        dropped: &AtomicU32,
        context: &Pump<'_>,
        mut cached: Option<String>,
    ) {
        let Pump { alerts, settings: baseline, status, destination, cancelled } = *context;
        while let Ok(queued) = waiting.recv() {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            let (work_revision, alert) = match &queued {
                Queued::Only(revision, alert) => (*revision, alert),
                Queued::Shared(revision, alert) => (*revision, alert.as_ref()),
            };
            let Some(settings) = wanted(alerts) else { continue };
            if settings.active_revision(destination) != Some(work_revision) || !same_disclosure(baseline, &settings) {
                continue;
            }
            if self.rebind(Some(work_revision)) {
                dropped.store(0, Ordering::Relaxed);
                cached = provider_secret(context, &settings, self);
            }
            let Some(secret) = cached.take() else { continue };
            self.unsent += dropped.swap(0, Ordering::Relaxed);
            let hosted_telegram = destination == Destination::Telegram && secret::telegram_binding(&secret).is_some();
            let target =
                Target { alerts, settings: &settings, status, destination, secret, hosted_telegram, cancelled };
            self.deliver(alert, &target);
            cached = Some(target.secret);
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
        let Some(request) =
            alerts.message_request(destination, secret, &settings.telegram_chat, &text, Some(alert.kind))
        else {
            self.refused = true;
            self.note(target, "Stopped: the saved destination secret is invalid. Save it again.");
            return;
        };
        enum Attempt<T> {
            Cancelled,
            Refused,
            Transport(io::Result<T>),
        }
        for attempt in 0..ATTEMPTS {
            // Register only this provider's transport under the settings
            // lock, so a pause can't race between the check and the spawn.
            // No request bytes are sent until the lock has been released.
            let started = alerts.locked(|edit| {
                if target.stopped() || !edit.settings().is_ok_and(|now| target.matches(&now)) {
                    return Ok(Attempt::Cancelled);
                }
                if lifecycle::known_refused(alerts, destination) {
                    return Ok(Attempt::Refused);
                }
                let lease = (alerts.state_dir(), destination.forwarding_lock());
                Ok(Attempt::Transport(send::launch(&request, Some(lease))))
            });
            let outcome = match started {
                Ok(Attempt::Transport(Ok(transport))) => transport.post(),
                Ok(Attempt::Transport(Err(_))) => Outcome::Failed(None),
                Ok(Attempt::Refused) => {
                    self.refused = true;
                    self.unsent += 1;
                    self.note(target, REFUSED_CREDENTIAL);
                    return;
                }
                Ok(Attempt::Cancelled) | Err(_) => return,
            };
            if !target.current() {
                return;
            }
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
                    let rejected_credential = matches!(code, 401 | 403 | 404);
                    if rejected_credential || self.refusals >= REFUSALS {
                        if rejected_credential {
                            lifecycle::provider_refused(alerts, settings, destination, secret);
                        }
                        self.refused = true;
                        let resume = if code == 401 && target.hosted_telegram {
                            ""
                        } else if rejected_credential {
                            " Save the destination again or turn its forwarding off and on."
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
    health: Option<Health>,
    recovery: Recovery,
    providers: [String; 2],
}

impl Status {
    fn update(&self, health: Health, recovery: Recovery) {
        let mut notes = self.notes.lock().unwrap();
        if notes.health != Some(health) || notes.recovery != recovery {
            notes.health = Some(health);
            notes.recovery = recovery;
            self.write(&notes);
        }
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
        let pid = std::process::id();
        let recovery = notes.recovery;
        let health = match notes.health {
            Some(Health::WaitingForRex) => "waiting",
            Some(Health::Listening) => "listening",
            _ => "stopped",
        };
        let text = format!("{pid} {recovery} {health}\n{telegram}{discord}");
        let _ = preferences::locked(&self.file, |dir| preferences::replace(dir, &self.file, &text));
    }
}

struct Snapshot {
    pid: u32,
    recovery: Recovery,
    health: Health,
}

fn snapshot(state: &Path) -> (Option<Snapshot>, String) {
    let mut text = preferences::read(&state.join("alerts.status"), STATUS_LIMIT).ok().flatten().unwrap_or_default();
    let Some((header, _)) = text.split_once('\n') else { return (None, String::new()) };
    let mut words = header.split_ascii_whitespace();
    let status = match (words.next().and_then(|pid| pid.parse().ok()), words.next(), words.next()) {
        (Some(pid), Some(recovery), Some(health)) => {
            let health = match health {
                "waiting" => Health::WaitingForRex,
                "listening" => Health::Listening,
                "stopped" => Health::Stopped,
                _ => return (None, String::new()),
            };
            let Some(recovery) = Recovery::parse(recovery) else { return (None, String::new()) };
            Snapshot { pid, recovery, health }
        }
        _ => return (None, String::new()),
    };
    let first = header.len() + 1;
    let end = text.trim_end().len().max(first);
    text.truncate(end);
    text.drain(..first);
    (Some(status), text)
}

pub(super) fn health(state: &Path, settings: &Settings, set_up: bool) -> (Health, String) {
    if !settings.enabled {
        return (if set_up { Health::Paused } else { Health::Off }, String::new());
    }
    if !settings.has_active() {
        return (Health::Stopped, String::new());
    }
    let (status, notes) = snapshot(state);
    let live = lock::alerts_worker(state).ok().flatten();
    if let Some(status) = &status
        && live == Some(status.pid as libc::pid_t)
    {
        return (status.health, notes);
    }
    if lock::alerts_supervised(state).unwrap_or(false) {
        return (Health::Retrying, String::new());
    }
    (Health::Stopped, if status.is_some_and(|status| status.health == Health::Stopped) { notes } else { String::new() })
}

#[cfg(test)]
fn note(state: &Path) -> String {
    snapshot(state).1
}

pub(crate) fn forget_note(state: &Path) {
    let _ = std::fs::remove_file(state.join("alerts.status"));
}

/// Passes on each chosen kind with only the chosen content. A line
/// too long to be an alert is skipped whole. What each terminal last said is
/// kept only for this stream: Rex repeats nothing to a listener that joins
/// late, so after the stream is opened again nothing remembered can be trusted.
/// The handler returns false to stop processing even a buffered stream.
pub(super) fn read(
    stream: impl io::Read,
    settings: &Settings,
    mut queue: impl FnMut(Alert, &event::Envelope<'_>) -> bool,
) {
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
                let Some(envelope) = event::Envelope::read(line.trim_ascii_end()) else { continue };
                let alert =
                    event::parse(&envelope, settings.content).and_then(|event| seen.alert(event, Instant::now()));
                if let Some(alert) = alert.filter(|alert| settings.mirrors(alert.kind))
                    && !queue(alert, &envelope)
                {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
#[test]
fn a_recovery_notice_is_not_rearmed_by_flapping_or_worker_replacement() {
    let initial = [Some(3), Some(9)];
    let mut recovery = Recovery::Fresh;
    recovery.lost();
    assert_eq!(recovery.connected(initial), None); // The first established stream is not a recovery.
    recovery.lost();
    for _ in 0..4 {
        recovery.lost();
    }
    let changed = [None, Some(10)];
    recovery.refresh(changed);
    assert_eq!(recovery.connected(changed), Some(initial));
    // Each brief reconnection or replacement belongs to the same outage.
    for _ in 0..4 {
        recovery = Recovery::parse(&recovery.to_string()).unwrap();
        recovery.lost();
        assert_eq!(recovery.connected(changed), None);
    }
    recovery = Recovery::Armed(changed); // The stream stayed up through the settling interval.
    recovery.lost();
    assert_eq!(recovery.connected(changed), Some(changed));
}

#[cfg(test)]
#[test]
fn health_distinguishes_an_empty_setup_from_a_pause_and_ignores_a_stale_listener() {
    let home = crate::fixture::Fixture::folder();
    let state = &home.root;
    let mut settings = Settings::default();
    assert_eq!(health(state, &settings, false).0, Health::Off);
    assert_eq!(health(state, &settings, true).0, Health::Paused);
    settings.enabled = true;
    settings.destination = Some(Destination::Discord);
    let status = Status { file: state.join("alerts.status"), notes: Mutex::default() };
    status.update(Health::Listening, Recovery::Armed(settings.active_revisions()));
    assert_eq!(health(state, &settings, true).0, Health::Stopped);
}

#[cfg(test)]
#[test]
fn a_refusal_recorded_while_listening_prevents_ordinary_delivery() {
    let fixture = crate::fixture::Fixture::folder();
    let mut alerts = Alerts::at(&fixture.root.join("config"), &fixture.root.join("state"));
    alerts.stand_in = Some("http://127.0.0.1:0".into());
    let destination = Destination::Discord;
    alerts.set_discord("https://discord.com/api/webhooks/1234567891/synthetic_SYNTHETICsynthetic-01").unwrap();
    alerts.set_forwarding(destination, true).unwrap();
    let settings = alerts.settings().unwrap();
    let status = Status { file: alerts.state_dir().join("alerts.status"), notes: Mutex::default() };
    let cancelled = AtomicBool::new(false);
    let target = Target {
        alerts: &alerts,
        settings: &settings,
        status: &status,
        destination,
        secret: alerts.target_secret(&settings, destination).unwrap(),
        hosted_telegram: false,
        cancelled: &cancelled,
    };
    alerts
        .locked(|_| {
            lifecycle::record_refusal(&alerts, destination);
            Ok(())
        })
        .unwrap();

    let mut delivery = Delivery::default();
    let alert = Alert { kind: super::Kind::Bell, waiting: None, app: None, text: String::new(), origin: None };
    delivery.deliver(&alert, &target);
    assert!(delivery.refused);
    assert!(note(alerts.state_dir()).contains(REFUSED_CREDENTIAL));
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
        fan_out(
            Alert { kind, waiting: None, app: None, text: String::new(), origin: None },
            [Some(0); 2],
            &senders,
            &dropped,
        );
    }
    let kinds: Vec<_> = discord_waiting
        .try_iter()
        .map(|queued| match queued {
            Queued::Shared(_, alert) => alert.kind,
            Queued::Only(_, alert) => alert.kind,
        })
        .collect();
    assert_eq!(kinds, [Kind::Bell, Kind::Blocked]);
    assert_eq!(dropped.map(AtomicU32::into_inner), [1, 0]);
}

#[cfg(test)]
#[test]
fn toggling_origin_off_and_back_on_discards_pending_labels_without_rotating_providers() {
    use super::Kind;
    use super::relay::{
        Origin,
        tests::{CAPABILITY, with_replies},
    };
    use crate::fixture::Fixture;

    let (_, wire) = with_replies(&[(503, r#"{"retry_after":3}"#), (204, "")], |origin| {
        let fixture = Fixture::folder();
        let mut alerts = Alerts::at(&fixture.root.join("config"), &fixture.root.join("state"));
        alerts.relay = Some(Origin::Test(origin.into()));
        alerts.stand_in = Some(origin.into());
        alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
        alerts.set_discord("https://discord.com/api/webhooks/1234567891/synthetic_SYNTHETICsynthetic-01").unwrap();
        for destination in Destination::ALL {
            alerts.set_forwarding(destination, true).unwrap();
        }
        alerts.set_origin(true).unwrap();
        let settings = alerts.settings().unwrap();
        let status = Status { file: alerts.state_dir().join("alerts.status"), notes: Mutex::default() };
        let cancelled = AtomicBool::new(false);
        let destination = Destination::Telegram;
        let telegram = Target {
            alerts: &alerts,
            settings: &settings,
            status: &status,
            destination,
            secret: alerts.target_secret(&settings, destination).unwrap(),
            hosted_telegram: true,
            cancelled: &cancelled,
        };
        let alert = Alert {
            kind: Kind::Blocked,
            waiting: None,
            app: None,
            text: String::new(),
            origin: Some("old-label".into()),
        };
        let mut delivery = Delivery::default();
        std::thread::scope(|scope| {
            let sender = scope.spawn(|| delivery.deliver(&alert, &telegram));
            let deadline = Instant::now() + Duration::from_secs(2);
            while !note(alerts.state_dir()).contains("Trying again:") {
                assert!(Instant::now() < deadline, "Telegram did not schedule a retry");
                std::thread::sleep(Duration::from_millis(5));
            }
            alerts.set_origin(false).unwrap();
            alerts.set_origin(true).unwrap();
            sender.thread().unpark();
            sender.join().unwrap();
        });
        assert_eq!((delivery.sent, delivery.unsent), (0, 0));

        let now = alerts.settings().unwrap();
        assert!(now.origin);
        assert_eq!(now.active_revisions(), settings.active_revisions());
        assert_eq!(changed(&alerts, &settings), Some(Ended::Changed));
        let context =
            Pump { alerts: &alerts, settings: &settings, status: &status, destination, cancelled: &cancelled };
        let (sender, waiting) = mpsc::sync_channel(1);
        sender.send(Queued::Only(settings.active_revision(destination).unwrap(), alert)).unwrap();
        drop(sender);
        let dropped = AtomicU32::new(1);
        delivery.pump(&waiting, &dropped, &context, Some(telegram.secret));
        assert_eq!(dropped.into_inner(), 1, "stale queue reached delivery");

        let destination = Destination::Discord;
        let discord = Target {
            settings: &now,
            destination,
            secret: alerts.target_secret(&now, destination).unwrap(),
            hosted_telegram: false,
            ..telegram
        };
        let mut delivery = Delivery::default();
        let fresh = Alert { kind: Kind::Done, waiting: None, app: None, text: String::new(), origin: None };
        delivery.deliver(&fresh, &discord);
        assert_eq!(delivery.sent, 1);
    });
    assert!(wire[0].starts_with("POST /v1/send "));
    assert!(wire[0].contains("old-label"));
    assert!(wire[1].starts_with("POST /api/v10/webhooks/"));
    assert!(!wire[1].contains("old-label"));
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
