//! Opens the requested pane and runs the terminal event loop.

mod diff;
#[cfg(test)]
mod fixture;
mod git;
mod help;
mod history;
mod icons;
mod keys;
mod outline;
mod paths;
mod picture;
mod read;
mod reader;
mod rex;
mod screen;
mod setup;
mod syntax;
mod term;
mod tree;
mod ui;
mod usage;
mod walk;
mod watch;
mod when;
mod width;

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::exit;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use git::{CommitId, Repo};
use help::Page;
use history::History;
use keys::{Key, receive};
use outline::Outline;
use reader::Reader;
use screen::Screen;
use term::Wait;
use ui::Pane;
use watch::Watcher;

const USAGE: &str = "\
milo — file, history, usage, and reader panes for your terminal

Usage: milo outline [PATH]
       milo history [PATH]
       milo usage
       milo setup
       milo view FILE [--commit ID]
       milo [PATH]
       milo listen [--stop]
       milo claude-status
       milo --help | --version

PATH defaults to the current directory. Bare milo [PATH] opens the outline.
Inside Rex, a bare milo offers setup the first time if no layout is installed.
milo setup saves a Rex layout and can enable Claude Code usage limits.
milo listen and milo claude-status are used by Rex and Claude Code.
To open a directory named like a command, use a path such as ./history.
In a git repository the outline shows the whole project and history shows
its recent commits. Both stay up to date on their own. Elsewhere the outline
is a plain file tree. Short git outlines show a strip of changed files;
wider trees show line counts and what happened to each file.

Outline (tree):
  ↑↓ or k j     move
  Enter         toggle a folder or open a file
  Space         toggle a folder
  ← or h        close an open folder or go to the parent
  → or l        open a closed folder
  { }           previous/next visible folder
  K J           previous/next item in the same folder
  p             go to the parent folder
  e             open every folder or restore earlier choices
  n N           next/previous changed file
  c             switch between all files and changed files only
  r             reload
  q             quit

In the strip, use ← → or h l to move between columns. In the tree, click a
folder to toggle it, click a file to select it, or double-click to open it.
Files open in a floating reader inside Rex and in the same pane elsewhere.

History:
  ↑↓ or k j     move
  Enter         show or hide files, show all, or read a file
  ← or h        go to the commit from its files, or close an open commit
  r             reload
  q             quit

Click a commit to show or hide its files, or click a file to select it.
Double-click a file to read it.

Reader (also opened by milo view):
  ↑↓ or k j     scroll by a line
  ←→ or h l     scroll sideways
  Space / b     page down/up
  g G           top/bottom
  n N           next/previous block of changed lines
  q or Esc      close

In every pane, ? opens help and Ctrl-C quits. The outline, history and
reader support the mouse wheel, Page Up/Down and Home/End.

Changed files are those waiting to be committed or pushed. Yellow is edited,
green new, red removed, cyan moved, and magenta in conflict. Uncommitted
names are bold; unpushed names are regular. In history, bold dots still
need pushing, faint dots are pushed, and a hollow dot is uncommitted work.
The reader colours supported code and marks added and removed lines. It
shows a picture as a picture where the terminal can draw one.
Use milo view FILE --commit ID to see the file as of that commit, not as
it is now. ID is a full or abbreviated Git commit ID in hex.

Icons use a Nerd Font by default. Set MILO_ICONS=plain for ordinary
symbols or MILO_ICONS=off to hide icons.

https://usemilo.net
";

/// Minimum rest after a look if the watcher has reported a change.
const MIN_LOOK_INTERVAL: Duration = Duration::from_millis(120);
/// Rest at least this many times the cost of the last look, so repeated
/// looks at the same cost take at most a fifth of the time.
const REST: u32 = 4;
/// Time between looks when the watcher is expected to cover the project.
const WATCHED: Duration = Duration::from_secs(30);
/// Time between looks when the watcher cannot cover the project.
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

    /// Time until the event loop next calls `Pane::check`.
    fn until_look(&self, now: Instant) -> Duration {
        let rest = if self.reported { MIN_LOOK_INTERVAL } else { self.idle }.max(self.took * REST);
        rest.saturating_sub(now.saturating_duration_since(self.ended))
    }

    /// An unsettled look, say during a checkout, is retried as if a change was reported.
    fn finish_look(&mut self, started: Instant, ended: Instant, settled: bool) {
        (self.ended, self.took, self.reported) = (ended, ended - started, !settled);
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
/// Returns true when the program should quit.
fn route_key(pane: &mut dyn Pane, screen: &mut Screen, help: &mut Option<Page>, key: Key) -> bool {
    match key {
        Key::Ctrl(b'c') | Key::Eof => return true,
        Key::Background(rgb) => screen.set_background(rgb),
        Key::Pictures(able) => screen.set_pictures(able),
        Key::CellSize(width, height) => screen.set_cell_size(width, height),
        Key::Answered => screen.answered(),
        // The pane still sees focus changes while its help is up.
        Key::Focus(_) => return pane.on_key(key),
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
                return pane.on_key(key);
            }
        }
    }
    false
}

