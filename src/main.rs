//! strata: a project's files, coloured by what is not yet committed or pushed.

#[cfg(test)]
mod fixture;
mod git;
mod help;
mod history;
mod icons;
mod outline;
mod read;
mod rex;
mod syntax;
mod term;
mod tree;
mod ui;
mod view;
mod walk;
mod watch;
mod when;

use std::ffi::OsStr;
use std::path::Path;
use std::process::exit;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use git::Repo;
use outline::Outline;
use term::{Key, Screen, Wait};
use view::View;
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
  ?             what the colours and marks mean, and these keys

Enter or a double click on a file opens it to read: in a floating window
when running inside Rex, otherwise in the pane itself. `strata view FILE`
is that reader by itself: the file, read-only, coloured as code, with the
lines it gained and lost marked. n and N step through the changes, q
closes it.

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
    Outline(Outline),
    View(View),
}

impl Ui {
    fn draw(&mut self, s: &mut Screen) {
        match self {
            Ui::Outline(o) => o.draw(s),
            Ui::View(v) => v.draw(s),
        }
    }

    /// Returns true when the program should quit.
    fn on_key(&mut self, key: Key) -> bool {
        match self {
            Ui::Outline(o) => o.on_key(key),
            Ui::View(v) => v.on_key(key),
        }
    }

    /// Looks for changes made outside, and takes them in. Returns false if
    /// it is too soon to tell.
    fn check(&mut self) -> bool {
        match self {
            Ui::Outline(o) => o.check(),
            Ui::View(v) => v.check(),
        }
    }

    /// How long the screen stays right if nothing happens.
    fn next_frame(&self) -> Option<Duration> {
        match self {
            Ui::Outline(o) => o.next_frame(),
            Ui::View(_) => None,
        }
    }

    /// Where to watch for changes and how much the watch covers.
    fn watch(&self) -> Scope {
        match self {
            Ui::Outline(o) => o.watch(),
            Ui::View(v) => v.watch(),
        }
    }
}

/// Ends the program with a message. A path in it may hold anything, so
/// control characters are left out.
fn fail(message: String) -> ! {
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
        fail("too many arguments; see strata --help".to_string());
    }
    // Paths may contain arbitrary bytes; only the words strata knows are text.
    let mut ui = match (first.as_deref().and_then(OsStr::to_str), second.as_deref()) {
        (Some("-h" | "--help"), None) => return print!("{USAGE}"),
        (Some("-V" | "--version"), None) => return println!("strata {}", env!("CARGO_PKG_VERSION")),
        (Some("view"), None) => fail("which file? strata view FILE".to_string()),
        (Some("view"), Some(file)) => {
            let file = Path::new(file);
            match View::open(file) {
                Some(view) => Ui::View(view),
                None => fail(format!("{} is not in a directory", file.display())),
            }
        }
        (Some(option), None) if option.starts_with('-') => {
            fail(format!("no option {option}; see strata --help"));
        }
        (_, None) => {
            let dir = first.as_deref().map_or(Path::new("."), Path::new);
            match Repo::open(dir) {
                Some(repo) => Ui::Outline(Outline::new(repo, rex::inside())),
                None if dir.is_file() => {
                    fail(format!("{0} is a file; to read it: strata view {0}", dir.display()))
                }
                None => fail(format!("{} is not a directory", dir.display())),
            }
        }
        _ => fail("too many arguments; see strata --help".to_string()),
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
        Err(e) => fail(format!("cannot use this terminal: {e}")),
    };
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::leave();
        hook(info);
    }));

    let mut screen = Screen::new();
    let mut keys = Vec::new();
    let mut pace = Pace::new(idle, Instant::now());
    'run: while !term::TERMINATED.load(Ordering::Relaxed) {
        if term::RESIZED.swap(false, Ordering::Relaxed) {
            let (w, h) = term::size();
            screen.resize(w, h);
        }
        ui.draw(&mut screen);
        screen.flush();

        let until_look = pace.wait(Instant::now());
        let wait = ui.next_frame().map_or(until_look, |frame| frame.min(until_look));
        match term::wait(&keyboard, signal.as_ref(), wait.as_millis() as i32 + 1, watcher.fd()) {
            Wait::Input => {
                term::read_keys(&mut keyboard, &mut keys);
                for key in keys.drain(..) {
                    if let Key::Background(rgb) = key {
                        screen.set_background(rgb);
                    } else if ui.on_key(key) {
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
}
