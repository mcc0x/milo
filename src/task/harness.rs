//! The coding agents a task can start, where each is installed, and how each takes its first prompt.
//!
//! A *harness* is a coding agent's own terminal program. Each is started the
//! way its documentation starts an interactive session with a first prompt.
//! Never use a print, headless or one-shot mode here: the person must find
//! the agent's full screen in the session and be able to answer it.

use std::cell::OnceCell;
use std::ffi::{CStr, OsString};
use std::io::Read as _;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::format;

/// How a harness is given the first prompt of an interactive session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prompt {
    /// As one word after these, the last of which ends the harness's options.
    After(&'static [&'static str]),
    /// As the value of `option`, joined to it with `=` so that it cannot be
    /// read as another option or as a command, after the words in `before`.
    Option { before: &'static [&'static str], option: &'static str },
    /// Not in its arguments at all: the harness documents no way to pass a
    /// first prompt and stay interactive. The person types it instead.
    Typed,
}

/// How far an entry was checked against the release named, and no further.
/// [`docs/evidence/harness-catalog`](../../docs/evidence/harness-catalog/README.md) has what was seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Checked {
    /// In a session Milo started, the harness showed the prompt as written,
    /// a reply to it from its own provider, and a reply to a typed follow-up.
    Replied(&'static str),
    /// The same, with the replies from a stand-in for a model on this
    /// computer: the harness needed no sign-in to be pointed at one.
    StandIn(&'static str),
    /// The release opened its interactive screen for exactly these arguments
    /// and stopped at its own sign-in or setup, so no reply was seen.
    Opened(&'static str),
}

/// One coding agent's terminal program: how to start it, and what else Milo
/// knows about it. Starting it and reading its usage are separate: `usage`
/// says what `milo usage` reads, and neither needs the other.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Harness {
    /// The word `--agent` takes.
    pub id: &'static str,
    /// The program's name, looked for on the search path.
    pub program: &'static str,
    /// The name people know the agent by.
    pub name: &'static str,
    /// The words that open an interactive session with nothing asked yet.
    idle: &'static [&'static str],
    prompt: Prompt,
    /// Whether the harness runs a prompt that is one bare word as its own
    /// command of that name, even after the end of its options.
    words_are_commands: bool,
    /// Whether the harness looks for its own options in a prompt that
    /// begins with a dash, even after the end of its options.
    dashes_are_options: bool,
    /// What the harness itself needs before it can reply. Milo supplies
    /// none of it and never reads what the harness saved for it.
    pub needs: &'static str,
    /// What `milo usage` reads of the figures this harness saved, if anything.
    pub usage: Option<&'static str>,
    pub checked: Checked,
}

impl Harness {
    /// An entry that opens idle with no arguments and takes its prompt after `--`.
    const fn new(id: &'static str, name: &'static str, needs: &'static str, checked: Checked) -> Self {
        Self {
            id,
            program: id,
            name,
            idle: &[],
            prompt: Prompt::After(&["--"]),
            words_are_commands: false,
            dashes_are_options: false,
            needs,
            usage: None,
            checked,
        }
    }
}

