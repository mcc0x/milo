//! Parses the command line and dispatches commands.

mod alerts;
mod diff;
#[cfg(test)]
mod fixture;
mod format;
mod fs_ops;
mod git;
mod handoff;
mod help;
mod history;
mod home;
mod host;
mod icons;
mod json;
mod keys;
mod menubar;
mod outline;
mod pane;
mod paths;
mod picture;
mod preferences;
mod presence;
mod preview;
mod read;
mod reader;
mod review;
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
use reader::{Reader, Tabs};

const USAGE: &str = "\
milo — file, history, usage, and reader panes for your terminal

Usage: milo
       milo outline [PATH]
       milo history [PATH]
       milo usage [--menu-stream]
       milo menubar install | uninstall
       milo setup
       milo web [--port PORT] [--no-open]
       milo web --host HOST [--project DIR] [--port PORT] [--no-open]
       milo preview URL [--placement right|left|above|below]
       milo settings export GROUPS | import FILE [--apply] | undo
       milo doctor
       milo setup --repair | --remove
       milo presence [status] | enable | disable
       milo presence details DETAILS | application ID
       milo view FILE [--commit ID | --edit]
       milo review PATCH [--from NAME] [--float]
       milo PATH
       milo [setup] --skip-onboarding
       milo listen [--stop]
       milo alerts [--help]
       milo claude-status
       milo --help [COMMAND] | --version

Bare milo opens Home. milo PATH opens the outline at PATH.
milo COMMAND --help, or milo --help COMMAND, explains one command briefly.
On first launch, milo and milo setup share a short welcome guide.
--skip-onboarding marks the guide skipped and exits without opening a terminal
  or changing settings.
milo setup saves a Rex layout and can enable Claude Code usage limits.
milo web opens local Settings in your browser. Use --no-open to print the link
  without opening it.
milo web --host HOST opens the Settings of another Rex host here, through a Rex
  forward that ends with it.
milo preview opens a local dev site in a tiled split beside the main terminal in
  this Rex tab on macOS. --placement applies to this launch only.
milo doctor explains local preferences, integration health and usage sources.
milo alerts mirrors Rex alerts to a Telegram chat or Discord channel you choose.
  It is off until you turn it on.
milo presence shows or changes the optional Discord Rich Presence, which is off
  until you turn it on.
milo settings copies chosen pane settings to another host as a file, shows what
  an import would change, and undoes it.
milo listen and milo claude-status are used by Rex and Claude Code.
To open a directory named like a command, use a path such as ./history.
In a git repository the outline shows the whole project and history shows
its recent commits. Both stay up to date on their own. Elsewhere the outline
is a plain file tree. Short git outlines show a strip of changed files;
wider trees show line counts and what happened to each file.

Outline (tree):
  ↑↓ or k j     move
  Enter         toggle a folder or open a file
  /             find a file by its name or path
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
  c             show only the changed lines, or the full file
  Tab Enter     choose the next run of hidden lines, show it
  e             edit a working text file
  q or Esc      close

Editing in the reader (also opened by milo view FILE --edit):
  Ctrl-S        save
  Ctrl-Z Ctrl-Y undo, redo
  Ctrl-F        format with an installed formatter; undo takes it back
  Esc           stop editing; asks first if there are unsaved changes
  Ctrl-G        help

Tabs in the reader, once the outline or history in Rex has opened a second file:
  Ctrl-N Ctrl-P next/previous tab
  Ctrl-W        close this tab; asks first if it has unsaved changes

