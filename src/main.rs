//! strata: a project's files, coloured by what is not yet committed or pushed.

mod git;
mod icons;
mod outline;
mod term;
mod tree;
mod ui;

use std::path::PathBuf;
use std::process::exit;
use std::sync::atomic::Ordering;

use outline::Outline;
use term::{Screen, Wait};

const USAGE: &str = "\
strata — a project's files, coloured by what is not yet committed or pushed

Usage: strata [PATH]

PATH is any directory inside a git repository (default: the current one).
It fits its pane: a file tree when tall, a strip of the changed files when
short, with what happened to each when wide. It updates on its own.

  ↑ ↓ or j k    move                enter    open or close a folder
  e             open every folder   c        changed files only
  r             reload              q        quit

Colour is the change: yellow edited, green new, red removed, cyan moved.
Bold is not committed yet; regular weight is committed, waiting to be pushed.

Icons need a Nerd Font. STRATA_ICONS=plain uses ordinary symbols instead,
STRATA_ICONS=off shows none.
";

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("-h" | "--help") => return print!("{USAGE}"),
        Some("-V" | "--version") => return println!("strata {}", env!("CARGO_PKG_VERSION")),
        _ => {}
    }
    let dir = PathBuf::from(arg.unwrap_or_else(|| ".".into()));
    let Some(root) = git::toplevel(&dir) else {
        eprintln!("strata: {} is not inside a git repository", dir.display());
        exit(1);
    };
    let mut outline = Outline::new(PathBuf::from(root));
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
        outline.draw(&mut screen);
        screen.flush();
        match term::wait(outline.idle_ms()) {
            Wait::Input => {
                term::read_keys(&mut keys);
                for key in keys.drain(..) {
                    if outline.on_key(key) {
                        break 'run;
                    }
                }
            }
            Wait::Timeout => outline.tick(),
            Wait::Interrupted => {}
        }
    }
    term::leave();
}
