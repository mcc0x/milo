//! Runs terminal panes with shared input, help, redraws and file watching.

use std::process::exit;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::help::Page;
use crate::keys::{Input, Key};
use crate::screen::Screen;
use crate::term::{self, Wait};
use crate::watch::{Scope, Watcher};

/// Whether the current pane is still open after a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    Stay,
    Leave,
}

/// One screen in the terminal. The loop owns input, redraws and when to check.
pub trait Pane {
    fn draw(&mut self, screen: &mut Screen);
    fn help(&self) -> Page;
    /// An empty, complete scope means nothing outside the pane can change it.
    fn watch_scope(&self) -> Scope {
        Scope { roots: Vec::new(), ignored: Vec::new(), complete: true }
    }

    /// Takes in changes from outside. False asks the loop to check again soon.
    fn check(&mut self) -> bool {
        true
    }

    /// How long the frame stays right when nothing happens.
    fn next_frame(&self) -> Option<Duration> {
        None
    }

    /// Handles a pane's own keys, including closing on q.
    fn key(&mut self, key: Key) -> After;
}

pub type Open<'a> = dyn Fn() -> Result<Box<dyn Pane>, String> + 'a;

/// How a pane starts: already loaded, needing its first check, or offered before another pane.
pub enum Start {
    Loaded,
    Check,
    Offer(fn() -> Result<Box<dyn Pane>, String>),
}

/// Minimum rest after a look if the watcher has reported a change.
const MIN_LOOK_INTERVAL: Duration = Duration::from_millis(120);
/// Rest at least this many times the cost of the last look, so repeated
/// looks at the same cost take at most a fifth of the time.
const REST: u32 = 4;
/// Time between looks when the watcher is expected to cover the project.
pub(super) const WATCHED: Duration = Duration::from_secs(30);
/// Time between looks when the watcher cannot cover the project.
const UNWATCHED: Duration = Duration::from_secs(2);

/// When to look for changes again, with or without a report from the watcher.
pub(super) struct Pace {
    idle: Duration,
    ended: Instant,
    took: Duration,
    reported: bool,
}

impl Pace {
    fn new(idle: Duration, now: Instant) -> Self {
        Self { idle, ended: now, took: Duration::ZERO, reported: false }
    }

    pub(super) fn for_pane(pane: &mut dyn Pane, idle: Duration, check_on_open: bool) -> Self {
        let mut pace = Self::new(idle, Instant::now());
        if check_on_open {
            pace.look(pane, true);
        }
        pace
    }

    #[cfg(test)]
    pub(super) fn idle(&self) -> Duration {
        self.idle
    }

    fn report_change(&mut self) {
        self.reported = true;
    }

    /// Time until the event loop next calls `Pane::check`.
    fn until_look(&self, now: Instant) -> Duration {
        let rest = if self.reported { MIN_LOOK_INTERVAL } else { self.idle }.max(self.took * REST);
        rest.saturating_sub(now.saturating_duration_since(self.ended))
    }

    /// An unsettled look, say during a checkout, is retried as if a change was reported.
    fn finish_look(&mut self, started: Instant, ended: Instant, settled: bool) {
        (self.ended, self.took, self.reported) = (ended, ended - started, !settled);
    }

    pub(super) fn look(&mut self, pane: &mut dyn Pane, watch_may_shrink: bool) {
        let started = Instant::now();
        let settled = pane.check();
        self.finish_look(started, Instant::now(), settled);
        if watch_may_shrink && self.idle == WATCHED && !pane.watch_scope().complete {
            self.idle = UNWATCHED;
        }
    }
}

fn draw_pane(pane: &mut dyn Pane, screen: &mut Screen, help: Option<&mut Page>) {
    if let Some(page) = help {
        page.draw(screen);
    } else {
        pane.draw(screen);
    }
}

/// Routes terminal replies and help keys before the pane sees them.
fn route_key(pane: &mut dyn Pane, screen: &mut Screen, help: &mut Option<Page>, key: Key) -> After {
    match key {
        Key::Ctrl(b'c') | Key::Eof => return After::Leave,
        Key::Background(rgb) => screen.set_background(rgb),
        Key::Pictures(able) => screen.set_pictures(able),
        Key::CellSize(width, height) => screen.set_cell_size(width, height),
        Key::Answered => screen.answered(),
        // The pane still sees focus changes while its help is up.
        Key::Focus(_) => return pane.key(key),
        _ => {
            if let Some(page) = help {
                if !page.scroll(key, screen.h) {
                    *help = None;
                }
            } else if key == Key::Char('?') {
                if Page::fits(screen) {
                    *help = Some(pane.help());
                }
            } else {
                return pane.key(key);
            }
        }
    }
    After::Stay
}