Ctrl-C quits; while a file is being edited it stops editing, and asks first
if there are unsaved edits, in any tab. Press ? for help
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
milo review PATCH reads a unified diff that a coding agent wrote of its own
edits, and shows each file as it is now with only those edits marked. [ and ]
move between the patch's files and r reads them again. Milo checks that a
file still holds the patch's lines, not who wrote them, and shows no diff
for a file that no longer does. --from NAME says whose patch it is; --float
opens the review in a floating Rex pane from a program with no terminal.
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
        "review" => {
            "Usage: milo review PATCH [--from NAME] [--float]\n\nRead the edits in PATCH, a unified diff that a coding agent wrote of its own edits. Each file is shown as it is now with only those edits marked; other uncommitted work reads as unchanged. Milo checks that the file still holds the patch's lines, not who wrote them. A file that no longer holds them shows no diff.\n--from NAME shows whose patch it is said to be.\n--float opens the review in a floating Rex pane and prints what it shows, for a program with no terminal of its own."
        }
        "setup" => {
            "Usage: milo setup\n       milo setup --skip-onboarding | --repair | --remove\n\nOpen Settings, skip the welcome guide without a terminal, or maintain managed integrations."
        }
        "web" => {
            "Usage: milo web [--port PORT] [--no-open]\n       milo web --host HOST [--project DIR] [--port PORT] [--no-open]\n\nEdit local settings in your browser. The default port is 6110; use --port 0 for a free port. --no-open prints the private link without launching a browser.\n\n--host opens the settings of another Rex host on this computer. HOST is a label from `rex hosts` or a server address, as for `rex -S`. Milo starts its editor on that host, carries it here with `rex forward` on 127.0.0.1 only, and closes both when you choose Done or press Ctrl-C. --project names a folder on the host, to edit that worktree's settings. The host needs this version of milo on the search path of its login shell."
        }
        "preview" => preview::HELP,
        "doctor" => {
            "Usage: milo doctor\n\nShow effective preferences, integration health and local usage sources without changing anything."
        }
        "presence" => presence::HELP,
        "settings" => setup::transfer::HELP,
        "listen" => {
            "Usage: milo listen [--stop]\n\nStart or stop the Rex listener that remembers sidebar sizes, arranges new tabs and titles tabs."
        }
        "alerts" => alerts::HELP,
        "claude-status" => "Usage: milo claude-status\n\nRead usage from Claude Code's status line input.",
        _ => return None,
    })
}

/// Every word `command_help` knows, in the order `--help` lists them.
const COMMANDS: [&str; 15] = [
    "outline", "history", "usage", "menubar", "setup", "web", "preview", "settings", "doctor", "presence", "view",
    "review", "listen", "alerts", "claude-status",
];
const OPTIONS: [&str; 3] = ["--help", "--version", "--skip-onboarding"];

/// How many slips turn `typed` into `word`: a letter added, dropped,
/// changed, or swapped with the one beside it.
fn slips(typed: &str, word: &str) -> usize {
    let (typed, word): (Vec<char>, Vec<char>) = (typed.chars().collect(), word.chars().collect());
    // Three rows of the usual table: the one being filled and the two above it.
    let (mut before, mut above) = (vec![0; word.len() + 1], (0..=word.len()).collect::<Vec<_>>());
    let mut row = before.clone();
    for i in 1..=typed.len() {
        row[0] = i;
        for j in 1..=word.len() {
            let same = typed[i - 1] == word[j - 1];
            row[j] = (above[j] + 1).min(row[j - 1] + 1).min(above[j - 1] + usize::from(!same));
            if i > 1 && j > 1 && typed[i - 1] == word[j - 2] && typed[i - 2] == word[j - 1] {
                row[j] = row[j].min(before[j - 2] + 1);
            }
        }
        std::mem::swap(&mut before, &mut above);
        std::mem::swap(&mut above, &mut row);
    }
    above[word.len()]
}

/// The one word of `known` that `typed` is a slip or two from, if exactly
/// one is nearest. A word of four letters or fewer is allowed one slip.
fn meant<'a>(typed: &str, known: &[&'a str]) -> Option<&'a str> {
    let allowed = if typed.chars().count() <= 4 { 1 } else { 2 };
    let mut near = known.iter().map(|word| (slips(typed, word), *word)).filter(|(slips, _)| *slips <= allowed);
    let best = near.clone().map(|(slips, _)| slips).min()?;
    let word = near.find(|(slips, _)| *slips == best)?.1;
    near.all(|(slips, _)| slips != best).then_some(word)
}

