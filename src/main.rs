//! Parses the command line and dispatches commands.

mod diff;
#[cfg(test)]
mod fixture;
mod fs_ops;
mod git;
mod help;
mod history;
mod icons;
mod json;
mod keys;
mod outline;
mod pane;
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

use git::{CommitId, Repo};
use history::History;
use outline::Outline;
use pane::{Open, Pane, Start};
use reader::Reader;

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

/// Ends the program with a message. A path in it may hold anything, so
/// control characters are left out.
fn fail(message: &str) -> ! {
    let safe: String = message.chars().filter(|c| !c.is_control()).collect();
    eprintln!("milo: {safe}");
    exit(1);
}

fn project_path(path: Option<&OsStr>) -> &Path {
    path.map_or(Path::new("."), Path::new)
}

fn file_dir(path: &Path) -> &Path {
    path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

fn open_repo(path: Option<&OsStr>) -> Result<Repo, String> {
    let dir = project_path(path);
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
    Setup { offered: bool },
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
        (Some("setup"), None) => Command::Setup { offered: false },
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
            None if offer_setup() => Command::Setup { offered: true },
            None => Command::Outline(None),
        },
        _ => return Err("expected a command or path; see milo --help".to_string()),
    })
}

/// The runner opens panes after installing signal handlers; tests draw without a terminal.
fn dispatch(command: Command<'_>, run: impl FnOnce(Start<'_>, &Open<'_>) -> Result<(), String>) -> Result<(), String> {
    match command {
        Command::Outline(path) => run(Start::Reading(project_path(path)), &|| open_outline(path)),
        Command::Path(path) => run(Start::Reading(Path::new(path)), &|| {
            open_outline(Some(path)).map_err(|why| format!("{why}; commands: outline, history, view, usage, setup"))
        }),
        Command::History(path) => {
            run(Start::Reading(project_path(path)), &|| Ok(Box::new(History::new(open_repo(path)?, rex::inside()))))
        }
        Command::View { file, commit } => run(Start::Reading(file_dir(Path::new(file))), &|| {
            let path = Path::new(file);
            let reader = if let Some(id) = commit {
                Reader::at_commit(path, id)?
            } else {
                Reader::open(path).ok_or_else(|| format!("{} is not in a directory", path.display()))?
            };
            Ok(Box::new(reader))
        }),
        Command::Usage => run(Start::Check, &|| Ok(Box::new(usage::Usage::open()))),
        Command::Setup { offered } => {
            let start = if offered { Start::Offer(|| open_outline(None)) } else { Start::Loaded };
            run(start, &|| Ok(Box::new(setup::Setup::open(offered))))
        }
        Command::Listen(stop) => exit(setup::listen(stop)),
        Command::ClaudeStatus => exit(usage::claude_status()),
        Command::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Command::Version => {
            println!("milo {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn open_outline(path: Option<&OsStr>) -> Result<Box<dyn Pane>, String> {
    Ok(Box::new(Outline::new(open_repo(path)?, rex::inside())))
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let second = args.next();
    let rest: Vec<_> = args.collect();
    let command =
        command(first.as_deref(), second.as_deref(), &rest, setup::offer_once).unwrap_or_else(|why| fail(&why));
    dispatch(command, pane::run).unwrap_or_else(|why| fail(&why));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Screen;

    fn open_pane(command: Command<'_>) -> Result<Box<dyn Pane>, String> {
        let mut pane = None;
        dispatch(command, |_, open| {
            pane = Some(open()?);
            Ok(())
        })?;
        Ok(pane.expect("tests dispatch only pane commands"))
    }

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
    fn every_project_help_row_fits_a_28_column_pane() {
        let f = fixture::Fixture::new();
        let plain = fixture::Fixture::folder();
        f.write("page.txt", "a file\n");
        f.commit("first");
        let file = f.root.join("page.txt");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let commits = repo.history(1).unwrap();
        let commands = [
            Command::Outline(Some(f.root.as_os_str())),
            Command::Outline(Some(plain.root.as_os_str())),
            Command::History(Some(f.root.as_os_str())),
            Command::View { file: file.as_os_str(), commit: None },
            Command::View { file: file.as_os_str(), commit: CommitId::parse(&commits[0].hash) },
        ];
        for command in commands {
            open_pane(command).unwrap().help().assert_rows_fit(26);
        }
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
            ("setup", None, Command::Setup { offered: false }),
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
        assert_eq!(command(None, None, &[], || true), Ok(Command::Setup { offered: true }));
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
}
