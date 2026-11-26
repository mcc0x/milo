//! Runs terminal panes with shared input, help, redraws and file watching.

use std::collections::VecDeque;
use std::io;
use std::ops::Range;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::help::Page;
use crate::keys::{ConfirmClick, Key};
use crate::preferences::ProjectMode;
use crate::reader::OpenReader;
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
    Open(crate::home::Destination),
    OpenWeb,
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
    /// The help page is about to cover the pane; stop user-controlled playback.
    fn cover(&mut self) {}
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

    /// A new scope after a key changes what the pane is watching.
    fn watch_scope_after_input(&self, _current: &Scope) -> Option<Scope> {
        None
    }

    /// The reader opener belongs to the terminal pane, not its current project.
    fn reader_opener(&mut self) -> Option<&mut OpenReader> {
        None
    }

    /// The worktree and shell-follow choice adopted when a plain folder became a repository.
    fn promoted_worktree(&self) -> Option<(&Path, ProjectMode)> {
        None
    }

    /// When Git is updating the index, keep the previous snapshot.
    fn git_busy(&self) -> bool {
        false
    }

    /// Takes in changes from outside. False asks the loop to check again soon.
    fn check(&mut self) -> bool {
        true
    }

    /// Takes an automatic look with the live listener status, independent of the check cadence.
    fn check_with_watcher(&mut self, _watching: bool, _started: Instant) -> bool {
        self.check()
    }

    /// Readable once a check that `check` left running away from the loop has
    /// its answer. The loop takes no other look until then.
    fn unfinished_check(&self) -> Option<RawFd> {
        None
    }

    /// Takes in what that check found, or returns `None` while there is nothing
    /// to take in. False asks the loop to check again soon.
    fn finish_check(&mut self) -> Option<bool> {
        None
    }

    fn external_fd(&self) -> Option<RawFd> {
        None
    }

    fn check_external(&mut self) -> External {
        External::Unchanged
    }

    /// A program to lend the terminal to until it ends, such as this pane's
    /// counterpart on the computer its shell reached with `milo ssh`.
    fn lend(&mut self) -> Option<std::process::Command> {
        None
    }

    /// While the terminal is lent, whether the lent program may keep it.
    fn still_lent(&mut self) -> bool {
        true
    }

    /// The lent program ended: on its own when `finished`, otherwise through
    /// a failure or a lost connection. The pane draws afresh.
    fn returned(&mut self, _finished: bool) -> After {
        After::Stay
    }

    /// Records a filesystem notification before the next scheduled check.
    fn filesystem_changed(&mut self) {}

    /// How long the frame stays right when nothing happens.
    fn next_frame(&self) -> Option<Duration> {
        None
    }

    /// The pane is taking text: `?`, `q` and Ctrl-C are keys for it, and pasted text is kept for it.
    fn editing(&self) -> bool {
        false
    }

    /// The pane holds unsaved text even where it is not `editing`: Ctrl-C is a key for it,
    /// so that it can ask before closing.
    fn holds_drafts(&self) -> bool {
        false
    }

    /// Takes text pasted while `editing`, or `None` if the paste was too long to keep.
    fn paste(&mut self, _text: Option<&str>) -> After {
        After::Ignored
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NextLook {
    Idle,
    Prompt,
    ReconcileOpen,
}

/// When to look for changes again, with or without a report from the watcher.
pub(super) struct Pace {
    idle: Duration,
    ended: Instant,
    took: Duration,
    next: NextLook,
    git_locked_since: Option<Instant>,
    /// When a check that is still running away from the loop started.
    away: Option<Instant>,
    /// The watcher reported a change while that check was away.
    moved_while_away: bool,
}

impl Pace {
    fn new(idle: Duration, now: Instant) -> Self {
        Self {
            idle,
            ended: now,
            took: Duration::ZERO,
            next: NextLook::Idle,
            git_locked_since: None,
            away: None,
            moved_while_away: false,
        }
    }

    #[cfg(test)]
    pub(super) fn idle(&self) -> Duration {
        self.idle
    }

    fn report_change(&mut self) {
        if self.next == NextLook::ReconcileOpen {
            // Opening can do more work than an ordinary check. A real change
            // need not wait out that estimate, but later checks use their own cost.
            self.took = Duration::ZERO;
        }
        self.next = NextLook::Prompt;
        self.moved_while_away |= self.away.is_some();
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
        let interval = if self.next == NextLook::Idle { self.idle } else { MIN_LOOK_INTERVAL };
        let rest = interval.max(self.took * REST);
        rest.saturating_sub(now.saturating_duration_since(self.ended))
    }

    /// The check carries on away from the loop, which takes no look until it is finished.
    fn leave_running(&mut self, started: Instant) {
        self.away = Some(started);
        self.git_locked_since = None;
    }

    /// An unsettled look, say during a checkout, is retried as if a change was reported.
    fn finish_look(&mut self, started: Instant, ended: Instant, settled: bool) {
        (self.ended, self.took) = (ended, ended - started);
        // A change reported while a check was away can be newer than what it read.
        let settled = settled && !std::mem::take(&mut self.moved_while_away);
        self.next = if settled { NextLook::Idle } else { NextLook::Prompt };
        self.git_locked_since = None;
        self.away = None;
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
    if screen.h == 1 {
        screen.clear();
        screen.put_fit(0, 0, screen.w, WAITING_FOR_GIT, Style::fg(YELLOW));
    } else if screen.h > 1 {
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
    let editing = pane.editing();
    let asks = editing || pane.holds_drafts();
    match (key, &mut *help) {
        (Key::Eof, _) => After::Leave,
        // A pane with unsaved text asks before it closes.
        (Key::Ctrl(b'c'), None) if asks => pane.key(key),
        (Key::Ctrl(b'c'), _) if !asks => After::Leave,
        // The pane still sees focus changes while its help is up.
        (Key::Focus(_), _) => pane.key(key),
        (_, Some(page)) => {
            if !page.scroll(key, screen.h) {
                *help = None;
            }
            After::Stay
        }
        (Key::Char('?'), None) if !editing => {
            if Page::fits(screen) {
                pane.cover();
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
    } else if let Some(timing) = &mut pace {
        // The first snapshot predates the watcher. Look again soon even if
        // edits in that gap left no notification.
        timing.next = NextLook::ReconcileOpen;
        if pane.git_busy() {
            timing.block_on_git(Instant::now());
        }
    }
    (watcher, pace)
}

fn watch_opened_pane(pane: &mut dyn Pane, check_on_open: bool, opened: Duration) -> (Watcher, Option<Pace>) {
    let (watcher, mut pace) = watch_pane(pane, check_on_open);
    if let Some(timing) = &mut pace
        && timing.next == NextLook::ReconcileOpen
    {
        timing.took = opened;
    }
    (watcher, pace)
}

pub(super) fn refresh_watch_after_input(
    pane: &dyn Pane,
    watcher: &mut Watcher,
    pace: &mut Option<Pace>,
    keep_polling: bool,
) {
    let Some(scope) = pane.watch_scope_after_input(watcher.scope()) else { return };
    let handoff = watcher.refresh(scope);
    if let Some(idle) = watch_idle(watcher, keep_polling) {
        let timing = pace.get_or_insert_with(|| Pace::new(idle, Instant::now()));
        timing.idle = idle;
        if handoff && timing.next == NextLook::Idle {
            // The key's snapshot preceded the new coverage.
            timing.next = NextLook::Prompt;
        }
    } else {
        *pace = None;
    }
}

pub(super) fn check_watched_pane(
    pane: &mut dyn Pane,
    watcher: &mut Watcher,
    pace: &mut Option<Pace>,
    keep_polling: bool,
) -> bool {
    check_watched_pane_at(pane, watcher, pace, keep_polling, Instant::now)
}

pub(super) fn check_watched_pane_at(
    pane: &mut dyn Pane,
    watcher: &mut Watcher,
    pace: &mut Option<Pace>,
    keep_polling: bool,
    clock: impl Fn() -> Instant,
) -> bool {
    let Some(timing) = pace else { return false };
    let started = clock();
    if pane.git_busy() {
        return timing.block_on_git(started);
    }
    let settled = pane.check_with_watcher(watcher.fd().is_some(), started);
    if pane.unfinished_check().is_some() {
        timing.leave_running(started);
    } else {
        finish_watched_look(pane, watcher, pace, keep_polling, (started, clock()), settled);
    }
    true
}

/// Ends a look that `started` and `ended` at `span`, whether it ran in the loop or away from it.
fn finish_watched_look(
    pane: &dyn Pane,
    watcher: &mut Watcher,
    pace: &mut Option<Pace>,
    keep_polling: bool,
    span: (Instant, Instant),
    settled: bool,
) {
    let Some(timing) = pace else { return };
    // A snapshot cannot see edits made just after it. If coverage changed,
    // check again under the new scope rather than relying on the old event queue.
    let handoff = watcher.refresh(pane.watch_scope());
    if let Some(idle) = watch_idle(watcher, keep_polling) {
        timing.idle = idle;
        timing.finish_look(span.0, span.1, settled && !handoff);
    } else {
        *pace = None;
    }
}

/// Takes in a check that was left running, once it has an answer or the pane has dropped it.
fn finish_away_check(
    pane: &mut dyn Pane,
    watcher: &mut Watcher,
    pace: &mut Option<Pace>,
    keep_polling: bool,
    clock: impl Fn() -> Instant,
) -> bool {
    let started = pace.as_ref().and_then(|pace| pace.away);
    // A pace made anew, as for another scope, does not know of a check its pane still has away.
    if started.is_none() && pane.unfinished_check().is_none() {
        return false;
    }
    let settled = match pane.finish_check() {
        Some(settled) => settled,
        // The pane loaded by itself meanwhile, and nothing says that was the latest.
        None if pane.unfinished_check().is_none() => false,
        None => return false,
    };
    let ended = clock();
    finish_watched_look(pane, watcher, pace, keep_polling, (started.unwrap_or(ended), ended), settled);
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
    delay: Duration,
    open: impl FnOnce() -> T,
) -> (T, Option<bool>) {
    let Ok((ready, done)) = io::pipe() else { return (open(), None) };
    let mut focused = None;
    let value = std::thread::scope(|scope| {
        let focus = &mut focused;
        scope.spawn(move || {
            let delay = Instant::now() + delay;
            let mut painted = false;
            'loading: while !terminal.stopping() {
                let timeout = if painted { -1 } else { milliseconds(delay.saturating_duration_since(Instant::now())) };
                let check_size = match terminal.wait_for_open(timeout, ready.as_raw_fd()) {
                    Wait::Woken | Wait::External | Wait::Checked => return,
                    Wait::Closed => break,
                    Wait::Timeout | Wait::Interrupted => true,
                    Wait::Input => {
                        for key in terminal.receive() {
                            if matches!(key, Key::Ctrl(b'c') | Key::Eof) {
                                break 'loading;
                            }
                            terminal_reply(screen, key);
                            if let Key::Focus(value) = key {
                                *focus = Some(value);
                            }
                        }
                        !painted
                    }
                };
                if check_size && Instant::now() >= delay {
                    let (w, h) = terminal.size();
                    if !painted || (screen.w, screen.h) != (w, h) {
                        draw_loading(screen, dir, w, h);
                        if let Err(e) = screen.flush(|bytes| terminal.write_while_open(bytes, ready.as_raw_fd())) {
                            if e.kind() == io::ErrorKind::Interrupted && !terminal.stopping() {
                                // The cancelled write may not have erased the previous pane.
                                // Discard the diff's baseline and erase on the next frame.
                                screen.resize(w, h);
                                return;
                            }
                            break;
                        }
                        painted = true;
                    }
                }
            }
            term::TERMINATED.store(true, Ordering::Relaxed);
            terminal.restore();
        });
        let opened = open();
        drop(done);
        opened
    });
    (value, focused)
}

pub fn run(start: Start<'_>, open: &Open<'_>) -> Result<(), String> {
    let mut terminal = match Session::open() {
        Ok(terminal) => terminal,
        Err(e) => {
            // An invalid command path has the same error with or without a terminal.
            open()?;
            return Err(format!("cannot use this terminal: {e}{}", without_a_terminal()));
        }
    };
    let result = run_entered(start, open, &mut terminal);
    terminal.restore();
    // The terminal is the shell's again, so this line stays on screen. It
    // holds a file's name, and it must not wait for a terminal that has stopped reading.
    for note in crate::reader::take_rescue_notes() {
        let safe: String = note.chars().filter(|c| !c.is_control()).collect();
        terminal.finish(format!("milo: {safe}\n").as_bytes());
    }
    result
}

/// What to do instead, for a pane started where there is no terminal to
/// draw in or read keys from: a pipe, a file, or a coding agent's shell.
fn without_a_terminal() -> &'static str {
    use std::io::IsTerminal as _;
    const PRINTS: &str =
        "; a pane needs an interactive terminal. milo doctor and milo COMMAND --help print without one";
    const IN_REX: &str = "; a pane needs an interactive terminal. milo doctor and milo COMMAND --help print without one, \
                          and rex run -s \"$REX_BLOCK\" -- milo COMMAND opens a pane in a Rex tab";
    const IN_OLDER_REX: &str = "; a pane needs an interactive terminal. milo doctor and milo COMMAND --help print without one, \
                                and rex run -s \"$REX_SESSION\" -- milo COMMAND opens a pane in a Rex tab";
    if io::stdout().is_terminal() && io::stdin().is_terminal() {
        ""
    } else if !crate::rex::inside() {
        PRINTS
    } else if crate::rex::names_session() {
        IN_OLDER_REX
    } else {
        IN_REX
    }
}

enum Exit {
    Done,
    OpenWeb,
}

fn run_entered(start: Start<'_>, open: &Open<'_>, terminal: &mut Session) -> Result<(), String> {
    let mut screen = Screen::for_terminal(terminal.capabilities());
    let exit = pane_loop(start, open, terminal, &mut screen);
    screen.release_pictures(|bytes| terminal.finish(bytes));
    match exit? {
        Exit::Done => Ok(()),
        Exit::OpenWeb => {
            terminal.restore();
            // Home has no port selector; a second worktree needs its own session.
            crate::web::run(crate::web::Start::Local { open_browser: true, port: 0 })
        }
    }
}

type LoadedPane = (Box<dyn Pane>, Watcher, Option<Pace>);

fn load_pane(
    start: Start<'_>,
    open: &Open<'_>,
    terminal: &mut Session,
    screen: &mut Screen,
) -> (Result<LoadedPane, String>, Option<bool>) {
    let load = || {
        let started = Instant::now();
        let mut pane = open()?;
        let (watcher, pace) = watch_opened_pane(pane.as_mut(), matches!(start, Start::Check), started.elapsed());
        Ok((pane, watcher, pace))
    };
    match start {
        Start::Reading(dir) => with_loading(screen, Some(dir), terminal, LOADING_DELAY, load),
        Start::Check => with_loading(screen, None, terminal, LOADING_DELAY, load),
        _ => (load(), None),
    }
}

fn pane_loop(start: Start<'_>, open: &Open<'_>, terminal: &mut Session, screen: &mut Screen) -> Result<Exit, String> {
    let mut check_on_open = matches!(start, Start::Check);
    let mut next = if let Start::Welcome(open) = start { Some(open) } else { None };
    let mut handoff_click = ConfirmClick::default();
    let (opened, mut focused) = load_pane(start, open, terminal, screen);
    if term::TERMINATED.load(Ordering::Relaxed) {
        return Ok(Exit::Done);
    }
    let (mut pane, mut watcher, mut pace) = opened?;
    if let Some(value) = focused {
        pane.key(Key::Focus(value));
    }
    // Home can open one local pane. Readers and other subviews remain the pane's own.
    let mut parent = None;
    let mut help = None;
    let mut redraw = true;
    let mut frame_due = None;
    let mut tracking = false;
    let mut pointer = None;
    let mut highlighted = None;
    let mut first_frame = true;
    let mut keys = VecDeque::new();
    'run: while !term::TERMINATED.load(Ordering::Relaxed) {
        if let Some(mut lent) = pane.lend() {
            let finished =
                terminal.lend(&mut lent, || pane.still_lent()).map_err(|e| format!("cannot use this terminal: {e}"))?;
            if pane.returned(finished) == After::Leave {
                break;
            }
            // What the shell did meanwhile was kept for now.
            let started = Instant::now();
            if matches!(pane.check_external(), External::NewScope) {
                (watcher, pace) = watch_opened_pane(pane.as_mut(), check_on_open, started.elapsed());
                help = help.map(|_| pane.help());
            }
            // The terminal comes back blank, in Milo's modes but without motion reports.
            (first_frame, tracking, pointer) = (true, false, None);
            keys.clear();
            continue;
        }
        let resized = term::RESIZED.swap(false, Ordering::Relaxed);
        if first_frame || resized {
            first_frame = false;
            let (w, h) = terminal.size();
            screen.resize_interactive(w, h, Instant::now());
            pointer = None;
            if !Page::fits(screen) {
                help = None;
            }
            redraw = true;
        } else if screen.picture_redraw_due().is_some_and(|due| due <= Instant::now()) {
            screen.resume_pictures();
            redraw = true;
        }
        if std::mem::take(&mut redraw) {
            let want_motion = pane.tracks_hover() || help.is_some();
            if tracking != want_motion {
                if let Err(e) = terminal.track_motion(want_motion) {
                    if terminal.stopping() {
                        break;
                    }
                    return Err(format!("cannot configure mouse tracking: {e}"));
                }
                tracking = want_motion;
            }
            let git_wait = help.is_none() && pace.as_ref().is_some_and(Pace::waiting_for_git);
            if git_wait {
                draw_git_wait(pane.as_mut(), screen);
            } else {
                draw_pane(pane.as_mut(), screen, help.as_mut());
            }
            highlighted = pointer
                .filter(|&(_, y)| pane_row_available(screen, &help, &pace, y))
                .and_then(|(x, y)| hover_at(pane.as_ref(), help.as_ref(), x, y));
            if let Some((y, span)) = &highlighted {
                screen.underline(*y, span.clone());
            }
            if let Err(e) = screen.flush(|bytes| terminal.write(bytes)) {
                if terminal.stopping() {
                    break;
                }
                return Err(format!("cannot write to this terminal: {e}"));
            }
            // A pane hidden by help or a one-row Git notice cannot draw and
            // retire an expired mark. Its next visible frame handles it.
            frame_due = if help.is_some() || git_wait && screen.h <= 1 {
                None
            } else {
                pane.next_frame().map(|wait| Instant::now() + wait)
            };
        }

        let now = Instant::now();
        let wait = pace
            .as_ref()
            .filter(|pace| pace.away.is_none())
            .map(|pace| pace.until_look(now))
            .into_iter()
            .chain(frame_due.map(|due: Instant| due.saturating_duration_since(now)))
            .chain(screen.picture_redraw_due().map(|due| due.saturating_duration_since(now)))
            .min();
        let timeout_ms = wait.map_or(-1, milliseconds);
        terminal.keep_pastes(help.is_none() && pane.editing());
        match terminal.wait(timeout_ms, [watcher.fd(), pane.external_fd(), pane.unfinished_check()]) {
            Wait::Input => {
                let mut overlay_changed = false;
                keys.extend(terminal.receive());
                while let Some(key) = keys.pop_front() {
                    if let Key::Hover(x, y) = key {
                        if tracking && !overlay_changed && pane_row_available(screen, &help, &pace, y) {
                            pointer = Some((x, y));
                            redraw |= hover_at(pane.as_ref(), help.as_ref(), x, y) != highlighted;
                        } else if !overlay_changed {
                            pointer = None;
                            redraw |= highlighted.is_some();
                        }
                        continue;
                    }
                    if !matches!(key, Key::Background(_) | Key::Pictures(_) | Key::CellSize(..) | Key::Answered) {
                        pointer = None;
                        redraw |= highlighted.is_some();
                    }
                    if !key.while_obscured() && !pane_row_available(screen, &help, &pace, 0) {
                        continue;
                    }
                    // Skip stale controls and the repeated click that opened another pane.
                    if let Key::Click(x, y) = key
                        && (overlay_changed || !pane_row_available(screen, &help, &pace, y) || handoff_click.is_repeat(x, y))
                    {
                        continue;
                    }
                    // A shortcut must not act on a handoff that has not been painted, or outrun a queued quit.
                    if crate::native::Action::key(key).is_some()
                        && (overlay_changed || keys.iter().any(|key| matches!(key, Key::Ctrl(b'c') | Key::Eof)))
                    {
                        continue;
                    }
                    if let Key::Focus(value) = key {
                        focused = Some(value);
                    }
                    let had_help = help.is_some();
                    let after = match key {
                        Key::Paste => {
                            let text = terminal.pasted();
                            if had_help { After::Ignored } else { pane.paste(text.as_deref()) }
                        }
                        _ => route_key(pane.as_mut(), screen, &mut help, key),
                    };
                    let mut changed_pane = false;
                    match after {
                        After::Ignored => {}
                        After::Stay => redraw = true,
                        // Cancel a handoff before the loader or browser can start.
                        After::Open(_) | After::OpenWeb
                            if keys.iter().any(|key| matches!(key, Key::Ctrl(b'c') | Key::Eof)) =>
                        {
                            break 'run;
                        }
                        After::Open(destination) => {
                            let start = destination.start();
                            let child_checks = matches!(start, Start::Check);
                            let (opened, focus) = load_pane(start, &|| Ok(destination.open()), terminal, screen);
                            if term::TERMINATED.load(Ordering::Relaxed) {
                                break 'run;
                            }
                            let (child, child_watcher, child_pace) = opened?;
                            debug_assert!(parent.is_none());
                            parent = Some((std::mem::replace(&mut pane, child), check_on_open));
                            (watcher, pace) = (child_watcher, child_pace);
                            check_on_open = child_checks;
                            // The loader read this after every key already in the queue.
                            if let Some(value) = focus {
                                keys.push_back(Key::Focus(value));
                            }
                            changed_pane = true;
                        }
                        After::OpenWeb => return Ok(Exit::OpenWeb),
                        After::Help => {
                            if Page::fits(screen) {
                                help = Some(pane.help());
                                redraw = true;
                            }
                        }
                        After::Leave => {
                            if matches!(key, Key::Ctrl(b'c') | Key::Eof) {
                                break 'run;
                            } else if let Some((previous, previous_check)) = parent.take() {
                                pane = previous;
                                check_on_open = previous_check;
                                (watcher, pace) = watch_pane(pane.as_mut(), check_on_open);
                            } else if let Some(open) = next.take() {
                                (pane, watcher, pace) = load_pane(Start::Loaded, &open, terminal, screen).0?;
                            } else {
                                break 'run;
                            }
                            changed_pane = true;
                        }
                    }
                    if changed_pane {
                        if let Some(value) = focused {
                            pane.key(Key::Focus(value));
                        }
                        handoff_click.clear();
                        if let Key::Click(x, y) = key {
                            handoff_click.opened(x, y);
                        }
                        help = None;
                        overlay_changed = true;
                        redraw = true;
                    }
                    overlay_changed |= had_help != help.is_some();
                }
                refresh_watch_after_input(pane.as_ref(), &mut watcher, &mut pace, check_on_open);
            }
            Wait::Woken => {
                watcher.drain();
                pane.filesystem_changed();
                if let Some(pace) = &mut pace {
                    pace.report_change();
                }
            }
            Wait::External => {
                let started = Instant::now();
                match pane.check_external() {
                    External::Unchanged => {}
                    External::Redraw => redraw = true,
                    External::NewScope => {
                        (watcher, pace) = watch_opened_pane(pane.as_mut(), check_on_open, started.elapsed());
                        help = help.map(|_| pane.help());
                        redraw = true;
                    }
                }
            }
            Wait::Interrupted if term::RESIZED.load(Ordering::Relaxed) => continue,
            Wait::Timeout | Wait::Interrupted | Wait::Checked => {}
            Wait::Closed => break,
        }
        if term::TERMINATED.load(Ordering::Relaxed) {
            break;
        }
        redraw |= finish_away_check(pane.as_mut(), &mut watcher, &mut pace, check_on_open, Instant::now);
        let now = Instant::now();
        redraw |= frame_due.is_some_and(|due| due <= now);
        if pace.as_ref().is_some_and(|pace| pace.away.is_none() && pace.until_look(now).is_zero()) {
            redraw |= check_watched_pane(pane.as_mut(), &mut watcher, &mut pace, check_on_open);
        }
    }
    Ok(Exit::Done)
}

fn pane_row_available(screen: &Screen, help: &Option<Page>, pace: &Option<Pace>, y: usize) -> bool {
    y < screen.h && (help.is_some() || y + 1 < screen.h || !pace.as_ref().is_some_and(Pace::waiting_for_git))
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
        let result = with_loading(&mut screen, None, &mut terminal, TEST_WAIT, || 42);
        assert_eq!(result, (42, None));
        assert_eq!((screen.w, screen.h), (0, 0));
        assert!(screen.emitted().is_empty());
    }

    #[test]
    fn the_loading_screen_preserves_the_most_recent_focus_report() {
        let mut screen = Screen::new();
        let (mut terminal, mut master, _signals) = Session::test_pty();
        master.write_all(b"\x1b[O\x1b[I").unwrap();
        let ((), focused) = with_loading(&mut screen, None, &mut terminal, LOADING_DELAY, || {
            let output = read_until(&mut master, "Reading…");
            assert!(output.contains("Reading…"), "{output:?}");
        });
        assert_eq!(focused, Some(true));
    }

    #[test]
    fn a_queued_cancel_does_not_launch_browser_settings() {
        let open = || Ok(Box::new(crate::home::Home::new()) as Box<dyn Pane>);
        for cancel in [false, true] {
            let (mut terminal, mut master, _signals) = Session::test_pty();
            let keys: &[u8] = if crate::rex::inside() { b"\r\x03" } else { b"jjj\r\x03" };
            let keys = &keys[..keys.len() - usize::from(!cancel)];
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let frame = read_until(&mut master, "q quit");
                    assert!(frame.contains("q quit"));
                    master.write_all(keys).unwrap();
                });
                let exit = pane_loop(Start::Loaded, &open, &mut terminal, &mut Screen::new());
                assert!(if cancel { matches!(exit, Ok(Exit::Done)) } else { matches!(exit, Ok(Exit::OpenWeb)) });
            });
        }
    }

    #[test]
    fn native_shortcuts_wait_for_the_pane_and_do_not_outrun_quit() {
        use crate::native::Action;

        struct Probe(Option<Key>);
        impl Pane for Probe {
            fn draw(&mut self, screen: &mut Screen) {
                screen.clear();
                screen.put(0, 0, screen.w, "Ready", Style::PLAIN);
            }
            fn help(&self) -> Page {
                Page::new(vec![])
            }
            fn key(&mut self, key: Key) -> After {
                if Action::key(key).is_some() {
                    assert_eq!(Some(key), self.0);
                    // The loop returns this without starting a browser.
                    After::OpenWeb
                } else if key == Key::Char('q') {
                    After::Leave
                } else {
                    After::Ignored
                }
            }
        }

        let welcome = Start::Welcome(|| Ok(Box::new(Probe(None))));
        for (start, keys, expected) in [
            (welcome, b"q\x0fq".as_slice(), None),
            (Start::Loaded, b"?q\x12q".as_slice(), None),
            (Start::Loaded, b"\x12\x03".as_slice(), None),
            (Start::Loaded, b"\x1b[200~\x12\x0f\x1b[201~\x0fq".as_slice(), Some(Key::Ctrl(b'o'))),
        ] {
            let (mut terminal, mut master, _signals) = Session::test_pty();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let output = read_until(&mut master, "Ready");
                    master.write_all(keys).unwrap();
                    assert!(output.contains("Ready"), "{output:?}");
                });
                let exit = pane_loop(start, &|| Ok(Box::new(Probe(expected))), &mut terminal, &mut Screen::new());
                let launched = match exit {
                    Ok(Exit::OpenWeb) => true,
                    Ok(Exit::Done) => false,
                    Err(error) => panic!("{error}"),
                };
                assert_eq!(launched, expected.is_some(), "{keys:?}");
            });
        }
    }

    #[test]
    fn a_hover_report_repaints_the_link_with_an_underline() {
        struct Link;
        impl Pane for Link {
            fn draw(&mut self, screen: &mut Screen) {
                screen.clear();
                screen.put(0, 0, screen.w, "link", Style::PLAIN);
            }
            fn help(&self) -> Page {
                Page::new(vec![])
            }
            fn tracks_hover(&self) -> bool {
                true
            }
            fn hover_span(&self, x: usize, y: usize) -> Option<Range<usize>> {
                (x < 4 && y == 0).then_some(0..4)
            }
            fn key(&mut self, key: Key) -> After {
                if key == Key::Char('q') { After::Leave } else { After::Ignored }
            }
        }

        let (mut terminal, mut master, _signals) = Session::test_pty();
        std::thread::scope(|scope| {
            let observe = scope.spawn(|| {
                let before = read_until(&mut master, "link");
                master.write_all(b"\x1b[<35;2;1M").unwrap();
                let after = read_until(&mut master, "\x1b[0;4mlink");
                master.write_all(b"q").unwrap();
                assert!(before.contains("link") && !before.contains("\x1b[0;4m"), "{before:?}");
                assert!(after.contains("\x1b[0;4mlink"), "{after:?}");
            });
            run_entered(Start::Loaded, &|| Ok(Box::new(Link)), &mut terminal).unwrap();
            observe.join().unwrap();
        });
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
    fn a_change_reported_while_a_check_is_away_brings_another_look_soon() {
        let start = Instant::now();
        let ended = start + Duration::from_millis(10);
        let mut pace = Pace::new(WATCHED, start);
        pace.leave_running(start);
        pace.finish_look(start, ended, true);
        assert_eq!(pace.until_look(ended), WATCHED);

        pace.leave_running(ended);
        pace.report_change();
        let later = ended + Duration::from_millis(10);
        pace.finish_look(ended, later, true);
        assert_eq!(pace.until_look(later), MIN_LOOK_INTERVAL);
    }

    /// A pane whose check stays away until the test says what it found.
    struct Away {
        ready: Option<io::PipeReader>,
        found: Option<bool>,
    }

    impl Away {
        fn watched(start: Instant) -> (Self, Watcher, Option<Pace>) {
            let pane = Self { ready: Some(io::pipe().unwrap().0), found: None };
            let watcher = Watcher::new(pane.watch_scope());
            (pane, watcher, Some(Pace::new(UNWATCHED, start)))
        }
    }

    impl Pane for Away {
        fn draw(&mut self, _: &mut Screen) {}
        fn help(&self) -> Page {
            Page::new(vec![])
        }
        fn unfinished_check(&self) -> Option<RawFd> {
            self.ready.as_ref().map(AsRawFd::as_raw_fd)
        }
        fn finish_check(&mut self) -> Option<bool> {
            let found = self.found.take()?;
            self.ready = None;
            Some(found)
        }
        fn key(&mut self, _: Key) -> After {
            After::Ignored
        }
    }

    #[test]
    fn a_check_left_running_is_not_repeated_and_rests_for_its_whole_length_afterwards() {
        let start = Instant::now();
        let (mut pane, mut watcher, mut pace) = Away::watched(start);
        assert!(check_watched_pane_at(&mut pane, &mut watcher, &mut pace, true, || start));

        let later = start + Duration::from_secs(1);
        assert!(!finish_away_check(&mut pane, &mut watcher, &mut pace, true, || later));
        assert!(pace.as_ref().unwrap().away.is_some());

        pane.found = Some(true);
        assert!(finish_away_check(&mut pane, &mut watcher, &mut pace, true, || later));
        let pace = pace.unwrap();
        assert!(pace.away.is_none());
        assert_eq!(pace.until_look(later), Duration::from_secs(4));
    }

    #[test]
    fn a_pane_that_drops_its_running_check_is_looked_at_again_soon() {
        let start = Instant::now();
        let (mut pane, mut watcher, mut pace) = Away::watched(start);
        check_watched_pane_at(&mut pane, &mut watcher, &mut pace, true, || start);
        pane.ready = None;
        assert!(finish_away_check(&mut pane, &mut watcher, &mut pace, true, || start));
        let pace = pace.unwrap();
        assert!(pace.away.is_none());
        assert_eq!(pace.until_look(start), MIN_LOOK_INTERVAL);
    }

    #[test]
    fn an_answer_is_taken_in_even_by_a_pace_that_never_saw_its_check_start() {
        let start = Instant::now();
        let (mut pane, mut watcher, mut pace) = Away::watched(start);
        assert!(!finish_away_check(&mut pane, &mut watcher, &mut pace, true, || start));
        pane.found = Some(true);
        assert!(finish_away_check(&mut pane, &mut watcher, &mut pace, true, || start));
        assert!(pane.unfinished_check().is_none());
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

    #[test]
    fn the_first_snapshot_gets_a_prompt_look_under_the_new_watcher() {
        let f = fixture::Fixture::new();
        f.write("source.rs", "first\n");
        f.commit("first");
        let mut pane = Outline::new(Repo::open(&f.root).unwrap(), false);

        // This change precedes the watcher's event stream.
        f.write("source.rs", "first\nsecond\n");
        let (mut watcher, mut pace) = watch_opened_pane(&mut pane, false, Duration::from_millis(10));
        let timing = pace.as_ref().unwrap();
        assert_eq!(timing.until_look(timing.ended), MIN_LOOK_INTERVAL);
        assert!(timing.until_look(timing.ended + MIN_LOOK_INTERVAL).is_zero());
        assert!(check_watched_pane(&mut pane, &mut watcher, &mut pace, false));
        assert_eq!(pace.as_ref().unwrap().next, NextLook::Idle, "the startup look should return to idle pacing");

        let mut screen = Screen::sized(80, 12);
        pane.draw(&mut screen);
        assert!(screen.text().iter().any(|row| row.contains("source.rs") && row.ends_with("+1")));
    }

    #[test]
    fn a_slow_open_delays_reconciliation_but_not_a_reported_change() {
        let f = fixture::Fixture::new();
        f.write("source.rs", "first\n");
        f.commit("first");
        let mut pane = Outline::new(Repo::open(&f.root).unwrap(), false);
        let (_watcher, pace) = watch_opened_pane(&mut pane, false, Duration::from_secs(3));
        let mut timing = pace.unwrap();
        assert_eq!(timing.until_look(timing.ended), Duration::from_secs(12));
        assert_eq!(timing.until_look(timing.ended + Duration::from_secs(1)), Duration::from_secs(11));
        assert!(timing.until_look(timing.ended + Duration::from_secs(12)).is_zero());
        timing.report_change();
        assert_eq!(timing.until_look(timing.ended), MIN_LOOK_INTERVAL);
        assert!(timing.until_look(timing.ended + Duration::from_secs(1)).is_zero());
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
        let ignored = &watcher.scope().ignored;
        assert!(ignored.dirs.is_empty() && ignored.files.is_empty());

        f.write("build/generated/output", "first build\n");
        check_watched_pane(&mut pane, &mut watcher, &mut pace, false);
        let ignored = pane.watch_scope().ignored;
        assert_eq!(ignored.dirs, std::collections::HashSet::from([f.root.join("build/generated")]));
        let current = watcher.fd().unwrap();
        assert_ne!(original, current);
        assert_eq!(pace.as_ref().unwrap().next, NextLook::Prompt);

        // A force-tracked file under the ignore rule is still source. A
        // post-snapshot write must be noticed even without a wakeup byte.
        f.write("build/source.rs", "first\nsecond\n");
        check_watched_pane(&mut pane, &mut watcher, &mut pace, false);
        let mut screen = Screen::sized(80, 12);
        pane.draw(&mut screen);
        assert!(screen.text().iter().any(|row| row.contains("source.rs") && row.ends_with("+1")));
        assert!(std::sync::Arc::ptr_eq(&ignored, &pane.watch_scope().ignored));
        assert_eq!(watcher.fd(), Some(current), "unchanged exclusions must not restart the stream");
        assert_eq!(pace.as_ref().unwrap().next, NextLook::Idle, "an unchanged scope must not request another check");
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
