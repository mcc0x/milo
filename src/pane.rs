//! Runs terminal panes with shared input, help, redraws and file watching.

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::help::Page;
use crate::keys::{Input, Key};
use crate::screen::Screen;
use crate::term::{self, Session, Wait, milliseconds};
use crate::ui;
use crate::watch::{Scope, Watcher};

/// What to do after a key: keep the frame, redraw, or close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    Ignored,
    Stay,
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
    /// An empty, complete scope means nothing outside the pane can change it.
    fn watch_scope(&self) -> Scope {
        Scope { roots: Vec::new(), ignored: Vec::new(), complete: true }
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
        let (watcher, idle) = watch_pane(pane.as_ref(), check_on_open);
        let pace = idle.map(|idle| Pace::for_pane(pane.as_mut(), idle, check_on_open));
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
    'run: while !term::TERMINATED.load(Ordering::Relaxed) {
        if term::RESIZED.swap(false, Ordering::Relaxed) || screen.w == 0 {
            let (w, h) = terminal.size();
            screen.resize(w, h);
            if !Page::fits(&screen) {
                help = None;
            }
            redraw = true;
        }
        if std::mem::take(&mut redraw) {
            draw_pane(pane.as_mut(), &mut screen, help.as_mut());
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
                for key in keys.drain(..) {
                    match route_key(pane.as_mut(), &mut screen, &mut help, key) {
                        After::Ignored => {}
                        After::Stay => redraw = true,
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
                }
            }
            Wait::Woken => {
                watcher.drain();
                if let Some(pace) = &mut pace {
                    pace.report_change();
                }
            }
            Wait::External => match pane.check_external() {
                External::Unchanged => {}
                External::Redraw => redraw = true,
                External::NewScope => {
                    let (next_watcher, idle) = watch_pane(pane.as_ref(), check_on_open);
                    watcher = next_watcher;
                    pace = idle.map(|idle| Pace::for_pane(pane.as_mut(), idle, check_on_open));
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
        if let Some(pace) = &mut pace
            && pace.until_look(now).is_zero()
        {
            pace.look(pane.as_mut(), check_on_open);
            redraw = true;
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
