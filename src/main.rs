//! strata: what in a git repository is waiting to be committed or pushed.

mod git;
mod history;
mod icons;
mod outline;
mod term;
mod tree;
mod ui;

use std::path::PathBuf;
use std::process::exit;
use std::sync::atomic::Ordering;

use term::{Key, Screen, Wait};

const USAGE: &str = "\
strata — what is waiting to be committed or pushed

  strata outline [PATH]   the project's files, coloured by what changed.
                          Fits its pane: a tree when tall, a strip when short.
  strata history [PATH]   the changed files, and how each one changed.

PATH is any directory inside a git repository (default: the current one).
Both update on their own as files change. q quits; ? in history lists keys.

Icons need a Nerd Font. STRATA_ICONS=plain uses ordinary symbols instead,
STRATA_ICONS=off shows none.
";

/// How often an idle view checks the repository for changes.
const IDLE_MS: i32 = 2000;

/// One full-pane screen of the program.
pub trait View {
    fn draw(&mut self, s: &mut Screen);
    /// Returns true when the program should quit.
    fn on_key(&mut self, key: Key) -> bool;
    /// Called when nothing has happened for a while.
    fn tick(&mut self);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let open: fn(PathBuf) -> Box<dyn View> = match command.as_str() {
        "outline" => |root| Box::new(outline::Outline::new(root)),
        "history" => |root| Box::new(history::History::new(root)),
        "-V" | "--version" => return println!("strata {}", env!("CARGO_PKG_VERSION")),
        "" | "-h" | "--help" => return print!("{USAGE}"),
        other => {
            eprintln!("strata: no command named {other:?}\n\n{USAGE}");
            exit(2);
        }
    };
    let dir = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let Some(root) = git::toplevel(&dir) else {
        eprintln!("strata: {} is not inside a git repository", dir.display());
        exit(1);
    };
    let mut view = open(PathBuf::from(root));
    if let Err(e) = term::enter() {
        eprintln!("strata: cannot use this terminal: {e}");
        exit(1);
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
        view.draw(&mut screen);
        screen.flush();
        match term::wait(IDLE_MS) {
            Wait::Input => {
                term::read_keys(&mut keys);
                for key in keys.drain(..) {
                    if view.on_key(key) {
                        break 'run;
                    }
                }
            }
            Wait::Timeout => view.tick(),
            Wait::Interrupted => {}
        }
    }
    term::leave();
}