/// Every harness a task can start, in the order they are offered. Each entry
/// is the vendor's documented interactive invocation; the notes say what was
/// seen of the release in `checked`.
pub(crate) const HARNESSES: [Harness; 14] = [
    // `claude -- doctor` runs Claude Code's `doctor`.
    Harness {
        words_are_commands: true,
        usage: Some("saved limit windows and today's transcript tokens"),
        ..Harness::new("claude", "Claude Code", "a Claude sign-in or an Anthropic API key", Checked::Replied("2.1.311"))
    },
    Harness {
        usage: Some("limit windows recorded in recent sessions"),
        ..Harness::new("codex", "Codex", "a ChatGPT sign-in or an OpenAI API key", Checked::Replied("0.171.0"))
    },
    // Without `launch`, a first word such as `models` is routed to that command.
    Harness {
        prompt: Prompt::After(&["launch", "--"]),
        usage: Some("the Codex and Claude limit reports it saved, under those tools"),
        ..Harness::new("omp", "oh-my-pi", "a provider signed in to inside oh-my-pi", Checked::Replied("19.1.1"))
    },
    Harness {
        prompt: Prompt::Option { before: &[], option: "--prompt" },
        usage: Some("today's tokens, steps and estimated cost"),
        ..Harness::new("opencode", "OpenCode", "a model chosen inside OpenCode", Checked::Replied("1.19.6 and 2.4.1"))
    },
    // `pi -- list` opens Pi with the message `list`; without `--` it runs `pi list`.
    Harness::new("pi", "Pi", "a model provider set up inside Pi", Checked::StandIn("1.6.1")),
    // Amp documents a first prompt only as text piped to it, and its screen needs the terminal there.
    Harness {
        prompt: Prompt::Typed,
        ..Harness::new("amp", "Amp", "an Amp account", Checked::Opened("0.0.1795608053"))
    },
    Harness {
        prompt: Prompt::Option { before: &[], option: "--interactive" },
        ..Harness::new(
            "copilot",
            "GitHub Copilot CLI",
            "a GitHub account with Copilot, or a model provider set up for it",
            Checked::StandIn("1.0.115"),
        )
    },
    // Homebrew installs only `cursor-agent`; Cursor's own installer adds `agent` beside it.
    // The `agent` command takes the prompt, so a one-word prompt cannot name another command.
    Harness {
        program: "cursor-agent",
        prompt: Prompt::After(&["agent", "--"]),
        ..Harness::new("cursor", "Cursor CLI", "a Cursor account", Checked::Opened("2026.11.22"))
    },
    Harness {
        program: "agy",
        prompt: Prompt::Option { before: &[], option: "--prompt-interactive" },
        ..Harness::new("antigravity", "Antigravity CLI", "a Google account", Checked::Opened("1.5.17"))
    },
    // Without `--`, Kiro's parser refuses a prompt that begins with a dash.
    Harness {
        program: "kiro-cli",
        idle: &["chat"],
        prompt: Prompt::After(&["chat", "--"]),
        ..Harness::new("kiro", "Kiro CLI", "a Kiro sign-in", Checked::Opened("3.6.0"))
    },
    // Without `-i`, Cline runs the prompt once, approving its own tools, and exits.
    // `cline -i -- doctor` runs Cline's `doctor`. Its source also looks for a few of its
    // options in every argument, `--` or not, so a prompt that begins with a dash is refused.
    Harness {
        idle: &["-i"],
        prompt: Prompt::After(&["-i", "--"]),
        words_are_commands: true,
        dashes_are_options: true,
        ..Harness::new("cline", "Cline CLI", "a provider chosen in Cline's setup", Checked::StandIn("3.0.75"))
    },
    // `droid -- doctor` runs Droid's `doctor`.
    Harness {
        words_are_commands: true,
        ..Harness::new("droid", "Factory Droid", "a Factory account", Checked::Opened("1.20.0"))
    },
    // `goose run` without `--interactive` answers once and exits; `goose session` takes no prompt.
    Harness {
        idle: &["session"],
        prompt: Prompt::Option { before: &["run", "--interactive"], option: "--text" },
        ..Harness::new("goose", "Goose", "a provider set up with goose configure", Checked::StandIn("1.61.0"))
    },
    // A bare `qwen PROMPT` answers once and exits.
    Harness {
        prompt: Prompt::Option { before: &[], option: "--prompt-interactive" },
        ..Harness::new("qwen", "Qwen Code", "a provider connected inside Qwen Code", Checked::StandIn("0.28.4"))
    },
];

impl Harness {
    pub(crate) fn named(id: &str) -> Option<&'static Self> {
        HARNESSES.iter().find(|harness| harness.id == id)
    }

    /// Whether a first prompt can be passed when the harness starts.
    pub(crate) fn takes_prompt(&self) -> bool {
        self.prompt != Prompt::Typed
    }

    /// The words after the program's name that open an interactive session,
    /// with `prompt` as its first message when there is one. The prompt is
    /// always a whole word of its own: no shell and no option parser reads it.
    pub(crate) fn arguments(&self, prompt: Option<&str>) -> Result<Vec<OsString>, String> {
        let Some(prompt) = prompt else { return Ok(self.idle.iter().map(OsString::from).collect()) };
        let name = self.name;
        let bare_word = prompt.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if self.words_are_commands && bare_word {
            return Err(format!(
                "{name} would run the one-word prompt {prompt} as its own command; describe the task in a few words"
            ));
        }
        if self.dashes_are_options && prompt.starts_with('-') {
            return Err(format!(
                "{name} would look for its own options in a prompt that begins with a dash; begin the task with a word"
            ));
        }
        let words = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        Ok(match self.prompt {
            Prompt::After(before) => [words(before), vec![prompt.into()]].concat(),
            Prompt::Option { before, option } => [words(before), vec![format!("{option}={prompt}").into()]].concat(),
            Prompt::Typed => {
                return Err(format!(
                    "{name} takes no first prompt when it starts; leave out --prompt and type the task once {name} is open"
                ));
            }
        })
    }

    /// The command as a person would type it, with `PROMPT` where a first prompt goes.
    pub(crate) fn shown(&self) -> String {
        let (before, last) = match self.prompt {
            Prompt::After(before) => (before, "PROMPT".to_owned()),
            Prompt::Option { before, option } => (before, format!("{option}=PROMPT")),
            Prompt::Typed => return [&[self.program], self.idle].concat().join(" "),
        };
        [&[self.program], before, &[&last]].concat().join(" ")
    }
}

