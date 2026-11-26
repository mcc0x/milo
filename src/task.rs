//! Starts a coding agent on a task: a Git worktree for it and a Rex session showing the agent.
//!
//! `milo task new` and the Rex action **Milo: New Agent Task…** both end in
//! `launch`, so there is one order of work and one set of refusals. Everything
//! that can be refused is checked first, and nothing is created until it has
//! been: the project, the agent's program, the worktree's branch and folder,
//! and the Rex server. After that a worktree is added if a new one was asked
//! for, then the session. Nothing made is ever removed again, and a failure
//! names what exists.
//!
//! The launcher creates a session and shows none: it selects, focuses and
//! renames nothing, and the person's pane stays where it was. It never
//! pushes, opens a pull request, merges or cleans up.

mod harness;

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::git::{self, worktrees};
use crate::{rex, titles};
use harness::{Harness, SearchPath};

pub(crate) const HELP: &str = "\
Usage: milo task new --agent AGENT --branch NAME [--base REF] [--path DIR] [--prompt TEXT]
       milo task new --agent AGENT --worktree DIR [--prompt TEXT]
       either form: [--project DIR] [--host HOST]

Start a coding agent on a task, in a Git worktree and a Rex session of its own. The session opens in the background with the agent's full interactive screen: your pane keeps the focus, and you visit the session when you like.

AGENT is claude (Claude Code), codex (Codex), omp (oh-my-pi) or opencode (OpenCode). It must be installed; Milo never starts a different agent in its place.

--branch NAME adds a worktree on that branch. A new branch needs --base REF, the commit it starts from; a branch that exists and is checked out nowhere takes no --base. The worktree goes in --path DIR, or beside the project in PROJECT-worktrees/NAME. The checkout you run this from is never switched.
--worktree DIR uses a worktree the project already has, as it is.
--prompt TEXT is the agent's first message, passed as written; no shell reads it. Without it the agent opens idle.
--project DIR names the project; the current folder otherwise.
--host HOST starts the task on another Rex host: a label from `rex hosts` or a server address, as for `rex -S`. --project, --worktree and --path are then folders on that host, written in full, and the host needs this version of milo on the search path of its login shell.

On success two lines are printed, `worktree DIR` and `session ID`, with `host HOST` after them for another host. Everything is checked before anything is created. If a step fails after that, the message says what exists; Milo removes nothing, and never pushes, merges or cleans up.

In Rex, the action Milo: New Agent Task… asks for the same choices in the command palette and starts the task the same way.";

const EXPECTED: &str = "see milo task --help";

/// Where the task's agent works.
#[derive(Debug, PartialEq, Eq)]
enum Place<'a> {
    /// A worktree the project already has.
    Worktree(&'a OsStr),
    /// A worktree to add for this branch.
    Branch { name: &'a str, base: Option<&'a str>, path: Option<&'a OsStr> },
}

/// One task to start, as the command line or the Rex action named it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request<'a> {
    agent: &'a str,
    place: Place<'a>,
    prompt: Option<&'a str>,
    project: Option<&'a OsStr>,
    host: Option<&'a OsStr>,
}

