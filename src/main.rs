//! strata: a project's files, coloured by what is not yet committed or pushed.

mod diff;
#[cfg(test)]
mod fixture;
mod git;
mod help;
mod history;
mod icons;
mod keys;
mod outline;
mod read;
mod reader;
mod rex;
mod screen;
mod syntax;
mod term;
mod tree;
mod ui;
mod walk;
mod watch;
mod when;
mod width;

use std::ffi::OsStr;
use std::path::Path;
use std::process::exit;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use git::Repo;
use help::Page;
use keys::{Key, read_keys};
use outline::Outline;
use reader::Reader;
use screen::Screen;
use term::Wait;
use watch::{Scope, Watcher};

const USAGE: &str = "\
strata — a project's files, coloured by what is not yet committed or pushed

Usage: strata [PATH]
       strata view FILE

PATH is any directory (default: the current one). Inside a git repository
it shows the whole project; anywhere else it is a plain file tree, with
nothing coloured. It fits its pane: a file tree when tall, a strip of the
changed files when short, with what happened to each when wide. Recent
history sits beneath the tree in a tall pane and beside it in a wide one.
It updates on its own.

  ↑ ↓ or j k    move                enter    open a folder, or read a file
  n N           next, previous changed file
  } {           next, previous folder
  J K           next, previous beside this one, over anything nested
  p             up to the parent folder
  e             open every folder   c        changed files only
  t             show or hide recent history
  r             reload              q        quit
  ?             what the colours and marks mean, and common keys

Enter or a double click on a file opens it to read: in a floating window
when running inside Rex, otherwise in the pane itself. `strata view FILE`
opens the file on its own, read-only, coloured as code, with the lines it
gained and lost marked. n and N step through the changes, q closes it.

Colour is the change: yellow edited, green new, red removed, cyan moved,
magenta in conflict. Bold is not committed yet; regular weight is
committed, waiting to be pushed. History says the same by weight: bold
dots are commits still to push, faint ones are already pushed.

While the keyboard is in another pane, the outline moves to whatever file
is being written and says so. It never moves while you are in it.

Icons need a Nerd Font. STRATA_ICONS=plain uses ordinary symbols instead,
STRATA_ICONS=off shows none.
";

/// How long to rest after a look when a change has been reported.
const MIN_LOOK_INTERVAL: Duration = Duration::from_millis(120);
/// Rests this many times as long as the last look took, even after a report.
/// Repeated looks at the same cost thus take at most a fifth of the time.
const REST: u32 = 4;
/// How long to rest without a report when the watcher covers everything.
const WATCHED: Duration = Duration::from_secs(30);
/// A shorter rest where the watcher can miss changes or is unavailable.
const UNWATCHED: Duration = Duration::from_secs(2);

/// When to look for changes again, with or without a report from the watcher.
struct Pace {
    idle: Duration,
    ended: Instant,
    took: Duration,
    reported: bool,
}

impl Pace {
    fn new(idle: Duration, now: Instant) -> Self {
        Self { idle, ended: now, took: Duration::ZERO, reported: false }
    }

    fn report_change(&mut self) {
        self.reported = true;
    }

    /// How long from `now` until the next look is due.
    fn wait(&self, now: Instant) -> Duration {
        let rest = if self.reported { MIN_LOOK_INTERVAL } else { self.idle }.max(self.took * REST);
        rest.saturating_sub(now.saturating_duration_since(self.ended))
    }

    /// An unsettled look, say during a checkout, is retried as if a change was reported.
    fn finished(&mut self, started: Instant, ended: Instant, settled: bool) {
        (self.ended, self.took, self.reported) = (ended, ended - started, !settled);
    }
}

/// The two things strata can fill a pane with.
enum Ui {
    Outline(Box<Outline>),
    Reader(Box<Reader>),
}

impl Ui {
    fn draw(&mut self, s: &mut Screen, help: Option<&mut Page>) {
        if let Some(page) = help {
            page.draw(s);
            return;
        }
        match self {
            Ui::Outline(o) => o.draw(s),
            Ui::Reader(reader) => reader.draw(s),
        }
    }

    /// Returns true when the program should quit.
    fn on_key(&mut self, key: Key) -> bool {
        match self {
            Ui::Outline(o) => o.on_key(key),
            Ui::Reader(reader) => reader.on_key(key),
        }
    }

    fn help(&self) -> Page {
        match self {
            Ui::Outline(o) => o.help(),
            Ui::Reader(_) => Page::reader(),
        }
    }

    /// Looks for changes made outside, and takes them in. Returns false if
    /// it is too soon to tell.
    fn check(&mut self) -> bool {
        match self {
            Ui::Outline(o) => o.check(),
            Ui::Reader(reader) => reader.check(),
        }
    }

    /// How long the screen stays right if nothing happens.
    fn next_frame(&self) -> Option<Duration> {
        match self {
            Ui::Outline(o) => o.next_frame(),
            Ui::Reader(_) => None,
        }
    }

    /// Where to watch for changes and how much the watch covers.
    fn watch(&self) -> Scope {
        match self {
            Ui::Outline(o) => o.watch(),
            Ui::Reader(reader) => reader.watch(),
        }
    }
}

/// Routes terminal replies and help keys before the Outline or Reader sees a key.
/// Returns true when the program should quit.
fn route_key(ui: &mut Ui, screen: &mut Screen, help: &mut Option<Page>, key: Key) -> bool {
    match key {
        Key::Ctrl(b'c') | Key::Eof => return true,
        Key::Background(rgb) => screen.set_background(rgb),
        // Focus still matters to the outline underneath the page.
        Key::Focus(_) => return ui.on_key(key),
        _ => {
            if let Some(page) = help {
                if !page.scroll(key, screen.h) {
                    *help = None;
                }
            } else if key == Key::Char('?') {
                *help = Some(ui.help());
            } else {
                return ui.on_key(key);
            }
        }
    }
    false
}

/// Ends the program with a message. A path in it may hold anything, so
/// control characters are left out.
fn fail(message: &str) -> ! {
    let safe: String = message.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    eprintln!("strata: {safe}");
    exit(1);
}

fn main() {
    let signal = term::listen();
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let second = args.next();
    if args.next().is_some() {
        fail("too many arguments; see strata --help");
    }
    // Paths may contain arbitrary bytes; only the words strata knows are text.
    let mut ui = match (first.as_deref().and_then(OsStr::to_str), second.as_deref()) {
        (Some("-h" | "--help"), None) => return print!("{USAGE}"),
        (Some("-V" | "--version"), None) => return println!("strata {}", env!("CARGO_PKG_VERSION")),
        (Some("view"), None) => fail("which file? strata view FILE"),
        (Some("view"), Some(file)) => {
            let file = Path::new(file);
            match Reader::open(file) {
                Some(reader) => Ui::Reader(Box::new(reader)),
                None => fail(&format!("{} is not in a directory", file.display())),
            }
        }
        (Some(option), None) if option.starts_with('-') => {
            fail(&format!("no option {option}; see strata --help"));
        }
        (_, None) => {
            let dir = first.as_deref().map_or(Path::new("."), Path::new);
            match Repo::open(dir) {
                Some(repo) => Ui::Outline(Box::new(Outline::new(repo, rex::inside()))),
                None if dir.is_file() => fail(&format!("{0} is a file; to read it: strata view {0}", dir.display())),
                None => fail(&format!("{} is not a directory", dir.display())),
            }
        }
        _ => fail("too many arguments; see strata --help"),
    };
    let (watcher, idle) = {
        let scope = ui.watch();
        let watcher = Watcher::new(&scope);
        let idle = if watcher.fd().is_some() && scope.complete { WATCHED } else { UNWATCHED };
        (watcher, idle)
    };
    // Asked to stop while still starting up: there is nothing to put back.
    if term::TERMINATED.load(Ordering::Relaxed) {
        exit(1);
    }
    let mut keyboard = match term::enter() {
        Ok(keyboard) => keyboard,
        Err(e) => fail(&format!("cannot use this terminal: {e}")),
    };
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::leave();
        hook(info);
    }));

    let mut screen = Screen::new();
    let mut help = None;
    let mut keys = Vec::new();
    let mut pace = Pace::new(idle, Instant::now());
    'run: while !term::TERMINATED.load(Ordering::Relaxed) {
        if term::RESIZED.swap(false, Ordering::Relaxed) {
            let (w, h) = term::size();
            screen.resize(w, h);
        }
        ui.draw(&mut screen, help.as_mut());
        screen.flush();

        let until_look = pace.wait(Instant::now());
        let wait =
            if help.is_some() { until_look } else { ui.next_frame().map_or(until_look, |frame| frame.min(until_look)) };
        let timeout_ms = i32::try_from(wait.as_millis() + 1).unwrap_or(i32::MAX);
        match term::wait(&keyboard, signal.as_ref(), timeout_ms, watcher.fd()) {
            Wait::Input => {
                read_keys(&mut keyboard, &mut keys);
                for key in keys.drain(..) {
                    if route_key(&mut ui, &mut screen, &mut help, key) {
                        break 'run;
                    }
                }
            }
            Wait::Woken => {
                watcher.drain();
                pace.report_change();
            }
            Wait::Timeout | Wait::Interrupted => {}
        }
        if pace.wait(Instant::now()).is_zero() {
            let started = Instant::now();
            let settled = ui.check();
            pace.finished(started, Instant::now(), settled);
        }
    }
    term::leave();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_projects_are_checked_even_without_a_report() {
        let start = Instant::now();
        for (idle, seconds) in [(WATCHED, 30), (UNWATCHED, 2)] {
            let pace = Pace::new(idle, start);
            assert_eq!(pace.wait(start + Duration::from_secs(1)), Duration::from_secs(seconds - 1));
            assert_eq!(pace.wait(start + Duration::from_secs(seconds)), Duration::ZERO);
        }
    }

    #[test]
    fn a_report_is_due_immediately_if_the_last_look_has_had_enough_rest() {
        let start = Instant::now();
        let mut pace = Pace::new(WATCHED, start);
        pace.report_change();
        assert_eq!(pace.wait(start + Duration::from_millis(20)), Duration::from_millis(100));
        assert_eq!(pace.wait(start + Duration::from_secs(1)), Duration::ZERO);
    }

    #[test]
    fn a_slow_look_rests_four_times_as_long_even_when_changes_are_reported() {
        let start = Instant::now();
        let ended = start + Duration::from_secs(1);
        let mut pace = Pace::new(UNWATCHED, start);
        pace.finished(start, ended, true);
        assert_eq!(pace.wait(ended), Duration::from_secs(4));
        pace.report_change();
        assert_eq!(pace.wait(ended + Duration::from_secs(1)), Duration::from_secs(3));
        assert_eq!(pace.wait(ended + Duration::from_secs(4)), Duration::ZERO);
    }

    #[test]
    fn an_unsettled_look_is_retried_soon_and_a_settled_look_goes_back_to_idle() {
        let start = Instant::now();
        let ended = start + Duration::from_millis(10);
        let mut pace = Pace::new(WATCHED, start);
        pace.finished(start, ended, false);
        assert_eq!(pace.wait(ended), Duration::from_millis(120));
        let retry = ended + Duration::from_millis(120);
        assert_eq!(pace.wait(retry), Duration::ZERO);
        pace.finished(retry, retry + Duration::from_millis(10), true);
        assert_eq!(pace.wait(retry + Duration::from_millis(10)), Duration::from_secs(30));
    }

    #[test]
    fn help_scrolls_by_a_page_and_escape_restores_the_outline() {
        let f = fixture::Fixture::new();
        f.write("page.txt", "a file\n");
        let mut ui = Ui::Outline(Box::new(Outline::new(Repo::open(&f.root).unwrap(), false)));
        let mut screen = Screen::sized(65, 12);
        let mut help = None;
        ui.draw(&mut screen, help.as_mut());
        let before = screen.text();

        assert!(!route_key(&mut ui, &mut screen, &mut help, Key::Char('?')));
        ui.draw(&mut screen, help.as_mut());
        let first = screen.text();
        assert!(first[0].trim_start().starts_with("Files"));
        assert!(first[11].contains("other keys close this"));
        assert!(!route_key(&mut ui, &mut screen, &mut help, Key::PgDn));
        ui.draw(&mut screen, help.as_mut());
        assert_eq!(screen.text()[0], first[10]);
        assert!(!route_key(&mut ui, &mut screen, &mut help, Key::PgUp));
        ui.draw(&mut screen, help.as_mut());
        assert_eq!(screen.text(), first);
        assert!(!route_key(&mut ui, &mut screen, &mut help, Key::Esc));
        ui.draw(&mut screen, help.as_mut());
        assert_eq!(screen.text(), before);
    }

    #[test]
    fn help_uses_the_nested_reader_page_and_terminal_quit_bypasses_it() {
        let f = fixture::Fixture::new();
        f.write("page.txt", "a file\n");
        let mut ui = Ui::Outline(Box::new(Outline::new(Repo::open(&f.root).unwrap(), false)));
        let mut screen = Screen::sized(65, 12);
        let mut help = None;
        assert!(!route_key(&mut ui, &mut screen, &mut help, Key::Enter));
        assert!(!route_key(&mut ui, &mut screen, &mut help, Key::Char('?')));
        ui.draw(&mut screen, help.as_mut());
        assert!(screen.text()[0].trim_start().starts_with("Lines"));
        assert!(route_key(&mut ui, &mut screen, &mut help, Key::Ctrl(b'c')));
        assert!(route_key(&mut ui, &mut screen, &mut help, Key::Eof));
    }
}
