//! strata: what changed in a git repository, and when.

mod app;
mod git;
mod term;
mod tree;
mod ui;
mod when;

use std::path::PathBuf;
use std::process::exit;
use std::sync::atomic::Ordering;

use app::App;
use term::{Screen, Wait};

const USAGE: &str = "\
strata — what changed in a git repository, and when

Usage: strata [PATH]

PATH is any directory inside a git repository (default: the current one).

Pick a moment in the timeline, see the files it touched, read the change.
Enter goes deeper, Esc comes back. Press ? inside for every key.
";

/// How long the cursor must rest before the change for a row is loaded.
const SETTLE_MS: i32 = 40;
/// How often an idle viewer checks the repository for outside changes.
const IDLE_MS: i32 = 2000;

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
    let mut app = App::new(PathBuf::from(root));
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
        app.draw(&mut screen);
        screen.flush();
        let pending = app.detail_dirty || app.need_stats;
        match term::wait(if pending { SETTLE_MS } else { IDLE_MS }) {
            Wait::Input => {
                term::read_keys(&mut keys);
                for key in keys.drain(..) {
                    if app.on_key(key) {
                        break 'run;
                    }
                }
            }
            Wait::Timeout if app.detail_dirty => app.load_detail(false),
            Wait::Timeout if app.need_stats => app.load_stats(),
            Wait::Timeout => app.tick(),
            Wait::Interrupted => {}
        }
    }
    term::leave();
}