impl<'a> Request<'a> {
    pub(crate) fn parse(action: Option<&'a OsStr>, rest: &'a [OsString]) -> Result<Self, String> {
        if action != Some(OsStr::new("new")) {
            return Err(format!("expected milo task new; {EXPECTED}"));
        }
        let (mut agent, mut prompt, mut branch, mut base) = (None, None, None, None);
        let (mut worktree, mut path, mut project, mut host) = (None, None, None, None);
        let mut words = rest.iter().map(OsString::as_os_str);
        while let Some(flag) = words.next() {
            let name = flag.to_str().unwrap_or_default();
            let missing = || format!("{name} needs a value; {EXPECTED}");
            let given = words.next().ok_or_else(missing);
            let value = given.clone().and_then(|value| if value.is_empty() { Err(missing()) } else { Ok(value) });
            let text =
                |value: Result<&'a OsStr, String>| value?.to_str().ok_or_else(|| format!("{name} must be UTF-8 text"));
            match name {
                "--agent" => once(&mut agent, name, text(value))?,
                // An empty prompt is no prompt, as when a script passes a variable that is empty.
                "--prompt" => once(&mut prompt, name, text(given))?,
                "--branch" => once(&mut branch, name, text(value))?,
                "--base" => once(&mut base, name, text(value))?,
                "--worktree" => once(&mut worktree, name, value)?,
                "--path" => once(&mut path, name, value)?,
                "--project" => once(&mut project, name, value)?,
                "--host" => once(&mut host, name, value)?,
                _ => return Err(format!("unexpected argument {}; {EXPECTED}", flag.display())),
            }
        }
        let agent = agent.ok_or_else(|| format!("--agent is required: one of {}", harness::ids()))?;
        if Harness::named(agent).is_none() {
            return Err(format!("{agent} is not an agent milo can start; choose one of {}", harness::ids()));
        }
        // A host is a label or an address, never something Rex would read as an option.
        if host.is_some_and(|host| host.as_encoded_bytes().starts_with(b"-")) {
            return Err("--host requires a host label from rex hosts, or a server address".into());
        }
        let place = match (worktree, branch) {
            (Some(dir), None) if base.is_none() && path.is_none() => Place::Worktree(dir),
            (Some(_), None) => return Err("--base and --path belong to --branch; --worktree is used as it is".into()),
            (None, Some(name)) => Place::Branch { name, base, path },
            (Some(_), Some(_)) => return Err("choose --worktree DIR or --branch NAME, not both".into()),
            (None, None) => {
                return Err(format!("say where the agent works: --worktree DIR or --branch NAME; {EXPECTED}"));
            }
        };
        Ok(Self { agent, place, prompt: prompt.filter(|text| !text.is_empty()), project, host })
    }
}

/// Takes an option's value the first time the option is given.
fn once<T>(slot: &mut Option<T>, name: &str, value: Result<T, String>) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{name} was given twice"));
    }
    *slot = Some(value?);
    Ok(())
}

/// A worktree that will exist once the plan is carried out, and what carrying it out adds.
#[derive(Debug)]
enum Plan {
    Existing(PathBuf),
    New { path: PathBuf, branch: String, commit: Option<String> },
}

impl Plan {
    fn path(&self) -> &Path {
        match self {
            Self::Existing(path) | Self::New { path, .. } => path,
        }
    }

    /// What exists because of this plan, for a message about a later failure.
    fn made(&self) -> String {
        match self {
            Self::Existing(path) => format!("The worktree {} is as it was.", path.display()),
            Self::New { path, branch, commit: Some(_) } => {
                format!("Created worktree {} on new branch {branch}; it was not removed.", path.display())
            }
            Self::New { path, branch, commit: None } => {
                format!("Created worktree {} on branch {branch}; it was not removed.", path.display())
            }
        }
    }
}

/// A folder as the person wrote it, in full. It need not exist yet.
fn full(dir: &OsStr) -> Result<PathBuf, String> {
    let path = std::path::absolute(dir).map_err(|_| format!("{} is not a folder name", dir.display()))?;
    // A result is one line for each fact, so a name that cannot be a line is refused.
    if !path.to_str().is_some_and(|text| !text.chars().any(char::is_control)) {
        return Err("a folder name with control characters or outside UTF-8 cannot be used for a task".into());
    }
    Ok(path)
}

