//! strata: a project's files, coloured by what is not yet committed or pushed.

mod git;
mod icons;
mod outline;
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

Colour is the change: yellow edited, green new, red removed, cyan moved.
Bold is not committed yet; regular weight is committed, waiting to be pushed.

Icons need a Nerd Font. STRATA_ICONS=plain uses ordinary symbols instead,
STRATA_ICONS=off shows none.
";

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

    /// Called when nothing has happened for `idle_ms`.
    fn tick(&mut self) {
        match self {
            Ui::Outline(o) => o.tick(),
            Ui::View(v) => v.tick(),
        }
    }

    /// Called when the operating system reports a change under the root.
    fn changed(&mut self) {
        match self {
            Ui::Outline(o) => o.changed(),
            Ui::View(v) => v.changed(),
        }
    }

    fn idle_ms(&self) -> i32 {
        match self {
            Ui::Outline(o) => o.idle_ms(),
            Ui::View(v) => v.idle_ms(),
        }
    }
}

fn fail(message: String) -> ! {
    eprintln!("strata: {message}");
    exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut ui, root) = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["-h" | "--help"] => return print!("{USAGE}"),
        ["-V" | "--version"] => return println!("strata {}", env!("CARGO_PKG_VERSION")),
        ["view", file] => {
            let Some(view) = View::open(Path::new(file)) else { fail(format!("{file} is not in a directory")) };
            let root = view.root().to_path_buf();
            (Ui::View(view), root)
        }
        [] | [_] => {
            let dir = PathBuf::from(args.first().map_or(".", String::as_str));
            let Some(repo) = Repo::open(&dir) else { fail(format!("{} is not a directory", dir.display())) };
            let root = repo.root.clone();
            (Ui::Outline(Outline::new(repo)), root)
        }
        _ => fail(format!("too many arguments\n\n{USAGE}")),
    };
    let watcher = Watcher::new(&root);
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
    'run: while !term::TERMINATED.load(Ordering::Relaxed) {
        if term::RESIZED.swap(false, Ordering::Relaxed) {
            let (w, h) = term::size();
            screen.resize(w, h);
        }
        ui.draw(&mut screen);
        screen.flush();
        match term::wait(ui.idle_ms(), watcher.fd()) {
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
                ui.changed();
            }
            Wait::Timeout => ui.tick(),
            Wait::Interrupted => {}
        }
    }
    term::leave();
}
