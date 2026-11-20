//! Parses the command line and dispatches commands.

mod diff;
#[cfg(test)]
mod fixture;
mod fs_ops;
mod git;
mod help;
mod history;
mod home;
mod icons;
mod json;
mod keys;
mod menubar;
mod outline;
mod pane;
mod paths;
mod picture;
mod preferences;
mod read;
mod reader;
mod rex;
mod screen;
mod setup;
mod syntax;
mod term;
mod titles;
mod tree;
mod ui;
mod uninstall;
mod usage;
mod walk;
mod watch;
mod web;
mod when;
mod width;
mod workflow;

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::process::exit;

use git::{CommitId, Repo};
use history::History;
use outline::Outline;
use pane::{Open, Pane, Start};
use reader::Reader;

const USAGE: &str = "\
milo — file, history, usage, and reader panes for your terminal

Usage: milo
       milo outline [PATH]
       milo history [PATH]
       milo usage [--menu-stream]
       milo menubar install | uninstall
       milo setup
       milo web [--port PORT] [--no-open]
       milo doctor
       milo setup --repair | --remove
       milo view FILE [--commit ID]
       milo PATH
       milo [setup] --skip-onboarding
       milo listen [--stop]
       milo claude-status
       milo --help | --version

Bare milo opens Home. milo PATH opens the outline at PATH.
On first launch, milo and milo setup share a short welcome guide.
--skip-onboarding marks the guide skipped and exits without opening a terminal or changing settings.
milo setup saves a Rex layout and can enable Claude Code usage limits.
milo web opens local Settings in your browser. Use --no-open to print the link without opening it.
milo doctor explains local preferences, integration health and usage sources.
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
  e             edit a working text file
  q or Esc      close

Editing in the reader (also opened by milo view FILE --edit):
  Ctrl-S        save
  Ctrl-Z Ctrl-Y undo, redo
  Esc           stop editing; asks first if there are unsaved changes
  Ctrl-G        help

Ctrl-C quits, or asks first while a file is being edited. Press ? for help
when the pane has room. The outline, history and reader support the mouse
wheel, Page Up/Down and Home/End.

Changed files are those waiting to be committed or pushed. Yellow is edited,
green new, red removed, cyan moved, and magenta in conflict. Uncommitted
names are bold; unpushed names are regular. In history, bold dots still
need pushing, faint dots are pushed, and a hollow dot is uncommitted work.
The reader colours supported code and marks added and removed lines. It
shows a picture as a picture where the terminal can draw one.
Use milo view FILE --commit ID to see the file as of that commit, not as
it is now. ID is a full or abbreviated Git commit ID in hex.
The reader only reads until you press e. Edits are a draft until saved, and
a save never replaces a file that changed on disk without asking. Files at
a commit, links, pictures and files that are not UTF-8 text stay read-only.

Choose icons in setup, or override them for one launch with MILO_ICONS=plain
or MILO_ICONS=off.

https://usemilo.net
";