/// Decides where the agent will work, changing nothing.
fn plan(root: &Path, place: &Place<'_>) -> Result<Plan, String> {
    const NO_GIT: &str = "Git could not read this project";
    let listed = worktrees::list(root).ok_or(NO_GIT)?;
    match place {
        Place::Worktree(dir) => {
            let wanted = full(dir)?;
            let found = listed.into_iter().find(|tree| git::same_folder(&tree.path, &wanted));
            let found = found.ok_or_else(|| {
                format!("{} is not a worktree of this project; git worktree list shows them", wanted.display())
            })?;
            full(found.path.as_os_str()).map(Plan::Existing)
        }
        Place::Branch { name, base, path } => {
            if !worktrees::branch_name(root, name) {
                return Err(format!("{name} is not a branch name Git accepts"));
            }
            if let Some(used) = listed.iter().find(|tree| tree.branch.as_deref() == Some(name)) {
                let at = used.path.display();
                return Err(format!("{name} is already checked out at {at}; to work there: --worktree {at}"));
            }
            let exists = worktrees::has_branch(root, name).ok_or(NO_GIT)?;
            let commit = match (exists, base) {
                (true, None) => None,
                (true, Some(_)) => {
                    return Err(format!(
                        "branch {name} already exists; leave out --base to give it a worktree, or choose a new name"
                    ));
                }
                (false, None) => {
                    return Err(format!("new branch {name} needs --base REF, the commit it starts from"));
                }
                (false, Some(base)) => Some(
                    worktrees::commit(root, base)
                        .ok_or(NO_GIT)?
                        .ok_or_else(|| format!("{base} names no commit in this project"))?,
                ),
            };
            let path = match path {
                Some(dir) => full(dir)?,
                None => {
                    let beside = worktrees::default_path(root, name).ok_or("no folder for the worktree; name one")?;
                    full(beside.as_os_str())?
                }
            };
            if path.symlink_metadata().is_ok() {
                return Err(format!("{} already exists; choose another --path", path.display()));
            }
            Ok(Plan::New { path, branch: (*name).to_owned(), commit })
        }
    }
}

/// Starts the task and gives the lines to print, or says why not and what exists.
pub(crate) fn command(request: &Request<'_>) -> Result<String, String> {
    if cfg!(windows) {
        return Err(
            "milo task is not supported on Windows yet; it needs Rex, which does not run natively there".to_owned()
        );
    }
    match request.host {
        Some(host) => on_host(host, request),
        None => launch(request, &SearchPath::new()),
    }
}

fn launch(request: &Request<'_>, search: &SearchPath) -> Result<String, String> {
    const UNTOUCHED: &str = "nothing was created";
    let harness = Harness::named(request.agent).ok_or("unknown agent")?;
    let args = harness.arguments(request.prompt)?;
    let dir = request.project.map_or(Path::new("."), Path::new);
    let root = git::discover_worktree(dir)
        .map_err(str::to_owned)?
        .ok_or_else(|| format!("{} is not in a Git project; {UNTOUCHED}", dir.display()))?;
    let program = search
        .find(harness)
        .ok_or_else(|| format!("{} ({}) is not installed on this computer; {UNTOUCHED}", harness.name, harness.id))?;
    let plan = plan(&root, &request.place)?;
    if !rex::task::reachable(None) {
        return Err(format!("no Rex server answered; {UNTOUCHED}"));
    }
    if let Plan::New { path, branch, commit } = &plan {
        let start = match commit {
            Some(commit) => worktrees::Start::NewBranch { branch, commit },
            None => worktrees::Start::Branch(branch),
        };
        worktrees::add(&root, path, &start).map_err(|why| format!("Git added no worktree: {why}"))?;
    }
    let label = titles::session_name(plan.path()).unwrap_or_default();
    let Some(started) = rex::task::start(&label, plan.path(), &program, &args) else {
        return Err(format!("Rex started no session. {}", plan.made()));
    };
    if let Some(status) = started.exited_at_once() {
        return Err(format!(
            "{} exited at once with status {status}; Rex session {} may still show why. {}",
            harness.name,
            started.session,
            plan.made()
        ));
    }
    Ok(format!("worktree {}\nsession {}", plan.path().display(), started.session))
}

/// Has the named host's own Milo start the task, since the project, the
/// worktree and the agent are all on that host.
fn on_host(host: &OsStr, request: &Request<'_>) -> Result<String, String> {
    let on_host = |dir: &OsStr| Path::new(dir).is_absolute();
    let project = request.project.ok_or("--host needs --project DIR, the project's folder on that host")?;
    let folders = match request.place {
        Place::Worktree(dir) => [Some(dir), None],
        Place::Branch { path, .. } => [path, None],
    };
    if !on_host(project) || !folders.into_iter().flatten().all(on_host) {
        return Err("with --host, write each folder in full, as it is on that host".into());
    }
    let mut args: Vec<OsString> = ["task", "new", "--agent", request.agent, "--project"].map(Into::into).into();
    args.push(project.into());
    let mut add = |flag: &str, value: &OsStr| args.extend([flag.into(), value.to_owned()]);
    match request.place {
        Place::Worktree(dir) => add("--worktree", dir),
        Place::Branch { name, base, path } => {
            add("--branch", OsStr::new(name));
            base.into_iter().for_each(|base| add("--base", OsStr::new(base)));
            path.into_iter().for_each(|path| add("--path", path));
        }
    }
    request.prompt.into_iter().for_each(|prompt| add("--prompt", OsStr::new(prompt)));
    let shown = host.display();
    let carried = rex::task::carry(host, &args).ok_or_else(|| {
        format!("could not run milo on {shown}; see rex hosts check, and check that milo is installed there")
    })?;
    carried_result(&carried.screen, carried.succeeded)
        .map(|lines| format!("{lines}\nhost {shown}"))
        .map_err(|why| format!("on {shown}: {why}"))
}