/// Ends the program with a message. A path in it may hold anything, so
/// control characters are left out.
fn fail(message: &str) -> ! {
    let safe: String = message.chars().filter(|c| !c.is_control()).collect();
    eprintln!("milo: {safe}");
    exit(1);
}

fn open_repo(path: Option<&OsStr>) -> Result<Repo, String> {
    let dir = path.map_or(Path::new("."), Path::new);
    Repo::open(dir).ok_or_else(|| {
        if dir.is_file() {
            format!("{0} is a file; to read it: milo view {0}", dir.display())
        } else {
            format!("{} is not a directory", dir.display())
        }
    })
}

#[derive(Debug, PartialEq, Eq)]
enum Command<'a> {
    Outline(Option<&'a OsStr>),
    History(Option<&'a OsStr>),
    View { file: &'a OsStr, commit: Option<CommitId<'a>> },
    Path(&'a OsStr),
    Usage,
    Setup,
    Listen(bool),
    ClaudeStatus,
    Help,
    Version,
}

/// Only known command words are text; a path can hold arbitrary bytes.
fn command<'a>(
    first: Option<&'a OsStr>,
    second: Option<&'a OsStr>,
    rest: &'a [OsString],
    offer_setup: impl FnOnce() -> bool,
) -> Result<Command<'a>, String> {
    if !rest.is_empty() && first != Some(OsStr::new("view")) {
        return Err("too many arguments; see milo --help".to_string());
    }
    Ok(match (first.and_then(OsStr::to_str), second) {
        (Some("outline"), path) => Command::Outline(path),
        (Some("history"), path) => Command::History(path),
        (Some("view"), Some(file)) => {
            let commit = match rest {
                [] => None,
                [flag, id] if flag == "--commit" => {
                    Some(id.to_str().and_then(CommitId::parse).ok_or("commit ID must be 4 to 64 hex digits")?)
                }
                _ => return Err("expected milo view FILE [--commit ID]".to_string()),
            };
            Command::View { file, commit }
        }
        (Some("view"), None) => return Err("which file? milo view FILE [--commit ID]".to_string()),
        (Some("setup"), None) => Command::Setup,
        (Some("usage"), None) => Command::Usage,
        (Some("listen"), None) => Command::Listen(false),
        (Some("listen"), Some(flag)) if flag == "--stop" => Command::Listen(true),
        (Some("listen"), _) => return Err("expected milo listen [--stop]".to_string()),
        (Some("claude-status"), None) => Command::ClaudeStatus,
        (Some("setup" | "usage" | "claude-status"), Some(_)) => {
            return Err("unexpected argument; see milo --help".to_string());
        }
        (Some("-h" | "--help"), None) => Command::Help,
        (Some("-V" | "--version"), None) => Command::Version,
        (Some(option), _) if option.starts_with('-') => return Err(format!("no option {option}; see milo --help")),
        (_, None) => match first {
            Some(path) => Command::Path(path),
            None if offer_setup() => Command::Setup,
            None => Command::Outline(None),
        },
        _ => return Err("expected a command or path; see milo --help".to_string()),
    })
}