/// How long the login shell may take to say its search path.
const SHELL_WAIT: Duration = Duration::from_secs(8);
/// A search path is a line of folders, never this long.
const PATH_LIMIT: u64 = 1 << 16;

/// Where harnesses are looked for: this process's search path, then the one
/// the person's shell sets up. Rex starts actions with a short system path,
/// so a launch from the command palette finds an agent only on the second.
pub(crate) struct SearchPath {
    own: OsString,
    shell: OnceCell<Option<OsString>>,
}

impl SearchPath {
    pub(crate) fn new() -> Self {
        Self { own: std::env::var_os("PATH").unwrap_or_default(), shell: OnceCell::new() }
    }

    /// This process's search path alone, for a report that must start no shell.
    pub(crate) fn own() -> Self {
        Self { shell: OnceCell::from(None), ..Self::new() }
    }

    /// A search path for a test, which must not ask the shell of whoever runs it.
    #[cfg(test)]
    pub(crate) fn with(own: OsString, shell: Option<OsString>) -> Self {
        Self { own, shell: OnceCell::from(shell) }
    }

    /// The harness's program, by its fixed name in a folder named in full.
    /// The shell is asked at most once, and only when this process's own
    /// path does not have the program.
    pub(crate) fn find(&self, harness: &Harness) -> Option<PathBuf> {
        format::find(harness.program, &self.own)
            .or_else(|| format::find(harness.program, self.shell.get_or_init(shell_path).as_ref()?))
    }
}

/// The search path of the person's login shell with its interactive startup
/// files read, which is where installers of these agents add their folders.
/// The shell runs apart from this terminal, so it cannot take the terminal over.
fn shell_path() -> Option<OsString> {
    path_from(&login_shell()?, SHELL_WAIT)
}

fn path_from(shell: &Path, wait: Duration) -> Option<OsString> {
    let mut command = Command::new(shell);
    command.args(["-lic", "/usr/bin/printenv PATH"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let leave_session = || {
        // SAFETY: setsid takes no arguments and touches no memory of this process.
        unsafe { libc::setsid() };
        Ok(())
    };
    // SAFETY: The closure runs in the child between fork and exec and calls
    // only setsid, which is async-signal-safe and allocates nothing.
    unsafe { command.pre_exec(leave_session) };
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut said = Vec::new();
        let _ = (&mut stdout).take(PATH_LIMIT).read_to_end(&mut said);
        said
    });
    let deadline = Instant::now() + wait;
    let ended = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => break false,
        }
    };
    if !ended {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    // A startup file may print before the path does. The path is the last line.
    let said = reader.join().ok()?;
    let line = said.split(|&b| b == b'\n').rev().find(|line| !line.is_empty())?;
    Some(OsString::from_vec(line.to_vec()))
}

/// The shell named by `SHELL`, or else the one the account was given.
fn login_shell() -> Option<PathBuf> {
    let named = std::env::var_os("SHELL").map(PathBuf::from).filter(|shell| shell.is_absolute());
    named.or_else(account_shell).filter(|shell| shell.is_file())
}

