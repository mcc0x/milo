//! Runs terminal panes with shared input, help, redraws and file watching.

use std::io;
use std::ops::Range;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::help::Page;
use crate::keys::{Input, Key};
use crate::screen::{Screen, Style, YELLOW};
use crate::term::{self, Session, Wait, milliseconds};
use crate::ui;
use crate::watch::{Scope, Watcher};

/// What to do after a key: keep the frame, redraw, or close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    Ignored,
    Stay,
    Help,
    Leave,
}

/// A pane can observe an external source independently of the project file watcher.
pub enum External {
    Unchanged,
    Redraw,
    NewScope,
}

/// One screen in the terminal. The loop owns input, redraws and when to check.
pub trait Pane {
    fn draw(&mut self, screen: &mut Screen);
    fn help(&self) -> Page;
    /// A noninteractive warning, shown only when the pane has a spare row.
    fn notice(&self) -> Option<&'static str> {
        None
    }
    fn tracks_hover(&self) -> bool {
        false
    }
    fn hover_span(&self, _x: usize, _y: usize) -> Option<Range<usize>> {
        None
    }

    /// An empty, complete scope means nothing outside the pane can change it.
    fn watch_scope(&self) -> Scope {
        Scope { roots: Vec::new(), ignored: Default::default(), complete: true }
    }

    /// When Git is updating the index, keep the previous snapshot.
    fn git_busy(&self) -> bool {
        false
    }

    /// Takes in changes from outside. False asks the loop to check again soon.
    fn check(&mut self) -> bool {
        true
    }

    fn external_fd(&self) -> Option<RawFd> {
        None
    }

    fn check_external(&mut self) -> External {
        External::Unchanged
    }

    /// Records a filesystem notification before the next scheduled check.
    fn filesystem_changed(&mut self) {}

    /// How long the frame stays right when nothing happens.
    fn next_frame(&self) -> Option<Duration> {
        None
    }

    /// Handles a pane's own keys, including closing on q.
    fn key(&mut self, key: Key) -> After;
}

pub type Open<'a> = dyn Fn() -> Result<Box<dyn Pane>, String> + 'a;

/// The first-run guide hands off to the originally requested screen without leaving the terminal.
pub enum Start<'a> {
    Reading(&'a Path),
    Loaded,
    Check,
    Welcome(fn() -> Result<Box<dyn Pane>, String>),
}

const LOADING_DELAY: Duration = Duration::from_millis(75);

/// Minimum rest after a look if the watcher has reported a change.
const MIN_LOOK_INTERVAL: Duration = Duration::from_millis(120);
/// Rest at least this many times the cost of the last look, so repeated
/// looks at the same cost take at most a fifth of the time.
const REST: u32 = 4;
/// Time between looks when the watcher is expected to cover the project.
pub(super) const WATCHED: Duration = Duration::from_secs(30);
/// Time between looks when the watcher cannot cover the project.
const UNWATCHED: Duration = Duration::from_secs(2);
/// Ignore brief index locks, then wait without taking or repainting snapshots.
const GIT_GRACE: Duration = Duration::from_millis(400);
const GIT_RETRY: Duration = Duration::from_millis(500);
const WAITING_FOR_GIT: &str = "Waiting for Git";

/// When to look for changes again, with or without a report from the watcher.
pub(super) struct Pace {
    idle: Duration,
    ended: Instant,
    took: Duration,
    reported: bool,
    git_locked_since: Option<Instant>,
}

impl Pace {
    fn new(idle: Duration, now: Instant) -> Self {
        Self { idle, ended: now, took: Duration::ZERO, reported: false, git_locked_since: None }
    }

    #[cfg(test)]
    pub(super) fn idle(&self) -> Duration {
        self.idle
    }

    fn report_change(&mut self) {
        self.reported = true;
    }

    fn waiting_for_git(&self) -> bool {
        self.git_locked_since.is_some_and(|since| self.ended - since >= GIT_GRACE)
    }

    /// Only the transition to a visible wait needs a new frame.
    fn block_on_git(&mut self, now: Instant) -> bool {
        let was_waiting = self.waiting_for_git();
        self.git_locked_since.get_or_insert(now);
        self.ended = now;
        !was_waiting && self.waiting_for_git()
    }