fn open_pane(command: Command<'_>) -> Result<Box<dyn Pane>, String> {
    match command {
        Command::Outline(path) => Ok(Box::new(Outline::new(open_repo(path)?, rex::inside()))),
        Command::Path(path) => {
            let repo = open_repo(Some(path))
                .map_err(|why| format!("{why}; commands: outline, history, view, usage, setup"))?;
            Ok(Box::new(Outline::new(repo, rex::inside())))
        }
        Command::History(path) => Ok(Box::new(History::new(open_repo(path)?, rex::inside()))),
        Command::View { file, commit: Some(id) } => Ok(Box::new(Reader::at_commit(Path::new(file), id)?)),
        Command::View { file, commit: None } => Ok(Box::new(
            Reader::open(Path::new(file))
                .ok_or_else(|| format!("{} is not in a directory", Path::new(file).display()))?,
        )),
        Command::Usage => Ok(Box::new(usage::Usage::open())),
        Command::Setup => Ok(Box::new(setup::Setup::open())),
        _ => unreachable!("non-pane commands are handled first"),
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let second = args.next();
    let rest: Vec<_> = args.collect();
    let command =
        command(first.as_deref(), second.as_deref(), &rest, setup::offer_once).unwrap_or_else(|why| fail(&why));
    match command {
        Command::Help => return print!("{USAGE}"),
        Command::Version => return println!("milo {}", env!("CARGO_PKG_VERSION")),
        Command::Listen(stop) => exit(setup::listen(stop)),
        Command::ClaudeStatus => exit(usage::claude_status()),
        _ => {}
    }
    let signal = term::listen();
    let mut pane = open_pane(command).unwrap_or_else(|why| fail(&why));
    let (watcher, idle) = {
        let scope = pane.watch_scope();
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
            if !Page::fits(&screen) {
                help = None;
            }
        }
        draw_pane(pane.as_mut(), &mut screen, help.as_mut());
        screen.flush();

        let until_look = pace.until_look(Instant::now());
        let wait = if help.is_some() {
            until_look
        } else {
            pane.next_frame().map_or(until_look, |frame| frame.min(until_look))
        };
        let timeout_ms = i32::try_from(wait.as_millis() + 1).unwrap_or(i32::MAX);
        match term::wait(&keyboard, signal.as_ref(), timeout_ms, watcher.fd()) {
            Wait::Input => {
                receive(&mut keyboard, &mut keys);
                for key in keys.drain(..) {
                    if route_key(pane.as_mut(), &mut screen, &mut help, key) {
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
        if pace.until_look(Instant::now()).is_zero() {
            let started = Instant::now();
            let settled = pane.check();
            pace.finish_look(started, Instant::now(), settled);
        }
    }
    // A picture is not part of the text, so it is taken down by name.
    screen.clear();
    screen.flush();
    term::leave();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_frame(first: Option<&OsStr>, second: Option<&OsStr>) -> Vec<String> {
        let command = command(first, second, &[], || false).unwrap();
        let mut pane = open_pane(command).unwrap();
        let mut screen = Screen::sized(65, 12);
        pane.draw(&mut screen);
        screen.text()
    }

    #[test]
    fn each_command_opens_its_own_pane() {
        let f = fixture::Fixture::new();
        f.write("page.txt", "a file\n");
        f.commit("first commit");
        let dir = f.root.as_os_str();
        let outline = command_frame(Some(OsStr::new("outline")), Some(dir));
        assert!(outline.iter().any(|row| row.contains("page.txt")));
        assert!(!outline.iter().any(|row| row.contains("first commit")));
        assert_eq!(command_frame(Some(dir), None), outline);

        let history = command_frame(Some(OsStr::new("history")), Some(dir));
        assert!(history.iter().any(|row| row.contains("first commit")));
        assert!(!history.iter().any(|row| row.contains("page.txt")));
        let file = f.root.join("page.txt");
        let reader = command_frame(Some(OsStr::new("view")), Some(file.as_os_str()));
        assert!(reader[0].contains("page.txt"));
        assert!(reader[1].ends_with("a file"));
    }

    #[test]
    fn the_view_command_reads_a_dash_named_file_at_an_abbreviated_commit_id() {
        let f = fixture::Fixture::new();
        f.write("-notes.txt", "stored content\n");
        f.commit("first");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let commits = repo.history(1).unwrap();
        let id = &commits[0].hash[..12];
        f.write("-notes.txt", "working content\n");

        let file = f.root.join("-notes.txt");
        let args = ["--commit".into(), id.into()];
        let command = command(Some(OsStr::new("view")), Some(file.as_os_str()), &args, || false).unwrap();
        let mut pane = open_pane(command).unwrap();
        assert!(pane.watch_scope().roots.is_empty());
        let mut screen = Screen::sized(65, 12);
        pane.draw(&mut screen);
        let rows = screen.text();
        assert!(rows[0].contains("-notes.txt") && rows[0].contains(&format!("commit {}", &id[..7])));
        assert_eq!(rows[1], " 1 + stored content");
        assert!(!rows.iter().any(|row| row.contains("working content")));
    }

    #[test]
    fn the_view_command_rejects_non_hex_ids_in_argument_parsing() {
        for id in ["--all", "HEAD", "123", "b...e", "abc\n", &"f".repeat(65)] {
            let args = ["--commit".into(), id.into()];
            let command = command(Some(OsStr::new("view")), Some(OsStr::new("-notes")), &args, || false);
            assert_eq!(command, Err("commit ID must be 4 to 64 hex digits".to_string()));
        }
    }

    #[test]
    fn no_arguments_choose_the_outline_in_the_current_directory() {
        assert_eq!(command(None, None, &[], || false), Ok(Command::Outline(None)));
    }

    #[test]
    fn setup_usage_and_helper_commands_reach_separate_destinations() {
        for (name, arg, expected) in [
            ("setup", None, Command::Setup),
            ("usage", None, Command::Usage),
            ("claude-status", None, Command::ClaudeStatus),
            ("listen", None, Command::Listen(false)),
            ("listen", Some("--stop"), Command::Listen(true)),
        ] {
            assert_eq!(
                command(Some(OsStr::new(name)), arg.map(OsStr::new), &[], || panic!(
                    "explicit commands never offer setup"
                )),
                Ok(expected)
            );
        }
    }

    #[test]
    fn only_bare_milo_can_offer_setup() {
        assert_eq!(command(None, None, &[], || true), Ok(Command::Setup));
        let directory = OsStr::new("./work");
        assert_eq!(command(Some(directory), None, &[], || panic!("path was explicit")), Ok(Command::Path(directory)));
    }

    #[test]
    fn a_directory_named_like_a_command_is_reachable_by_path() {
        let f = fixture::Fixture::folder();
        f.write("history/page.txt", "a file\n");
        let path = f.root.join("./history");
        let rows = command_frame(Some(path.as_os_str()), None);
        assert_eq!(rows[0], " history");
        assert!(rows.iter().any(|row| row.contains("page.txt")));
    }

    #[test]
    fn an_unknown_word_names_the_commands() {
        let f = fixture::Fixture::folder();
        let missing = f.root.join("not-a-milo-command");
        let command = command(Some(missing.as_os_str()), None, &[], || false).unwrap();
        let Err(why) = open_pane(command) else {
            panic!("unknown word opened a pane");
        };
        assert!(why.contains("not-a-milo-command") && why.contains("outline, history, view"), "{why}");
        assert_eq!(why.lines().count(), 1);
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

        assert!(!route_key(&mut pane, &mut screen, &mut help, Key::Char('?')));
        draw_pane(&mut pane, &mut screen, help.as_mut());
        let first = screen.text();
        assert!(first[0].trim_start().starts_with("Files"));
        assert!(first[11].contains("Esc close") && first[11].ends_with("q close"));
        assert!(!route_key(&mut pane, &mut screen, &mut help, Key::PgDn));
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert_eq!(screen.text()[0], first[10]);
        assert!(!route_key(&mut pane, &mut screen, &mut help, Key::PgUp));
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert_eq!(screen.text(), first);
        assert!(!route_key(&mut pane, &mut screen, &mut help, Key::Esc));
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
        assert!(!route_key(&mut pane, &mut screen, &mut help, Key::Char('?')));
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert_eq!(screen.text(), before);
        assert!(help.is_none());
    }

    #[test]
    fn help_uses_the_nested_reader_page_and_terminal_quit_bypasses_it() {
        let f = fixture::Fixture::new();
        f.write("page.txt", "a file\n");
        let mut pane = Outline::new(Repo::open(&f.root).unwrap(), false);
        let mut screen = Screen::sized(65, 12);
        let mut help = None;
        assert!(!route_key(&mut pane, &mut screen, &mut help, Key::Enter));
        assert!(!route_key(&mut pane, &mut screen, &mut help, Key::Char('?')));
        draw_pane(&mut pane, &mut screen, help.as_mut());
        assert!(screen.text()[0].trim_start().starts_with("Lines"));
        assert!(route_key(&mut pane, &mut screen, &mut help, Key::Ctrl(b'c')));
        assert!(route_key(&mut pane, &mut screen, &mut help, Key::Eof));
    }
}