/// The two lines the host's launcher printed, or its reason for printing none.
/// The text comes from another computer, so only whole lines of plain text are kept.
fn carried_result(screen: &str, succeeded: bool) -> Result<String, String> {
    let plain = |line: &str| line.chars().filter(|c| !c.is_control()).collect::<String>();
    let line = |start: &str| screen.lines().rev().find(|line| line.starts_with(start)).map(plain);
    let session =
        line("session ").filter(|line| rex::exact_id(OsStr::new(&line["session ".len()..]), "session:").is_some());
    match (succeeded, line("worktree "), session) {
        (true, Some(worktree), Some(session)) => Ok(format!("{worktree}\n{session}")),
        _ => Err(line("milo: ").map_or_else(
            || "milo did not report a result; is this version of milo on the host's search path?".to_owned(),
            |why| why["milo: ".len()..].to_owned(),
        )),
    }
}

/// Internal, noninteractive helper for the Rex action: the agents installed
/// here and the worktrees of the project holding the shell's folder, a line
/// each. Names that can hold any byte are written as lowercase hex.
pub(crate) fn options(args: &[OsString]) -> i32 {
    if cfg!(windows) {
        eprintln!("milo task is not supported on Windows yet.");
        return 1;
    }
    let [cwd] = args else {
        eprintln!("Invalid task-options arguments.");
        return 1;
    };
    match options_text(Path::new(cwd), &SearchPath::new()) {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err(why) => {
            eprintln!("{why}");
            1
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut hex, byte| {
        write!(hex, "{byte:02x}").expect("writing to a string");
        hex
    })
}

fn options_text(cwd: &Path, search: &SearchPath) -> Result<String, &'static str> {
    const NO_PROJECT: &str = "This shell is not in a Git project.";
    let root = git::discover_worktree(cwd)?.ok_or(NO_PROJECT)?;
    let listed = worktrees::list(&root).ok_or(NO_PROJECT)?;
    let mut text = format!("project {}\n", hex(root.as_os_str().as_encoded_bytes()));
    for harness in harness::installed(search) {
        writeln!(text, "agent {} {}", harness.id, harness.name).expect("writing to a string");
    }
    for tree in &listed {
        let here = if git::same_folder(&tree.path, &root) { "here" } else { "-" };
        let branch = tree.branch.as_deref().map_or_else(|| "-".to_owned(), |branch| hex(branch.as_bytes()));
        writeln!(text, "worktree {} {branch} {here}", hex(tree.path.as_os_str().as_encoded_bytes()))
            .expect("writing to a string");
    }
    // A branch with no worktree can be given one as it is, with no commit to start from.
    for branch in worktrees::branches(&root).ok_or(NO_PROJECT)? {
        if !listed.iter().any(|tree| tree.branch.as_deref() == Some(&branch)) {
            writeln!(text, "branch {}", hex(branch.as_bytes())).expect("writing to a string");
        }
    }
    // Where a new branch could start: the branch of the shell's checkout, then the remote's main one.
    let here = listed.iter().find(|tree| git::same_folder(&tree.path, &root)).and_then(|tree| tree.branch.clone());
    for base in here.into_iter().chain(worktrees::remote_default(&root)) {
        writeln!(text, "base {}", hex(base.as_bytes())).expect("writing to a string");
    }
    Ok(text)
}

#[cfg(all(test, unix))]
mod tests;