fn account_shell() -> Option<PathBuf> {
    let mut buffer = vec![0; 4096];
    // SAFETY: A zeroed passwd is a valid value for getpwuid_r to fill in.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    // SAFETY: geteuid has no preconditions.
    let user = unsafe { libc::geteuid() };
    // SAFETY: The entry, the buffer and the result pointer are live for the
    // call, and the length given is the buffer's own.
    let status = unsafe { libc::getpwuid_r(user, &raw mut entry, buffer.as_mut_ptr(), buffer.len(), &raw mut found) };
    if status != 0 || found.is_null() || entry.pw_shell.is_null() {
        return None;
    }
    // SAFETY: On success pw_shell points at a terminated string inside `buffer`, which is still live.
    let shell = unsafe { CStr::from_ptr(entry.pw_shell) };
    let shell = PathBuf::from(OsString::from_vec(shell.to_bytes().to_vec()));
    shell.is_absolute().then_some(shell)
}

/// The programs found, for the harnesses a picker may offer.
pub(crate) fn installed(path: &SearchPath) -> Vec<&'static Harness> {
    HARNESSES.iter().filter(|harness| path.find(harness).is_some()).collect()
}

/// The words `--agent` takes, for a message that lists them.
pub(crate) fn ids() -> String {
    HARNESSES.map(|harness| harness.id).join(", ")
}

/// Runs the harness in `dir` in place of this process, so that it has this
/// terminal as the person's shell would give it. Returns only when the
/// program could not be started.
pub(crate) fn run_here(program: &Path, args: &[OsString], dir: &Path) -> std::io::Error {
    Command::new(program).args(args).current_dir(dir).exec()
}

impl Checked {
    /// The release and how far it was followed, in a few words.
    fn said(self) -> String {
        match self {
            Self::Replied(release) => format!("{release}: prompt, reply and a typed follow-up"),
            Self::StandIn(release) => format!("{release}: prompt, reply and a typed follow-up, from a stand-in model"),
            Self::Opened(release) => format!("{release}: opened, as far as its own sign-in or setup"),
        }
    }
}

/// What `milo task agents` prints: every harness, whether its program is on
/// this computer, and what the catalog says of it.
pub(crate) fn listing(path: &SearchPath) -> String {
    let mut text = String::new();
    for harness in &HARNESSES {
        let found = path.find(harness).map_or_else(
            || format!("{} is not on the search path", harness.program),
            |program| program.display().to_string(),
        );
        let starts = if harness.takes_prompt() {
            harness.shown()
        } else {
            format!("{}, and takes no first prompt: type it once {} is open", harness.shown(), harness.name)
        };
        let rows = [
            ("installed", found),
            ("starts as", starts),
            ("needs", harness.needs.to_owned()),
            ("milo usage", harness.usage.unwrap_or("reads nothing this agent saves").to_owned()),
            ("checked", harness.checked.said()),
        ];
        text.push_str(&format!("{}  {}\n", harness.id, harness.name));
        rows.iter().for_each(|(label, value)| text.push_str(&format!("  {label:<11} {value}\n")));
    }
    text.push_str(
        "\nmilo task new starts only the agent you name, as shown, and never in a print or one-shot mode.\n\
         Usage is read from what a tool saved, whether or not milo started it. milo reads no agent's sign-in.",
    );
    text
}

/// The line `milo doctor` gives agent tasks: which programs `path` has.
pub(crate) fn doctor(path: &SearchPath) -> String {
    let here: Vec<&str> = installed(path).iter().map(|harness| harness.id).collect();
    let known = HARNESSES.len();
    if here.is_empty() {
        return format!(
            "Agent tasks: none of the {known} agents is on this search path (optional). milo task agents lists them."
        );
    }
    format!(
        "Agent tasks: {} of {known} agents on this search path ({}). milo task agents lists them.",
        here.len(),
        here.join(", ")
    )
}