    /// Time until the event loop next calls `Pane::check`.
    fn until_look(&self, now: Instant) -> Duration {
        if let Some(since) = self.git_locked_since {
            let due = if self.waiting_for_git() { self.ended + GIT_RETRY } else { since + GIT_GRACE };
            return due.saturating_duration_since(now);
        }
        let rest = if self.reported { MIN_LOOK_INTERVAL } else { self.idle }.max(self.took * REST);
        rest.saturating_sub(now.saturating_duration_since(self.ended))
    }

    /// An unsettled look, say during a checkout, is retried as if a change was reported.
    fn finish_look(&mut self, started: Instant, ended: Instant, settled: bool) {
        (self.ended, self.took, self.reported) = (ended, ended - started, !settled);
        self.git_locked_since = None;
    }
}

fn draw_pane(pane: &mut dyn Pane, screen: &mut Screen, help: Option<&mut Page>) {
    if let Some(page) = help {
        page.draw(screen);
    } else if let Some(note) = pane.notice().filter(|_| screen.h > 1) {
        draw_notice(screen, note, |screen| pane.draw(screen));
    } else {
        pane.draw(screen);
    }
}

fn draw_notice(screen: &mut Screen, note: &str, draw: impl FnOnce(&mut Screen)) {
    // Leave room for the notice rather than covering a selectable row or picture.
    screen.h -= 1;
    draw(screen);
    screen.h += 1;
    let margin = usize::from(screen.w >= note.len() + 2);
    screen.put_fit(margin, screen.h - 1, screen.w - margin * 2, note, Style::fg(YELLOW));
}

fn draw_git_wait(pane: &mut dyn Pane, screen: &mut Screen) {
    if screen.h > 0 {
        draw_notice(screen, WAITING_FOR_GIT, |screen| draw_pane(pane, screen, None));
    }
}

fn hover_at(pane: &dyn Pane, help: Option<&Page>, x: usize, y: usize) -> Option<(usize, Range<usize>)> {
    let span = match help {
        Some(page) => page.hover_span(x, y),
        None => pane.hover_span(x, y),
    };
    span.map(|span| (y, span))
}

fn terminal_reply(screen: &mut Screen, key: Key) -> bool {
    match key {
        Key::Background(rgb) => screen.set_background(rgb),
        Key::Pictures(able) => screen.set_pictures(able),
        Key::CellSize(width, height) => screen.set_cell_size(width, height),
        Key::Answered => screen.answered(),
        _ => return false,
    }
    true
}

/// Routes terminal replies and help keys before the pane sees them.
fn route_key(pane: &mut dyn Pane, screen: &mut Screen, help: &mut Option<Page>, key: Key) -> After {
    if terminal_reply(screen, key) {
        return After::Stay;
    }
    match (key, &mut *help) {
        (Key::Ctrl(b'c') | Key::Eof, _) => After::Leave,
        // The pane still sees focus changes while its help is up.
        (Key::Focus(_), _) => pane.key(key),
        (_, Some(page)) => {
            if !page.scroll(key, screen.h) {
                *help = None;
            }
            After::Stay
        }
        (Key::Char('?'), None) => {
            if Page::fits(screen) {
                *help = Some(pane.help());
            }
            After::Stay
        }
        _ => pane.key(key),
    }
}

fn watch_idle(watcher: &Watcher, keep_polling: bool) -> Option<Duration> {
    let scope = watcher.scope();
    if !keep_polling && scope.complete && scope.roots.is_empty() {
        None
    } else if watcher.fd().is_some() && scope.complete {
        Some(WATCHED)
    } else {
        Some(UNWATCHED)
    }
}

pub(super) fn watch_pane(pane: &mut dyn Pane, check_on_open: bool) -> (Watcher, Option<Pace>) {
    let mut watcher = Watcher::new(pane.watch_scope());
    let mut pace = watch_idle(&watcher, check_on_open).map(|idle| Pace::new(idle, Instant::now()));
    if check_on_open {
        check_watched_pane(pane, &mut watcher, &mut pace, true);
    } else if let Some(timing) = &mut pace
        && pane.git_busy()
    {
        timing.block_on_git(Instant::now());
    }
    (watcher, pace)
}