fn watch_pane(pane: &dyn Pane, needs_polling: bool) -> (Watcher, Option<Duration>) {
    let scope = pane.watch_scope();
    let watcher = Watcher::new(&scope);
    let idle = if !needs_polling && scope.complete && scope.roots.is_empty() {
        None
    } else if watcher.fd().is_some() && scope.complete {
        Some(WATCHED)
    } else {
        Some(UNWATCHED)
    };
    (watcher, idle)
}

pub fn run(start: Start, open: &Open<'_>) -> Result<(), String> {
    let signal = term::listen();
    let check_on_open = matches!(start, Start::Check);
    let mut next = if let Start::Offer(open) = start { Some(open) } else { None };
    let mut pane = open()?;
    let (mut watcher, idle) = watch_pane(pane.as_ref(), check_on_open);
    // Asked to stop while still starting up: there is nothing to put back.
    if term::TERMINATED.load(Ordering::Relaxed) {
        exit(1);
    }
    let mut keyboard = term::enter().map_err(|e| format!("cannot use this terminal: {e}"))?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::leave();
        hook(info);
    }));

    let mut screen = Screen::new();
    let mut help = None;
    let mut keys = Vec::new();
    let mut input = Input::default();
    let mut pace = idle.map(|idle| Pace::for_pane(pane.as_mut(), idle, check_on_open));
    'run: while !term::TERMINATED.load(Ordering::Relaxed) {
        if term::RESIZED.swap(false, Ordering::Relaxed) {
            let (w, h) = term::size();
            screen.resize(w, h);
            if !Page::fits(&screen) {
                help = None;
            }
        }
        draw_pane(pane.as_mut(), &mut screen, help.as_mut());
        screen.flush();

        let drawn = Instant::now();
        let until_look = pace.as_ref().map(|pace| pace.until_look(drawn));
        let until_frame = if help.is_some() { None } else { pane.next_frame() };
        let wait = until_look
            .into_iter()
            .chain(until_frame)
            // If the signal pipe couldn't be opened, don't sleep through a signal.
            .chain(signal.is_none().then_some(UNWATCHED))
            .min();
        let event = loop {
            let timeout_ms = wait.map_or(-1, |wait| {
                i32::try_from(wait.saturating_sub(drawn.elapsed()).as_millis() + 1).unwrap_or(i32::MAX)
            });
            match term::wait(&keyboard, signal.as_ref(), timeout_ms, watcher.fd()) {
                Wait::Input => {
                    input.receive(&mut keyboard, &mut keys);
                    if !keys.is_empty() {
                        break Wait::Input;
                    }
                    // Ignored input doesn't need a new frame, but timers still run.
                    if wait.is_some_and(|wait| drawn.elapsed() >= wait) {
                        break Wait::Timeout;
                    }
                }
                event => break event,
            }
        };
        match event {
            Wait::Input => {
                for key in keys.drain(..) {
                    if route_key(pane.as_mut(), &mut screen, &mut help, key) == After::Leave {
                        if matches!(key, Key::Char('q') | Key::Esc)
                            && let Some(open) = next.take()
                        {
                            pane = match open() {
                                Ok(pane) => pane,
                                Err(why) => {
                                    term::leave();
                                    return Err(why);
                                }
                            };
                            let (next_watcher, next_idle) = watch_pane(pane.as_ref(), false);
                            watcher = next_watcher;
                            pace = next_idle.map(|idle| Pace::new(idle, Instant::now()));
                            continue;
                        }
                        break 'run;
                    }
                }
            }
            Wait::Woken => {
                watcher.drain();
                if let Some(pace) = &mut pace {
                    pace.report_change();
                }
            }
            Wait::Timeout | Wait::Interrupted => {}
        }
        if let Some(pace) = &mut pace
            && pace.until_look(Instant::now()).is_zero()
        {
            pace.look(pane.as_mut(), check_on_open);
        }
    }
    // A picture is not part of the text, so it is taken down by name.
    screen.clear();
    screen.flush();
    term::leave();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fixture, git::Repo, outline::Outline};

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