/// The catalog as the table in `docs/integrations.md`.
#[cfg(test)]
fn table() -> String {
    let mut table = String::from(
        "| `--agent` | Agent | Started as | Before it can reply | `milo usage` reads | Checked |\n| --- | --- | --- | --- | --- | --- |\n",
    );
    for harness in &HARNESSES {
        let starts = if harness.takes_prompt() {
            format!("`{}`", harness.shown())
        } else {
            format!("`{}`; no first prompt", harness.shown())
        };
        table.push_str(&format!(
            "| `{}` | {} | {starts} | {} | {} | {} |\n",
            harness.id,
            harness.name,
            harness.needs,
            harness.usage.unwrap_or("Nothing"),
            harness.checked.said(),
        ));
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(id: &str, prompt: Option<&str>) -> Vec<String> {
        let args = Harness::named(id).unwrap().arguments(prompt).unwrap();
        args.into_iter().map(|word| word.into_string().unwrap()).collect()
    }

    #[test]
    fn each_harness_gets_its_documented_interactive_invocation_with_the_prompt_as_one_word() {
        let prompt = "--print $(touch x); `id` 'q' \"q\"\nsecond line";
        let joined = |option: &str| format!("{option}={prompt}");
        for (id, idle, expected) in [
            ("claude", &[][..], vec!["--".to_owned(), prompt.to_owned()]),
            ("codex", &[], vec!["--".into(), prompt.into()]),
            ("omp", &[], vec!["launch".into(), "--".into(), prompt.into()]),
            ("opencode", &[], vec![joined("--prompt")]),
            ("pi", &[], vec!["--".into(), prompt.into()]),
            ("copilot", &[], vec![joined("--interactive")]),
            ("cursor", &[], vec!["agent".into(), "--".into(), prompt.into()]),
            ("antigravity", &[], vec![joined("--prompt-interactive")]),
            ("kiro", &["chat"], vec!["chat".into(), "--".into(), prompt.into()]),
            ("droid", &[], vec!["--".into(), prompt.into()]),
            ("goose", &["session"], vec!["run".into(), "--interactive".into(), joined("--text")]),
            ("qwen", &[], vec![joined("--prompt-interactive")]),
        ] {
            assert_eq!(words(id, Some(prompt)), expected, "{id}");
            assert_eq!(words(id, None), idle, "{id} opens idle");
        }
        assert_eq!(words("cline", Some("fix it")), ["-i", "--", "fix it"]);
        assert_eq!(words("cline", None), ["-i"], "without -i Cline runs once and exits");
        // No entry reaches a mode that answers once and exits, or that approves its own tools.
        let one_shot = ["-p", "--print", "--prompt", "exec", "-x", "--execute", "--no-interactive", "-y", "--yolo"];
        for harness in HARNESSES.iter().filter(|harness| harness.takes_prompt()) {
            let args = words(harness.id, Some("fix it"));
            assert!(!args.iter().any(|word| one_shot.contains(&word.as_str())), "{args:?}");
            assert_eq!(args.iter().filter(|word| word.contains("fix it")).count(), 1, "{args:?}");
            assert_eq!(args.contains(&"run".to_owned()), args.contains(&"--interactive".to_owned()), "{args:?}");
        }
    }

    #[test]
    fn a_prompt_a_harness_would_read_as_its_own_command_or_option_is_refused_and_never_changed() {
        for (id, name) in [("claude", "Claude Code"), ("cline", "Cline CLI"), ("droid", "Factory Droid")] {
            let harness = Harness::named(id).unwrap();
            for word in ["doctor", "update", "auto-mode"] {
                let why = harness.arguments(Some(word)).unwrap_err();
                assert!(why.contains(name) && why.contains(word), "{why}");
            }
            assert_eq!(words(id, Some("run doctor")).last().unwrap(), "run doctor");
        }
        assert_eq!(words("codex", Some("doctor")), ["--", "doctor"]);
        assert_eq!(words("omp", Some("models")), ["launch", "--", "models"]);
        assert_eq!(words("pi", Some("list")), ["--", "list"]);
        assert_eq!(words("cursor", Some("status")), ["agent", "--", "status"]);
        assert_eq!(words("goose", Some("configure")), ["run", "--interactive", "--text=configure"]);

        let dashed = Harness::named("cline").unwrap().arguments(Some("--thinking about it")).unwrap_err();
        assert!(dashed.contains("begins with a dash"), "{dashed}");
        assert_eq!(words("droid", Some("--print it")), ["--", "--print it"]);
    }

    #[test]
    fn a_harness_with_no_argument_for_a_first_prompt_opens_idle_or_is_refused_with_one() {
        let amp = Harness::named("amp").unwrap();
        assert!(!amp.takes_prompt() && words("amp", None).is_empty());
        let why = amp.arguments(Some("fix the tests")).unwrap_err();
        assert_eq!(
            why,
            "Amp takes no first prompt when it starts; leave out --prompt and type the task once Amp is open"
        );
        assert_eq!(amp.shown(), "amp");
        assert!(HARNESSES.iter().all(|harness| harness.takes_prompt() || harness.id == "amp"));
    }

    #[test]
    fn every_agent_has_one_word_and_one_program_and_nothing_else_is_known() {
        let ids =
            "claude, codex, omp, opencode, pi, amp, copilot, cursor, antigravity, kiro, cline, droid, goose, qwen";
        assert_eq!(super::ids(), ids);
        for (at, harness) in HARNESSES.iter().enumerate() {
            assert!(HARNESSES[..at].iter().all(|other| other.id != harness.id && other.program != harness.program));
        }
        let program = |id: &str| Harness::named(id).unwrap().program;
        assert_eq!([program("cursor"), program("antigravity"), program("kiro")], ["cursor-agent", "agy", "kiro-cli"]);
        // An online account, a program's name and a near miss are not agents.
        for other in ["openrouter", "cursor-agent", "agy", "gemini", "Claude", "", "claude "] {
            assert!(Harness::named(other).is_none(), "{other:?}");
        }
    }

    #[test]
    fn the_list_says_what_is_installed_and_claims_no_more_than_was_checked() {
        let f = crate::fixture::Fixture::folder();
        format::tests::stand_in(&f.root, "cursor-agent", "exit 0");
        // A program named like the agent's word is not the agent's program.
        format::tests::stand_in(&f.root, "kiro", "exit 0");
        let path = SearchPath::with(f.root.clone().into_os_string(), None);
        let text = listing(&path);
        let cursor = f.root.join("cursor-agent");
        let entry = format!(
            "cursor  Cursor CLI\n  installed   {}\n  starts as   cursor-agent agent -- PROMPT\n",
            cursor.display()
        );
        assert!(text.contains(&entry), "{text}");
        assert!(text.contains("  installed   kiro-cli is not on the search path\n"), "{text}");
        assert!(text.contains("  starts as   amp, and takes no first prompt: type it once Amp is open\n"), "{text}");
        assert!(text.contains("  starts as   goose run --interactive --text=PROMPT\n"), "{text}");
        assert!(text.contains("  milo usage  reads nothing this agent saves\n"));
        let found = doctor(&path);
        assert_eq!(found, "Agent tasks: 1 of 14 agents on this search path (cursor). milo task agents lists them.");
        let none = doctor(&SearchPath::with(OsString::new(), None));
        assert!(none.starts_with("Agent tasks: none of the 14 agents is on this search path (optional)."), "{none}");
    }

    #[test]
    fn the_integrations_guide_shows_the_catalog_as_it_is() {
        let guide = include_str!("../../docs/integrations.md");
        assert!(guide.contains(&table()), "docs/integrations.md should hold this table:\n{}", table());
    }

    #[test]
    fn a_harness_is_found_only_by_its_own_name_in_a_folder_named_in_full() {
        let f = crate::fixture::Fixture::folder();
        let bin = f.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        format::tests::stand_in(&bin, "codex", "exit 0");
        let path = |own: OsString| SearchPath::with(own, None);
        let found = installed(&path(bin.clone().into_os_string()));
        assert_eq!(found.iter().map(|harness| harness.id).collect::<Vec<_>>(), ["codex"]);
        assert_eq!(path(bin.clone().into_os_string()).find(found[0]), Some(bin.join("codex")));
        // A folder named from wherever the command runs is never searched.
        assert!(installed(&path("bin:.".into())).is_empty());

        // What this process cannot find, the shell's path may still have.
        let shell = SearchPath::with(OsString::new(), Some(bin.clone().into_os_string()));
        assert_eq!(shell.find(Harness::named("codex").unwrap()), Some(bin.join("codex")));
        assert_eq!(shell.find(Harness::named("claude").unwrap()), None);
    }

    #[test]
    fn the_shells_search_path_is_its_last_line_and_a_shell_that_stalls_is_given_up_on() {
        let f = crate::fixture::Fixture::folder();
        // A stand-in that shows how it was asked, after a startup file's greeting.
        format::tests::stand_in(&f.root, "noisy", "echo 'welcome back'\necho \"/asked:$1:$2\"\n");
        let said = path_from(&f.root.join("noisy"), Duration::from_secs(30)).unwrap();
        assert_eq!(said, "/asked:-lic:/usr/bin/printenv PATH");
        format::tests::stand_in(&f.root, "stalls", "exec sleep 60\n");
        assert_eq!(path_from(&f.root.join("stalls"), Duration::from_millis(200)), None);
        format::tests::stand_in(&f.root, "fails", "echo /bin\nexit 1\n");
        assert_eq!(path_from(&f.root.join("fails"), Duration::from_secs(30)), None);
    }
}