pub(super) fn check_watched_pane(
    pane: &mut dyn Pane,
    watcher: &mut Watcher,
    pace: &mut Option<Pace>,
    keep_polling: bool,
) -> bool {
    let Some(timing) = pace else { return false };
    let started = Instant::now();
    if pane.git_busy() {
        return timing.block_on_git(started);
    }
    let settled = pane.check();
    // A snapshot cannot see edits made just after it. If coverage changed,
    // check again under the new scope rather than relying on the old event queue.
    let handoff = watcher.refresh(pane.watch_scope());
    if let Some(idle) = watch_idle(watcher, keep_polling) {
        timing.idle = idle;
        timing.finish_look(started, Instant::now(), settled && !handoff);
    } else {
        *pace = None;
    }
    true
}

fn draw_loading(screen: &mut Screen, dir: Option<&Path>, w: usize, h: usize) {
    screen.resize(w, h);
    let Some(dir) = dir.filter(|_| h > 1) else {
        screen.put_fit(0, 0, w, "Reading…", ui::FAINT);
        return;
    };
    let absolute = dir.canonicalize().ok();
    let dir = absolute.as_deref().unwrap_or(dir);
    let project = dir.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(dir);
    let name = project.file_name().unwrap_or(project.as_os_str()).to_string_lossy();
    let room = w.saturating_sub(2);
    ui::section_heading(screen, 0, room, &name);
    screen.put_fit(1, 1, room, "Reading…", ui::FAINT);
}

/// A worker manages the terminal while the main thread opens the pane.
/// Closing the pipe stops the worker before the main thread uses the terminal again.
fn with_loading<T>(
    screen: &mut Screen,
    dir: Option<&Path>,
    terminal: &mut Session,
    input: &mut Input,
    delay: Duration,
    open: impl FnOnce() -> T,
) -> T {
    let Ok((ready, done)) = io::pipe() else { return open() };
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let delay = Instant::now() + delay;
            let mut painted = false;
            let mut keys = Vec::new();
            while !term::TERMINATED.load(Ordering::Relaxed) {
                let timeout = if painted { -1 } else { milliseconds(delay.saturating_duration_since(Instant::now())) };
                match terminal.wait_for_open(timeout, ready.as_raw_fd()) {
                    Wait::Woken | Wait::External => return,
                    Wait::Closed => break,
                    Wait::Timeout => {
                        let (w, h) = terminal.size();
                        draw_loading(screen, dir, w, h);
                        if screen.flush(|bytes| terminal.write(bytes)).is_err() {
                            break;
                        }
                        painted = true;
                    }
                    Wait::Input => {
                        input.receive(&mut terminal.keyboard, &mut keys);
                        if keys.iter().any(|key| matches!(key, Key::Ctrl(b'c') | Key::Eof)) {
                            break;
                        }
                        for key in keys.drain(..) {
                            terminal_reply(screen, key);
                        }
                    }
                    Wait::Interrupted => {}
                }
            }
            term::TERMINATED.store(true, Ordering::Relaxed);
            terminal.restore();
        });
        let opened = open();
        drop(done);
        opened
    })
}

pub fn run(start: Start<'_>, open: &Open<'_>) -> Result<(), String> {
    let mut terminal = match Session::open() {
        Ok(terminal) => terminal,
        Err(e) => {
            // An invalid command path has the same error with or without a terminal.
            open()?;
            return Err(format!("cannot use this terminal: {e}"));
        }
    };
    run_entered(start, open, &mut terminal)
}