const SEE_COMMANDS: &str = "see milo --help for commands";

/// Where to go from a word that reads as a command and is not one, or
/// nothing for a word that does not.
fn instead_of(word: &OsStr) -> Option<String> {
    match word.to_str()? {
        "help" => Some("for the guide: milo --help".to_string()),
        "version" => Some("for the version: milo --version".to_string()),
        typed => {
            meant(typed, &COMMANDS).map(|command| format!("did you mean milo {command}? Otherwise {SEE_COMMANDS}"))
        }
    }
}

/// The guide to name in an error about `name`: the command's own short one where it has one.
fn guide(name: Option<&str>) -> String {
    match name.filter(|name| command_help(name).is_some()) {
        Some(command) => format!("milo {command} --help"),
        None => "milo --help".to_string(),
    }
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
    Repo::open_to_read(dir).ok_or_else(|| {
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
    Review { patch: &'a OsStr, from: Option<&'a str>, float: bool },
    Edit(&'a OsStr),
    Path(&'a OsStr),
    Usage,
    UsageMenuStream,
    MenuBar(menubar::Action),
    Home,
    Setup,
    Web(web::Start<'a>),
    Preview { url: &'a str, placement: Option<preferences::PreviewPlacement> },
    PreviewAnchor { url: &'a str, anchor: preview::Anchor<'a> },
    SkipOnboarding,
    Doctor,
    Repair,
    Remove,
    Listen(bool),
    Alerts(Option<&'a OsStr>, &'a [OsString]),
    Presence(presence::Request),
    Settings(setup::transfer::Request<'a>),
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
    if !rest.is_empty()
        && !matches!(name, Some("view" | "review" | "web" | "preview" | "preview-anchor" | "alerts" | "presence" | "settings"))
    {
        return Err(format!("too many arguments; see {}", guide(name)));
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
        (Some("review"), Some(patch)) => {
            let (mut from, mut float) = (None, false);
            let mut flags = rest.iter();
            while let Some(flag) = flags.next() {
                match flag.to_str() {
                    Some("--float") if !float => float = true,
                    Some("--from") if from.is_none() => {
                        let name = flags.next().and_then(|name| name.to_str()).and_then(review::name);
                        from = Some(name.ok_or("--from requires a name of at most 40 characters")?);
                    }
                    _ => return Err("expected milo review PATCH [--from NAME] [--float]".to_string()),
                }
            }
            Command::Review { patch, from, float }
        }
        (Some("review"), None) => {
            return Err("missing patch; use milo review PATCH [--from NAME] [--float]".to_string());
        }
        (Some("setup"), None) => Command::Setup,
        (Some("preview"), _) => {
            let url = preview::url(option.ok_or(preview::EXPECTED)?)?;
            let placement = match rest {
                [] => None,
                [flag, value] if flag == "--placement" => {
                    Some(value.to_str().and_then(preferences::PreviewPlacement::parse).ok_or(preview::EXPECTED)?)
                }
                _ => return Err(preview::EXPECTED.into()),
            };
            Command::Preview { url, placement }
        }
        (Some("preview-anchor"), _) => {
            let [main, window, owner, owner_pid, native] = rest else {
                return Err("missing preview terminal, viewer or native window".into());
            };
            let url = preview::url(option.ok_or(preview::EXPECTED)?)?;
            let main = rex::exact_id(main, "block:").ok_or("invalid preview terminal")?;
            let window = rex::exact_id(window, "window:").ok_or("invalid preview tab")?;
            let owner = rex::exact_id(owner, "client:").ok_or("invalid preview viewer")?;
            let owner_pid = owner_pid
                .to_str()
                .and_then(|pid| pid.parse::<u32>().ok())
                .filter(|pid| (2..=2_147_483_647).contains(pid))
                .ok_or("invalid preview viewer PID")?;
            let native_window_id = native.to_str().and_then(|id| id.parse().ok()).ok_or("invalid native window ID")?;
            Command::PreviewAnchor { url, anchor: preview::Anchor { main, window, owner, owner_pid, native_window_id } }
        }
        (Some("web"), first_flag) => {
            const EXPECTED: &str = "expected milo web [--host HOST [--project DIR]] [--port PORT] [--no-open]";
            let mut open_browser = true;
            let (mut port, mut forwarded_port, mut host, mut project) = (None, None, None, None);
            let mut flags = first_flag.into_iter().chain(rest.iter().map(OsString::as_os_str));
            while let Some(flag) = flags.next() {
                let mut number = |least: u16, why: &'static str| {
                    flags
                        .next()
                        .and_then(OsStr::to_str)
                        .and_then(|s| s.parse::<u16>().ok())
                        .filter(|n| *n >= least)
                        .ok_or(why)
                };
                match flag.to_str() {
                    Some("--no-open") if open_browser => open_browser = false,
                    Some("--port") if port.is_none() => {
                        port = Some(number(0, "--port requires a number from 0 to 65535")?)
                    }
                    Some("--forwarded-port") if forwarded_port.is_none() => {
                        forwarded_port = Some(number(1, "--forwarded-port requires a number from 1 to 65535")?);
                    }
                    Some("--host") if host.is_none() => {
                        // A label or an address, never something Rex would read as an option.
                        let named =
                            flags.next().filter(|name| !name.is_empty() && !name.as_encoded_bytes().starts_with(b"-"));
                        host = Some(named.ok_or("--host requires a host label from rex hosts, or a server address")?);
                    }
                    Some("--project") if project.is_none() => {
                        project = Some(
                            flags
                                .next()
                                .filter(|dir| !dir.is_empty())
                                .ok_or("--project requires a folder on the host")?,
                        );
                    }
                    _ => return Err(EXPECTED.into()),
                }
            }
            Command::Web(match (host, forwarded_port) {
                (Some(host), None) => web::Start::Host { host, project, open_browser, port: port.unwrap_or(0) },
                (None, Some(forwarded_port)) if project.is_none() && open_browser => {
                    web::Start::Forwarded { port: port.unwrap_or(0), forwarded_port }
                }
                (None, None) if project.is_none() => {
                    web::Start::Local { open_browser, port: port.unwrap_or(web::DEFAULT_PORT) }
                }
                _ => return Err(EXPECTED.into()),
            })
        }
        (Some("--skip-onboarding"), None) => Command::SkipOnboarding,
        (Some("setup"), Some(flag)) if flag == "--skip-onboarding" => Command::SkipOnboarding,
        (Some("doctor"), None) => Command::Doctor,
        (Some("setup"), Some(flag)) if flag == "--repair" => Command::Repair,
        (Some("setup"), Some(flag)) if flag == "--remove" => Command::Remove,
        (Some("usage"), None) => Command::Usage,
        (Some("usage"), Some(flag)) if flag == "--menu-stream" => Command::UsageMenuStream,
        (Some("presence"), action) => Command::Presence(presence::Request::parse(action, rest)?),
        (Some("settings"), action) => Command::Settings(setup::transfer::Request::parse(action, rest)?),
        (Some("listen"), None) => Command::Listen(false),
        (Some("listen"), Some(flag)) if flag == "--stop" => Command::Listen(true),
        (Some("listen"), _) => return Err("expected milo listen [--stop]".to_string()),
        (Some("alerts"), action) => Command::Alerts(action, rest),
        (Some("claude-status"), None) => Command::ClaudeStatus,
        (Some("setup" | "usage" | "claude-status" | "doctor"), Some(_)) => {
            return Err(format!("unexpected argument; see {}", guide(name)));
        }
        (Some("-h" | "--help"), None) => Command::Help(USAGE),
        (Some("-V" | "--version"), None) => Command::Version,
        (Some("-h" | "--help"), Some(word)) => match word.to_str().and_then(command_help) {
            Some(help) => Command::Help(help),
            None => {
                let instead = instead_of(word).unwrap_or_else(|| SEE_COMMANDS.to_string());
                return Err(format!("{} is not a command; {instead}", word.display()));
            }
        },
        (Some("-v"), _) => return Err("no option -v; for the version: milo --version".to_string()),
        (Some(option), _) if option.starts_with('-') => {
            return Err(match meant(option, &OPTIONS) {
                Some(known) => format!("no option {option}; did you mean {known}? See milo --help"),
                None => format!("no option {option}; see milo --help"),
            });
        }
        (_, None) => match first {
            Some(path) => Command::Path(path),
            None => Command::Home,
        },
        _ => {
            let word = first.unwrap_or_default();
            let known = second.and_then(OsStr::to_str).filter(|name| command_help(name).is_some());
            return Err(match (word.to_str(), known, instead_of(word)) {
                (Some("help"), Some(command), _) => {
                    format!("help is not a command; for that guide: milo {command} --help")
                }
                (Some(typed), _, Some(instead)) => format!("{typed} is not a command; {instead}"),
                _ => "expected a command or path; see milo --help".to_string(),
            });
        }
    })
}

/// The runner opens panes after installing signal handlers; tests draw without a terminal.
fn dispatch(command: Command<'_>, run: impl FnOnce(Start<'_>, &Open<'_>) -> Result<(), String>) -> Result<(), String> {
    match command {
        Command::MenuBar(action) => print_report(menubar::run(action)),
        Command::Outline(path) => run(Start::Reading(project_path(path)), &|| open_outline(path)),
        Command::Path(path) => run(Start::Reading(Path::new(path)), &|| {
            open_outline(Some(path)).map_err(|why| match instead_of(path) {
                Some(instead) => format!("{why}; {instead}"),
                None => format!("{why}; {SEE_COMMANDS}"),
            })
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
                    Box::new(Outline::with_preferences(repo, rex::inside(), prefs).into_peek().refreshing_away())
                }))
            });
            setup::return_from_peek(origin);
            result
        }
        Command::Review { patch, from, float: true } => {
            print_report(review::float(Path::new("."), Path::new(patch), from))
        }
        Command::Review { patch, from, float: false } => run(Start::Reading(Path::new(".")), &|| {
            Ok(Box::new(review::Review::open(Path::new("."), Path::new(patch), from)?))
        }),
        Command::View { file, commit } => run(Start::Reading(file_dir(Path::new(file))), &|| {
            let path = Path::new(file);
            let reader = if let Some(id) = commit {
                Reader::at_commit(path, id)?
            } else {
                Reader::open(path).ok_or_else(|| format!("{} is not in a directory", path.display()))?
            };
            Ok(Box::new(Tabs::new(reader)))
        }),
        Command::Edit(file) => run(Start::Reading(file_dir(Path::new(file))), &|| {
            let path = Path::new(file);
            let mut reader = Reader::open(path).ok_or_else(|| format!("{} is not in a directory", path.display()))?;
            // A file that cannot be edited opens for reading and says why.
            reader.edit(std::time::Instant::now());
            Ok(Box::new(Tabs::new(reader)))
        }),
        Command::Usage => run(Start::Check, &|| Ok(Box::new(usage::Usage::open()))),
        Command::UsageMenuStream => usage::menu_stream().map_err(|e| format!("could not stream usage: {e}")),
        Command::Web(start) => web::run(start),
        Command::Preview { url, placement } => preview::open(url, placement),
        Command::PreviewAnchor { url, anchor } => preview::anchor(url, anchor),
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
        Command::Alerts(action, rest) => alerts::command(action, rest),
        Command::Presence(request) => print_report(presence::command(request)),
        Command::Settings(request) => print_report(setup::transfer::command(request)),
        Command::Listen(stop) => {
            if !stop {
                // Rex runs this as it starts, which is when presence should come up too.
                presence::start_with_rex();
            }
            exit(setup::listen(stop))
        }
        Command::ClaudeStatus => exit(usage::claude_status()),
        Command::Help(text) => match term::output_columns() {
            Some(columns) => print_line(&wrapped(text.trim_end(), columns.clamp(GUIDE_LEAST, GUIDE_MOST))),
            // A pipe or a file gets the lines as they are written, whatever reads them.
            None => print_line(text.trim_end()),
        },
        Command::Version => print_line(concat!("milo ", env!("CARGO_PKG_VERSION"))),
    }
}

/// The widest a guide's prose is left for a person to read, and the narrowest it is broken to.
const GUIDE_MOST: usize = 80;
const GUIDE_LEAST: usize = 40;

/// A guide with its prose broken at spaces to fit `columns`. A usage line
/// and an indented row are a command or a table, and stay as written.
fn wrapped(text: &str, columns: usize) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / columns);
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.starts_with([' ', '\t']) || line.starts_with("Usage:") || width::str_width(line) <= columns {
            out.push_str(line);
            continue;
        }
        let mut used = 0;
        for word in line.split(' ') {
            let wide = width::str_width(word);
            if used > 0 && used + 1 + wide > columns {
                out.push('\n');
                used = 0;
            } else if used > 0 {
                out.push(' ');
                used += 1;
            }
            out.push_str(word);
            used += wide;
        }
    }
    out
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
    Box::new(Outline::with_preferences(repo, rex::inside(), prefs).refreshing_away())
}

