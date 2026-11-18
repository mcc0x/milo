//! strata: a project's files, coloured by what is not yet committed or pushed.

mod git;
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

use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use git::Repo;
use outline::Outline;
use term::{Key, Screen, Wait};
use view::View;
use watch::Watcher;

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

Enter or a double click on a file opens it in a floating window, when
running inside Rex. `strata view FILE` is that reader by itself: the file,
read-only, with the lines it gained and lost marked. n and N step through
the changes, q closes it.

Colour is the change: yellow edited, green new, red removed, cyan moved,
magenta in conflict. Bold is not committed yet; regular weight is
committed, waiting to be pushed.

Icons need a Nerd Font. STRATA_ICONS=plain uses ordinary symbols instead,
STRATA_ICONS=off shows none.
";

/// How soon after the operating system reports a change it is looked at.
const SETTLE: Duration = Duration::from_millis(120);
/// A look is never more than a fifth of the time: after one that took this
/// long, the next waits four times as long, however fast changes arrive.
const REST: u32 = 4;
/// How often to look for changes regardless: seldom where the operating
/// system reports them, often where it cannot.
const WATCHED: Duration = Duration::from_secs(30);
const UNWATCHED: Duration = Duration::from_secs(2);

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

    /// The directories to watch, and whether watching them misses nothing.
    fn watch(&self) -> (Vec<&Path>, bool) {
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
    term::listen();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut ui = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["-h" | "--help"] => return print!("{USAGE}"),
        ["-V" | "--version"] => return println!("strata {}", env!("CARGO_PKG_VERSION")),
        ["view", file] => match View::open(Path::new(file)) {
            Some(view) => Ui::View(view),
            None => fail(format!("{file} is not in a directory")),
        },
        [] | [_] => {
            let dir = PathBuf::from(args.first().map_or(".", String::as_str));
            match Repo::open(&dir) {
                Some(repo) => Ui::Outline(Outline::new(repo)),
                None => fail(format!("{} is not a directory", dir.display())),
            }
        }
        _ => fail(format!("too many arguments\n\n{USAGE}")),
    };
    let (watcher, idle) = {
        let (paths, fully) = ui.watch();
        let watcher = Watcher::new(&paths);
        let idle = if watcher.fd().is_some() && fully { WATCHED } else { UNWATCHED };
        (watcher, idle)
    };
    // Asked to stop while still starting up: there is nothing to put back.
    if term::TERMINATED.load(Ordering::Relaxed) {
        exit(1);
    }
    if let Err(e) = term::enter() {
        fail(format!("cannot use this terminal: {e}"));
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::leave();
        hook(info);
    }));

    let mut screen = Screen::new();
    let mut keys = Vec::new();
    // When the last look for changes ended, how long it took, and whether
    // the operating system has reported a change since.
    let (mut looked, mut took, mut told) = (Instant::now(), Duration::ZERO, false);
    'run: while !term::TERMINATED.load(Ordering::Relaxed) {
        if term::RESIZED.swap(false, Ordering::Relaxed) {
            let (w, h) = term::size();
            screen.resize(w, h);
        }
        ui.draw(&mut screen);
        screen.flush();

        // A look is due soon after a change is reported, and now and then
        // regardless; either way it rests in proportion to how long the
        // last one took.
        let due = |told: bool| if told { SETTLE } else { idle }.max(took * REST);
        let until_look = due(told).saturating_sub(looked.elapsed());
        let wait = ui.next_frame().map_or(until_look, |frame| frame.min(until_look));
        match term::wait(wait.as_millis() as i32 + 1, watcher.fd()) {
            Wait::Input => {
                term::read_keys(&mut keys);
                for key in keys.drain(..) {
                    if ui.on_key(key) {
                        break 'run;
                    }
                }
            }
            Wait::Woken => {
                watcher.drain();
                told = true;
            }
            Wait::Timeout | Wait::Interrupted => {}
        }
        if looked.elapsed() >= due(told) {
            let started = Instant::now();
            // Too soon to tell, say in the middle of a checkout: look again
            // once things have settled.
            let settled = ui.check();
            (looked, took, told) = (Instant::now(), started.elapsed(), !settled);
        }
    }
    term::leave();
}