fn run_entered(start: Start<'_>, open: &Open<'_>, terminal: &mut Session) -> Result<(), String> {
    let check_on_open = matches!(start, Start::Check);
    let mut next = if let Start::Welcome(open) = start { Some(open) } else { None };
    let mut screen = Screen::new();
    let mut input = Input::default();
    let load = |open: &Open<'_>| {
        let mut pane = open()?;
        let (watcher, pace) = watch_pane(pane.as_mut(), check_on_open);
        Ok::<_, String>((pane, watcher, pace))
    };
    let opened = match start {
        Start::Reading(dir) => with_loading(&mut screen, Some(dir), terminal, &mut input, LOADING_DELAY, || load(open)),
        Start::Check => with_loading(&mut screen, None, terminal, &mut input, LOADING_DELAY, || load(open)),
        _ => load(open),
    };
    if term::TERMINATED.load(Ordering::Relaxed) {
        return Ok(());
    }
    let (mut pane, mut watcher, mut pace) = opened?;
    let mut help = None;
    let mut keys = Vec::new();
    let mut redraw = true;
    let mut frame_due = None;
    let mut tracking = false;
    let mut pointer = None;
    let mut highlighted = None;
    'run: while !term::TERMINATED.load(Ordering::Relaxed) {
        if term::RESIZED.swap(false, Ordering::Relaxed) || screen.w == 0 {
            let (w, h) = terminal.size();
            screen.resize(w, h);
            pointer = None;
            if !Page::fits(&screen) {
                help = None;
            }
            redraw = true;
        }
        if std::mem::take(&mut redraw) {
            let want_motion = pane.tracks_hover() || help.is_some();
            if tracking != want_motion {
                terminal.track_motion(want_motion).map_err(|e| format!("cannot configure mouse tracking: {e}"))?;
                tracking = want_motion;
            }
            if help.is_none() && pace.as_ref().is_some_and(Pace::waiting_for_git) {
                draw_git_wait(pane.as_mut(), &mut screen);
            } else {
                draw_pane(pane.as_mut(), &mut screen, help.as_mut());
            }
            highlighted = pointer.and_then(|(x, y)| hover_at(pane.as_ref(), help.as_ref(), x, y));
            if let Some((y, span)) = &highlighted {
                screen.underline(*y, span.clone());
            }
            if let Err(e) = screen.flush(|bytes| terminal.write(bytes)) {
                if term::TERMINATED.load(Ordering::Relaxed) {
                    break;
                }
                return Err(format!("cannot write to this terminal: {e}"));
            }
            frame_due = if help.is_some() { None } else { pane.next_frame().map(|wait| Instant::now() + wait) };
        }

        let now = Instant::now();
        let wait = pace
            .as_ref()
            .map(|pace| pace.until_look(now))
            .into_iter()
            .chain(frame_due.map(|due: Instant| due.saturating_duration_since(now)))
            .min();
        let timeout_ms = wait.map_or(-1, milliseconds);
        match terminal.wait(timeout_ms, watcher.fd(), pane.external_fd()) {
            Wait::Input => {
                input.receive(&mut terminal.keyboard, &mut keys);
                let mut overlay_changed = false;
                for key in keys.drain(..) {
                    if let Key::Hover(x, y) = key {
                        if tracking && !overlay_changed {
                            pointer = Some((x, y));
                            redraw |= hover_at(pane.as_ref(), help.as_ref(), x, y) != highlighted;
                        }
                        continue;
                    }
                    if !matches!(key, Key::Background(_) | Key::Pictures(_) | Key::CellSize(..) | Key::Answered) {
                        pointer = None;
                        redraw |= highlighted.is_some();
                    }
                    if overlay_changed && matches!(key, Key::Click(..)) {
                        continue; // Wait for a frame showing the new click targets.
                    }
                    let had_help = help.is_some();
                    match route_key(pane.as_mut(), &mut screen, &mut help, key) {
                        After::Ignored => {}
                        After::Stay => redraw = true,
                        After::Help => {
                            if Page::fits(&screen) {
                                help = Some(pane.help());
                                redraw = true;
                            }
                        }
                        After::Leave => {
                            if !matches!(key, Key::Ctrl(b'c') | Key::Eof)
                                && let Some(open) = next.take()
                            {
                                (pane, watcher, pace) = load(&open)?;
                                redraw = true;
                            } else {
                                break 'run;
                            }
                        }
                    }
                    overlay_changed |= had_help != help.is_some();
                }
            }
            Wait::Woken => {
                watcher.drain();
                pane.filesystem_changed();
                if let Some(pace) = &mut pace {
                    pace.report_change();
                }
            }
            Wait::External => match pane.check_external() {
                External::Unchanged => {}
                External::Redraw => redraw = true,
                External::NewScope => {
                    (watcher, pace) = watch_pane(pane.as_mut(), check_on_open);
                    if !check_on_open && let Some(pace) = &mut pace {
                        pace.report_change();
                    }
                    help = help.map(|_| pane.help());
                    redraw = true;
                }
            },
            Wait::Timeout | Wait::Interrupted => {}
            Wait::Closed => return Ok(()),
        }
        if term::TERMINATED.load(Ordering::Relaxed) {
            break;
        }
        let now = Instant::now();
        redraw |= frame_due.is_some_and(|due| due <= now);
        if pace.as_ref().is_some_and(|pace| pace.until_look(now).is_zero()) {
            redraw |= check_watched_pane(pane.as_mut(), &mut watcher, &mut pace, check_on_open);
        }
    }
    screen.release_pictures(|bytes| terminal.finish(bytes));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{Read, Write};

    use crate::{fixture, git::Repo, outline::Outline};

    const TEST_WAIT: Duration = Duration::from_secs(60);

    fn read_until(master: &mut File, text: &str) -> String {
        let until = Instant::now() + TEST_WAIT;
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains(text) {
            let left = until.saturating_duration_since(Instant::now());
            let mut fds = [libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            if left.is_zero() {
                break;
            }
            match term::wait_ready(&mut fds, milliseconds(left)) {
                0 => break,
                n if n < 0 => {
                    let error = io::Error::last_os_error();
                    assert_eq!(error.kind(), io::ErrorKind::Interrupted, "{error}");
                    continue;
                }
                _ => {}
            }
            let mut bytes = [0; 512];
            match master.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => panic!("cannot read test terminal: {e}"),
            }
        }
        String::from_utf8_lossy(&output).into_owned()
    }

    #[test]
    fn a_fast_open_does_not_draw_a_loading_frame() {
        let mut screen = Screen::new();
        let (mut terminal, _master, _signals) = Session::test_pty();
        let value = with_loading(&mut screen, None, &mut terminal, &mut Input::default(), TEST_WAIT, || 42);
        assert_eq!(value, 42);
        assert_eq!((screen.w, screen.h), (0, 0));
        assert!(screen.emitted().is_empty());
    }

    #[test]
    fn a_one_row_pane_says_it_is_reading_instead_of_showing_only_the_project() {
        let mut screen = Screen::new();
        draw_loading(&mut screen, Some(Path::new(".")), 10, 1);
        assert_eq!(screen.text(), ["Reading…"]);
    }

    #[test]
    fn a_slow_first_check_has_a_loading_frame() {
        struct Checked(File);
        impl Pane for Checked {
            fn check(&mut self) -> bool {
                let output = read_until(&mut self.0, "Reading…");
                assert!(output.contains("Reading…"), "{output:?}");
                true
            }
            fn draw(&mut self, screen: &mut Screen) {
                screen.put(0, 0, screen.w, "Ready", ui::FAINT);
                self.0.write_all(b"q").unwrap();
            }
            fn help(&self) -> Page {
                Page::new(vec![])
            }
            fn key(&mut self, key: Key) -> After {
                if key == Key::Char('q') { After::Leave } else { After::Ignored }
            }
        }

        let (mut terminal, master, _signals) = Session::test_pty();
        run_entered(Start::Check, &|| Ok(Box::new(Checked(master.try_clone().unwrap()))), &mut terminal).unwrap();
    }

    #[test]
    fn an_ignored_key_leaves_a_frame_alone_but_a_terminal_reply_redraws_it() {
        struct Quiet {
            master: File,
            first: bool,
        }
        impl Pane for Quiet {
            fn draw(&mut self, screen: &mut Screen) {
                let text = if std::mem::take(&mut self.first) {
                    self.master.write_all(b"z").unwrap();
                    "Ready"
                } else if screen.light_background() {
                    "Light"
                } else {
                    "Wrong"
                };
                screen.clear();
                screen.put(0, 0, screen.w, text, ui::FAINT);
            }
            fn help(&self) -> Page {
                Page::new(vec![])
            }
            fn key(&mut self, key: Key) -> After {
                if key == Key::Char('z') {
                    // Arrives in a separate input batch, even on a loaded machine.
                    self.master.write_all(b"\x1b]11;rgb:ff/ff/ff\x1b\\").unwrap();
                }
                if key == Key::Char('q') { After::Leave } else { After::Ignored }
            }
        }

        let (mut terminal, mut master, _signals) = Session::test_pty();
        let pane_master = master.try_clone().unwrap();
        let output = std::thread::scope(|scope| {
            let read = scope.spawn(|| {
                let output = read_until(&mut master, "Light");
                master.write_all(b"q").unwrap();
                output
            });
            let open =
                || Ok(Box::new(Quiet { master: pane_master.try_clone().unwrap(), first: true }) as Box<dyn Pane>);
            run_entered(Start::Loaded, &open, &mut terminal).unwrap();
            read.join().unwrap()
        });
        assert!(output.contains("Ready") && output.contains("Light") && !output.contains("Wrong"), "{output:?}");
    }

    #[test]
    fn quiet_projects_get_a_look_even_without_a_report() {
        let start = Instant::now();
        for (idle, seconds) in [(WATCHED, 30), (UNWATCHED, 2)] {
            let pace = Pace::new(idle, start);
            assert_eq!(pace.until_look(start + Duration::from_secs(1)), Duration::from_secs(seconds - 1));
            assert_eq!(pace.until_look(start + Duration::from_secs(seconds)), Duration::ZERO);
        }
    }

    #[test]
    fn a_report_is_due_immediately_if_the_last_look_has_had_enough_rest() {
        let start = Instant::now();
        let mut pace = Pace::new(WATCHED, start);
        pace.report_change();
        assert_eq!(pace.until_look(start + Duration::from_millis(20)), Duration::from_millis(100));
        assert_eq!(pace.until_look(start + Duration::from_secs(1)), Duration::ZERO);
    }

    #[test]
    fn a_slow_look_rests_four_times_as_long_even_when_changes_are_reported() {
        let start = Instant::now();
        let ended = start + Duration::from_secs(1);
        let mut pace = Pace::new(UNWATCHED, start);
        pace.finish_look(start, ended, true);
        assert_eq!(pace.until_look(ended), Duration::from_secs(4));
        pace.report_change();
        assert_eq!(pace.until_look(ended + Duration::from_secs(1)), Duration::from_secs(3));
        assert_eq!(pace.until_look(ended + Duration::from_secs(4)), Duration::ZERO);
    }

    #[test]
    fn an_unsettled_look_is_retried_soon_and_a_settled_look_goes_back_to_idle() {
        let start = Instant::now();
        let ended = start + Duration::from_millis(10);
        let mut pace = Pace::new(WATCHED, start);
        pace.finish_look(start, ended, false);
        assert_eq!(pace.until_look(ended), Duration::from_millis(120));
        let retry = ended + Duration::from_millis(120);
        assert_eq!(pace.until_look(retry), Duration::ZERO);
        pace.finish_look(retry, retry + Duration::from_millis(10), true);
        assert_eq!(pace.until_look(retry + Duration::from_millis(10)), Duration::from_secs(30));
    }

    #[test]
    fn a_brief_git_lock_does_not_get_a_waiting_frame() {
        let start = Instant::now();
        let mut pace = Pace::new(WATCHED, start);
        assert!(!pace.block_on_git(start));
        assert!(!pace.waiting_for_git());
        assert_eq!(pace.until_look(start), GIT_GRACE);

        let unlocked = start + GIT_GRACE;
        pace.finish_look(unlocked, unlocked, true);
        assert!(!pace.waiting_for_git());
        assert_eq!(pace.until_look(unlocked), WATCHED);
    }

    #[test]
    fn a_persistent_git_lock_gets_one_waiting_frame_and_slow_probes() {
        let start = Instant::now();
        let mut pace = Pace::new(WATCHED, start);
        assert!(!pace.block_on_git(start));

        let grace = start + GIT_GRACE;
        assert_eq!(pace.until_look(grace), Duration::ZERO);
        assert!(pace.block_on_git(grace));
        assert!(pace.waiting_for_git());
        pace.report_change();
        assert_eq!(pace.until_look(grace), GIT_RETRY);

        let retry = grace + GIT_RETRY;
        assert_eq!(pace.until_look(retry), Duration::ZERO);
        assert!(!pace.block_on_git(retry));
        assert!(pace.waiting_for_git());
        assert_eq!(pace.until_look(retry), GIT_RETRY);

        let unlocked = retry + GIT_RETRY;
        pace.finish_look(unlocked, unlocked, true);
        assert!(!pace.waiting_for_git());
        assert_eq!(pace.until_look(unlocked), WATCHED);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn changed_git_exclusions_get_one_followup_snapshot_under_the_new_stream() {
        let f = fixture::Fixture::new();
        f.write(".gitignore", "build/\n");
        f.write("build/source.rs", "first\n");
        f.git(&["add", "-f", "--", "build/source.rs"]);
        f.commit("first");
        let mut pane = Outline::new(Repo::open(&f.root).unwrap(), false);
        let (mut watcher, mut pace) = watch_pane(&mut pane, false);
        let original = watcher.fd().unwrap();
        assert!(watcher.scope().ignored.is_empty());

        f.write("build/generated/output", "first build\n");
        check_watched_pane(&mut pane, &mut watcher, &mut pace, false);
        let ignored = pane.watch_scope().ignored;
        assert_eq!(ignored.as_ref(), [f.root.join("build/generated")]);
        let current = watcher.fd().unwrap();
        assert_ne!(original, current);
        assert!(pace.as_ref().unwrap().reported);

        // A force-tracked file under the ignore rule is still source. A
        // post-snapshot write must be noticed even without a wakeup byte.
        f.write("build/source.rs", "first\nsecond\n");
        check_watched_pane(&mut pane, &mut watcher, &mut pace, false);
        let mut screen = Screen::sized(80, 12);
        pane.draw(&mut screen);
        assert!(screen.text().iter().any(|row| row.contains("source.rs") && row.ends_with("+1")));
        assert!(std::rc::Rc::ptr_eq(&ignored, &pane.watch_scope().ignored));
        assert_eq!(watcher.fd(), Some(current), "unchanged exclusions must not restart the stream");
        assert!(!pace.as_ref().unwrap().reported, "an unchanged scope must not request another check");
    }

    #[test]
    fn help_scrolls_by_a_page_and_escape_restores_the_outline() {
        let f = fixture::Fixture::new();
        f.write("page.txt", "a file\n");
        let mut pane = Outline::new(Repo::open(&f.root).unwrap(), false);
        let mut screen = Screen::sized(65, 12);
        let mut help = None;
        draw_pane(&mut pane, &mut screen, help.as_mut());
        let before = screen.text();

        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::Char('?')), After::Stay);
        draw_pane(&mut pane, &mut screen, help.as_mut());
        let first = screen.text();
        assert!(first[0].trim_start().starts_with("Files"));
        assert!(first[11].contains("Esc close") && first[11].ends_with("q close"));
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::PgDn), After::Stay);
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert_eq!(screen.text()[0], first[10]);
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::PgUp), After::Stay);
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert_eq!(screen.text(), first);
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::Esc), After::Stay);
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert_eq!(screen.text(), before);
    }

    #[test]
    fn help_cannot_blank_a_one_row_pane() {
        let f = fixture::Fixture::folder();
        f.write("page.txt", "a file\n");
        let mut pane = Outline::new(Repo::open(&f.root).unwrap(), false);
        let mut screen = Screen::sized(40, 1);
        let mut help = None;
        draw_pane(&mut pane, &mut screen, help.as_mut());
        let before = screen.text();
        assert!(before[0].contains("page.txt"));
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::Char('?')), After::Stay);
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert_eq!(screen.text(), before);
        assert!(help.is_none());
    }

    #[test]
    fn help_does_not_cover_a_pane_too_narrow_to_show_how_to_close_it() {
        let f = fixture::Fixture::folder();
        f.write("a.txt", "a file\n");
        let mut pane = Outline::new(Repo::open(&f.root).unwrap(), false);
        let mut screen = Screen::sized(8, 5);
        let mut help = None;
        draw_pane(&mut pane, &mut screen, None);
        let before = screen.text();
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::Char('?')), After::Stay);
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert_eq!(screen.text(), before);

        screen.resize(9, 5);
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::Char('?')), After::Stay);
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert_eq!(screen.text()[4], " q close");
    }

    #[test]
    fn help_uses_the_nested_reader_page_and_terminal_quit_bypasses_it() {
        let f = fixture::Fixture::new();
        f.write("page.txt", "a file\n");
        let mut pane = Outline::new(Repo::open(&f.root).unwrap(), false);
        let mut screen = Screen::sized(65, 12);
        let mut help = None;
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::Enter), After::Stay);
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::Char('?')), After::Stay);
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert!(screen.text()[0].trim_start().starts_with("Lines"));
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::Ctrl(b'c')), After::Leave);
        assert_eq!(route_key(&mut pane, &mut screen, &mut help, Key::Eof), After::Leave);
    }
}