fn history(repo: Repo, prefs: &preferences::Preferences) -> Box<dyn Pane> {
    Box::new(
        History::new(repo, rex::inside())
            .refreshing_away()
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
    if first.as_deref() == Some(OsStr::new(presence::HELPER_COMMAND)) {
        exit(presence::helper(&args.collect::<Vec<_>>()));
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
    fn the_review_command_takes_a_patch_then_a_name_and_float_in_either_order() {
        let patch = OsStr::new("--odd.patch");
        let review = |rest: &[&str]| {
            let rest: Vec<OsString> = rest.iter().map(Into::into).collect();
            command(Some(OsStr::new("review")), Some(patch), &rest).map(|command| format!("{command:?}"))
        };
        let both = format!("{:?}", Command::Review { patch, from: Some("omp"), float: true });
        assert_eq!(review(&["--from", "omp", "--float"]), Ok(both.clone()));
        assert_eq!(review(&["--float", "--from", "omp"]), Ok(both));
        assert_eq!(review(&[]), Ok(format!("{:?}", Command::Review { patch, from: None, float: false })));
        // The word after --from is a name, whatever it looks like.
        let named_float = format!("{:?}", Command::Review { patch, from: Some("--float"), float: false });
        assert_eq!(review(&["--from", "--float"]), Ok(named_float));
        for bad in [&["--from"][..], &["--from", "a\u{1b}[2J"], &["--float", "--float"], &["extra"]] {
            assert!(review(bad).is_err(), "{bad:?}");
        }
        assert!(command(Some(OsStr::new("review")), None, &[]).is_err_and(|why| why.starts_with("missing patch")));
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
            ("web", None, Command::Web(web::Start::Local { open_browser: true, port: web::DEFAULT_PORT })),
            (
                "web",
                Some("--no-open"),
                Command::Web(web::Start::Local { open_browser: false, port: web::DEFAULT_PORT }),
            ),
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
    fn web_names_another_host_or_serves_a_forward_but_never_both() {
        let parse = |flags: &[&str]| {
            let flags: Vec<OsString> = flags.iter().map(OsString::from).collect();
            command(Some(OsStr::new("web")), flags.first().map(OsString::as_os_str), flags.get(1..).unwrap_or(&[]))
                .map(|command| format!("{command:?}"))
        };
        let host = web::Start::Host {
            host: OsStr::new("studio"),
            project: Some(OsStr::new("/srv/app")),
            open_browser: false,
            port: 0,
        };
        assert_eq!(
            parse(&["--host", "studio", "--project", "/srv/app", "--no-open"]),
            Ok(format!("{:?}", Command::Web(host)))
        );
        let forwarded = web::Start::Forwarded { port: 0, forwarded_port: 7001 };
        assert_eq!(parse(&["--forwarded-port", "7001"]), Ok(format!("{:?}", Command::Web(forwarded))));
        for refused in [
            &["--host", "-S"][..],
            &["--host", ""],
            &["--host"],
            &["--host", "studio", "--forwarded-port", "7001"],
            &["--forwarded-port", "0"],
            &["--forwarded-port", "7001", "--no-open"],
            &["--project", "/srv/app"],
        ] {
            assert!(parse(refused).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn preview_distinguishes_the_saved_default_from_a_one_shot_placement() {
        let first = Some(OsStr::new("preview"));
        let url = "http://localhost:3000";
        let second = Some(OsStr::new(url));
        assert_eq!(command(first, second, &[]), Ok(Command::Preview { url, placement: None }));
        for name in ["left", "right", "above", "below"] {
            let args = [OsString::from("--placement"), OsString::from(name)];
            let placement = preferences::PreviewPlacement::parse(name);
            assert!(placement.is_some());
            assert_eq!(command(first, second, &args), Ok(Command::Preview { url, placement }));
        }
        for rest in [
            &["--placement"][..],
            &["--placement", "auto"],
            &["--placement", "floating"],
            &["--placement", "left", "extra"],
        ] {
            let args: Vec<_> = rest.iter().map(|&arg| OsString::from(arg)).collect();
            assert_eq!(command(first, second, &args).unwrap_err(), preview::EXPECTED);
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

    fn refused(words: &[&str]) -> String {
        let words: Vec<OsString> = words.iter().map(OsString::from).collect();
        let (first, second) = (words.first().map(OsString::as_os_str), words.get(1).map(OsString::as_os_str));
        match command(first, second, words.get(2..).unwrap_or(&[])) {
            Ok(command) => open_pane(command).err().expect("the words opened a pane"),
            Err(why) => why,
        }
    }

    #[test]
    fn a_mistyped_command_is_answered_with_the_command_it_is_nearest() {
        const OTHERWISE: &str = "Otherwise see milo --help for commands";
        for (typed, command) in [
            ("histroy", "history"),
            ("outlin", "outline"),
            ("veiw", "view"),
            ("revew", "review"),
            ("docter", "doctor"),
            ("claude_status", "claude-status"),
        ] {
            let expected = format!("{typed} is not a directory; did you mean milo {command}? {OTHERWISE}");
            assert_eq!(refused(&[typed]), expected);
            // With an argument it cannot be a path, so it is not called one.
            let with_argument = format!("{typed} is not a command; did you mean milo {command}? {OTHERWISE}");
            assert_eq!(refused(&[typed, "."]), with_argument);
        }
        // A word far from every command is given no guess, and a short word is allowed one slip.
        for word in ["src-main", "sett", "branch", "x"] {
            assert_eq!(refused(&[word]), format!("{word} is not a directory; see milo --help for commands"));
            assert_eq!(refused(&[word, "."]), "expected a command or path; see milo --help");
        }
        // Nor is a word as near to two.
        assert_eq!(meant("lisen", &["listen", "linen"]), None);
        assert_eq!(meant("lisen", &["listen", "liner"]), Some("listen"));
    }

    #[test]
    fn every_spelling_of_asking_for_help_says_where_the_guide_is() {
        assert_eq!(refused(&["help"]), "help is not a directory; for the guide: milo --help");
        assert_eq!(refused(&["help", "view"]), "help is not a command; for that guide: milo view --help");
        assert_eq!(refused(&["help", "veiw"]), "help is not a command; for the guide: milo --help");
        assert_eq!(refused(&["version"]), "version is not a directory; for the version: milo --version");
        assert_eq!(refused(&["-v"]), "no option -v; for the version: milo --version");
        assert_eq!(refused(&["--hlep"]), "no option --hlep; did you mean --help? See milo --help");
        assert_eq!(refused(&["--verison"]), "no option --verison; did you mean --version? See milo --help");
        assert_eq!(refused(&["--json"]), "no option --json; see milo --help");
        for flag in ["-h", "--help"] {
            let asked = [OsStr::new(flag), OsStr::new("review")];
            assert_eq!(
                command(Some(asked[0]), Some(asked[1]), &[]),
                Ok(Command::Help(command_help("review").unwrap()))
            );
            assert_eq!(
                refused(&[flag, "reveiw"]),
                "reveiw is not a command; did you mean milo review? Otherwise see milo --help for commands"
            );
            assert_eq!(refused(&[flag, "nothing"]), "nothing is not a command; see milo --help for commands");
        }
    }

    #[test]
    fn an_error_about_a_command_names_that_commands_own_guide() {
        assert_eq!(refused(&["outline", "a", "b"]), "too many arguments; see milo outline --help");
        assert_eq!(refused(&["usage", "--json"]), "unexpected argument; see milo usage --help");
        assert_eq!(refused(&["doctor", "--fix"]), "unexpected argument; see milo doctor --help");
        assert_eq!(refused(&["some", "path", "more"]), "too many arguments; see milo --help");
        // Every command the guesses can name has a guide of its own to be sent to.
        assert!(COMMANDS.iter().all(|name| command_help(name).is_some()));
        let listed = USAGE.lines().skip(2).take_while(|line| !line.is_empty());
        for line in listed.filter_map(|line| line.trim_start_matches("Usage:").trim_start().strip_prefix("milo ")) {
            let word = line.split(' ').next().unwrap();
            let listed = COMMANDS.contains(&word) || word.starts_with(['-', '[']) || word == "PATH";
            assert!(listed, "{word} is in --help and not among the commands a guess can name");
        }
    }

    #[test]
    fn a_guide_shown_in_a_terminal_breaks_prose_between_words_and_leaves_commands_whole() {
        let guide = "Usage: milo web --host HOST [--project DIR] [--port PORT]\n\n\
                     Edit local settings in your browser. The default port is 6110.\n  \
                     Ctrl-S        save the draft into the file it was read from\n\
                     “Quoted” words are measured in columns, not bytes.";
        let lines: Vec<String> = wrapped(guide, 40).lines().map(str::to_owned).collect();
        assert_eq!(
            lines,
            [
                "Usage: milo web --host HOST [--project DIR] [--port PORT]",
                "",
                "Edit local settings in your browser. The",
                "default port is 6110.",
                "  Ctrl-S        save the draft into the file it was read from",
                "“Quoted” words are measured in columns,",
                "not bytes.",
            ]
        );
        // Nothing a guide says is dropped or reordered, at any width a terminal is given.
        let every = std::iter::once(USAGE).chain(COMMANDS.iter().map(|name| command_help(name).unwrap()));
        for (text, columns) in every.flat_map(|text| [GUIDE_LEAST, 57, GUIDE_MOST].map(|columns| (text, columns))) {
            let shown = wrapped(text, columns);
            assert_eq!(shown.split_whitespace().collect::<Vec<_>>(), text.split_whitespace().collect::<Vec<_>>());
            for line in shown.lines().filter(|line| !line.starts_with(' ') && !line.starts_with("Usage:")) {
                let one_word = !line.contains(' ');
                assert!(width::str_width(line) <= columns || one_word, "{columns}: {line}");
            }
        }
        // The full guide is written to 80 columns, so a pipe gets it at that width too.
        assert_eq!(USAGE.lines().find(|line| width::str_width(line) > GUIDE_MOST), None);
    }
}