fn command_help(name: &str) -> Option<&'static str> {
    Some(match name {
        "outline" => "Usage: milo outline [PATH]\n\nBrowse files and changes. PATH defaults to the current directory.",
        "history" => "Usage: milo history [PATH]\n\nBrowse recent commits. PATH defaults to the current directory.",
        "usage" => {
            "Usage: milo usage [--menu-stream]\n\nSee Codex and Claude Code limits and OpenCode activity.\n--menu-stream prints one JSON snapshot per line using global preferences. It refreshes about every 30 seconds or on a newline from stdin, and exits at EOF."
        }
        "menubar" => {
            "Usage: milo menubar install | uninstall\n\nInstall and open the optional macOS usage app, or remove it without changing Milo settings."
        }
        "view" => {
            "Usage: milo view FILE [--commit ID | --edit]\n\nRead a file now or as it was at a Git commit. --edit starts editing a working text file; in the reader, e does the same."
        }
        "setup" => {
            "Usage: milo setup\n       milo setup --skip-onboarding | --repair | --remove\n\nOpen Settings, skip the welcome guide without a terminal, or maintain managed integrations."
        }
        "web" => {
            "Usage: milo web [--port PORT] [--no-open]\n\nEdit local settings in your browser. The default port is 6110; use --port 0 for a free port. --no-open prints the private link without launching a browser."
        }
        "doctor" => {
            "Usage: milo doctor\n\nShow effective preferences, integration health and local usage sources without changing anything."
        }
        "listen" => {
            "Usage: milo listen [--stop]\n\nStart or stop the Rex listener that remembers sidebar sizes, arranges new tabs and titles tabs."
        }
        "claude-status" => "Usage: milo claude-status\n\nRead usage from Claude Code's status line input.",
        _ => return None,
    })
}

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
    ShellOutline(&'a OsStr),
    ShellHistory(&'a OsStr),
    PeekOutline(&'a OsStr),
    View { file: &'a OsStr, commit: Option<CommitId<'a>> },
    Edit(&'a OsStr),
    Path(&'a OsStr),
    Usage,
    UsageMenuStream,
    MenuBar(menubar::Action),
    Home,
    Setup,
    Web { open_browser: bool, port: u16 },
    SkipOnboarding,
    Doctor,
    Repair,
    Remove,
    Listen(bool),
    ClaudeStatus,
    Help(&'static str),
    Version,
}

/// Only known command words are text; a path can hold arbitrary bytes.
fn command<'a>(
    first: Option<&'a OsStr>,
    second: Option<&'a OsStr>,
    rest: &'a [OsString],
) -> Result<Command<'a>, String> {
    let name = first.and_then(OsStr::to_str);
    let option = second.and_then(OsStr::to_str);
    if matches!(
        (name, option),
        (Some("outline" | "history"), Some("--shell")) | (Some("outline"), Some("--peek-origin"))
    ) {
        let [block] = rest else { return Err("expected a Rex block ID".into()) };
        let valid = block
            .to_str()
            .and_then(|id| id.strip_prefix("block:"))
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'));
        if !valid {
            return Err("expected a Rex block ID".into());
        }
        return Ok(if option == Some("--peek-origin") {
            Command::PeekOutline(block)
        } else if name == Some("history") {
            Command::ShellHistory(block)
        } else {
            Command::ShellOutline(block)
        });
    }
    if !rest.is_empty() && !matches!(name, Some("view" | "web")) {
        return Err("too many arguments; see milo --help".to_string());
    }
    if rest.is_empty()
        && second.is_some_and(|arg| arg == "--help" || arg == "-h")
        && let Some(help) = name.and_then(command_help)
    {
        return Ok(Command::Help(help));
    }
    Ok(match (name, second) {
        (Some("menubar"), _) => Command::MenuBar(match option {
            Some("install") => menubar::Action::Install,
            Some("uninstall") => menubar::Action::Uninstall,
            _ => return Err("expected milo menubar install | uninstall".into()),
        }),
        (Some("outline"), path) => Command::Outline(path),
        (Some("history"), path) => Command::History(path),
        (Some("view"), Some(file)) => {
            let commit = match rest {
                [] => None,
                [flag] if flag == "--edit" => return Ok(Command::Edit(file)),
                [flag, id] if flag == "--commit" => {
                    Some(id.to_str().and_then(CommitId::parse).ok_or("commit ID must be 4 to 64 hex digits")?)
                }
                _ => return Err("expected milo view FILE [--commit ID | --edit]".to_string()),
            };
            Command::View { file, commit }
        }
        (Some("view"), None) => return Err("missing file; use milo view FILE [--commit ID | --edit]".to_string()),
        (Some("setup"), None) => Command::Setup,
        (Some("web"), first_flag) => {
            let mut open_browser = true;
            let mut port = None;
            let mut flags = first_flag.into_iter().chain(rest.iter().map(OsString::as_os_str));
            while let Some(flag) = flags.next() {
                match flag.to_str() {
                    Some("--no-open") if open_browser => open_browser = false,
                    Some("--port") if port.is_none() => {
                        port = Some(
                            flags
                                .next()
                                .and_then(OsStr::to_str)
                                .and_then(|s| s.parse::<u16>().ok())
                                .ok_or("--port requires a number from 0 to 65535")?,
                        );
                    }
                    _ => return Err("expected milo web [--port PORT] [--no-open]".into()),
                }
            }
            Command::Web { open_browser, port: port.unwrap_or(web::DEFAULT_PORT) }
        }
        (Some("--skip-onboarding"), None) => Command::SkipOnboarding,
        (Some("setup"), Some(flag)) if flag == "--skip-onboarding" => Command::SkipOnboarding,
        (Some("doctor"), None) => Command::Doctor,
        (Some("setup"), Some(flag)) if flag == "--repair" => Command::Repair,
        (Some("setup"), Some(flag)) if flag == "--remove" => Command::Remove,
        (Some("usage"), None) => Command::Usage,
        (Some("usage"), Some(flag)) if flag == "--menu-stream" => Command::UsageMenuStream,
        (Some("listen"), None) => Command::Listen(false),
        (Some("listen"), Some(flag)) if flag == "--stop" => Command::Listen(true),
        (Some("listen"), _) => return Err("expected milo listen [--stop]".to_string()),
        (Some("claude-status"), None) => Command::ClaudeStatus,
        (Some("setup" | "usage" | "claude-status" | "doctor"), Some(_)) => {
            return Err("unexpected argument; see milo --help".to_string());
        }
        (Some("-h" | "--help"), None) => Command::Help(USAGE),
        (Some("-V" | "--version"), None) => Command::Version,
        (Some(option), _) if option.starts_with('-') => return Err(format!("no option {option}; see milo --help")),
        (_, None) => match first {
            Some(path) => Command::Path(path),
            None => Command::Home,
        },
        _ => return Err("expected a command or path; see milo --help".to_string()),
    })
}

/// The runner opens panes after installing signal handlers; tests draw without a terminal.
fn dispatch(command: Command<'_>, run: impl FnOnce(Start<'_>, &Open<'_>) -> Result<(), String>) -> Result<(), String> {
    match command {
        Command::MenuBar(action) => print_report(menubar::run(action)),
        Command::Outline(path) => run(Start::Reading(project_path(path)), &|| open_outline(path)),
        Command::Path(path) => run(Start::Reading(Path::new(path)), &|| {
            open_outline(Some(path)).map_err(|why| format!("{why}; see milo --help for commands"))
        }),
        Command::ShellOutline(shell) => {
            run(Start::Reading(Path::new(".")), &|| Ok(workflow::project(open_repo(None)?, Some(shell), outline)))
        }
        Command::History(path) => {
            run(Start::Reading(project_path(path)), &|| Ok(workflow::project(open_repo(path)?, None, history)))
        }
        Command::ShellHistory(shell) => {
            run(Start::Reading(Path::new(".")), &|| Ok(workflow::project(open_repo(None)?, Some(shell), history)))
        }
        Command::PeekOutline(origin) => {
            let result = run(Start::Reading(Path::new(".")), &|| {
                Ok(workflow::project(open_repo(None)?, None, |repo, prefs| {
                    Box::new(Outline::with_preferences(repo, rex::inside(), prefs).into_peek())
                }))
            });
            setup::return_from_peek(origin);
            result
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
        Command::Edit(file) => run(Start::Reading(file_dir(Path::new(file))), &|| {
            let path = Path::new(file);
            let mut reader = Reader::open(path).ok_or_else(|| format!("{} is not in a directory", path.display()))?;
            // A file that cannot be edited opens for reading and says why.
            reader.edit(std::time::Instant::now());
            Ok(Box::new(reader))
        }),
        Command::Usage => run(Start::Check, &|| Ok(Box::new(usage::Usage::open()))),
        Command::UsageMenuStream => usage::menu_stream().map_err(|e| format!("could not stream usage: {e}")),
        Command::Web { open_browser, port } => web::run(open_browser, port),
        Command::Doctor => print_report(setup::doctor::report()),
        Command::Repair => print_report(setup::doctor::repair()),
        Command::Remove => print_report(setup::doctor::remove()),
        Command::SkipOnboarding => setup::skip_welcome(),
        Command::Home | Command::Setup => {
            let destination: fn() -> Result<Box<dyn Pane>, String> = if command == Command::Home {
                || Ok(Box::new(home::Home::new()))
            } else {
                || Ok(Box::new(setup::Setup::open()))
            };
            if setup::needs_welcome()? {
                run(Start::Welcome(destination), &|| Ok(Box::new(setup::Welcome::open()?)))
            } else {
                run(Start::Loaded, &destination)
            }
        }
        Command::Listen(stop) => exit(setup::listen(stop)),
        Command::ClaudeStatus => exit(usage::claude_status()),
        Command::Help(text) => print_line(text.trim_end()),
        Command::Version => print_line(concat!("milo ", env!("CARGO_PKG_VERSION"))),
    }
}

fn print_report(report: Result<String, String>) -> Result<(), String> {
    print_line(&report?)
}

fn print_line(text: &str) -> Result<(), String> {
    match writeln!(io::stdout().lock(), "{text}") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(format!("cannot write output: {e}")),
    }
}

fn open_outline(path: Option<&OsStr>) -> Result<Box<dyn Pane>, String> {
    Ok(workflow::project(open_repo(path)?, None, outline))
}

fn outline(repo: Repo, prefs: &preferences::Preferences) -> Box<dyn Pane> {
    Box::new(Outline::with_preferences(repo, rex::inside(), prefs))
}

fn history(repo: Repo, prefs: &preferences::Preferences) -> Box<dyn Pane> {
    Box::new(
        History::new(repo, rex::inside())
            .with_follow_new(prefs.history_follow_new)
            .with_appearance(prefs.icons.effective(), prefs.text_git_marks),
    )
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    // Two helpers for the installed Rex files. Neither opens a pane.
    if first.as_deref() == Some(OsStr::new("layout-profile")) {
        exit(setup::layout_profile(&args.collect::<Vec<_>>()));
    }
    if first.as_deref() == Some(OsStr::new("tab-title")) {
        exit(titles::command(&args.collect::<Vec<_>>()));
    }
    let second = args.next();
    let rest: Vec<_> = args.collect();
    let command = command(first.as_deref(), second.as_deref(), &rest).unwrap_or_else(|why| fail(&why));
    let result = dispatch(command, pane::run);
    if term::TERMINATED.load(std::sync::atomic::Ordering::Relaxed) {
        exit(1);
    }
    result.unwrap_or_else(|why| fail(&why));
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
        let command = command(first, second, &[]).unwrap();
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
        let command = command(Some(OsStr::new("view")), Some(file.as_os_str()), &args).unwrap();
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
            let command = command(Some(OsStr::new("view")), Some(OsStr::new("-notes")), &args);
            assert_eq!(command, Err("commit ID must be 4 to 64 hex digits".to_string()));
        }
    }

    #[test]
    fn home_and_explicit_paths_have_separate_destinations() {
        assert_eq!(command(None, None, &[]), Ok(Command::Home));
        let directory = OsStr::new("./work");
        assert_eq!(command(Some(directory), None, &[]), Ok(Command::Path(directory)));
    }

    #[test]
    fn setup_usage_and_helper_commands_reach_separate_destinations() {
        for (name, arg, expected) in [
            ("setup", None, Command::Setup),
            ("web", None, Command::Web { open_browser: true, port: web::DEFAULT_PORT }),
            ("web", Some("--no-open"), Command::Web { open_browser: false, port: web::DEFAULT_PORT }),
            ("--skip-onboarding", None, Command::SkipOnboarding),
            ("setup", Some("--skip-onboarding"), Command::SkipOnboarding),
            ("usage", None, Command::Usage),
            ("usage", Some("--menu-stream"), Command::UsageMenuStream),
            ("claude-status", None, Command::ClaudeStatus),
            ("listen", None, Command::Listen(false)),
            ("listen", Some("--stop"), Command::Listen(true)),
        ] {
            assert_eq!(command(Some(OsStr::new(name)), arg.map(OsStr::new), &[]), Ok(expected));
        }
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
    fn an_unknown_word_points_to_the_commands() {
        let f = fixture::Fixture::folder();
        let missing = f.root.join("not-a-milo-command");
        let command = command(Some(missing.as_os_str()), None, &[]).unwrap();
        let Err(why) = open_pane(command) else {
            panic!("unknown word opened a pane");
        };
        assert!(why.contains("not-a-milo-command") && why.contains("milo --help for commands"), "{why}");
        assert_eq!(why.lines().count(), 1);
    }
}
