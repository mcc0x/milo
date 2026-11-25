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

/// How a harness is given the prompt of an interactive session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prompt {
    /// As one word after these, the last of which ends the harness's options.
    After(&'static [&'static str]),
    /// As the value of this option, joined to it with `=` so that it cannot be read as another option.
    Option(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Harness {
    /// The word `--agent` takes, which is also the program's name.
    pub id: &'static str,
    /// The name people know the agent by.
    pub name: &'static str,
    prompt: Prompt,
    /// Whether the harness runs a prompt that is one bare word as its own
    /// command of that name, even after the end of its options.
    words_are_commands: bool,
}

/// Every harness a task can start, in the order they are offered.
pub(crate) const HARNESSES: [Harness; 4] = [
    // `claude -- doctor` runs Claude Code's `doctor`, checked with 2.1.311.
    Harness { id: "claude", name: "Claude Code", prompt: Prompt::After(&["--"]), words_are_commands: true },
    Harness { id: "codex", name: "Codex", prompt: Prompt::After(&["--"]), words_are_commands: false },
    // Without `launch`, a first word such as `models` is routed to that command.
    Harness { id: "omp", name: "oh-my-pi", prompt: Prompt::After(&["launch", "--"]), words_are_commands: false },
    Harness { id: "opencode", name: "OpenCode", prompt: Prompt::Option("--prompt"), words_are_commands: false },
];

impl Harness {
    pub(crate) fn named(id: &str) -> Option<&'static Self> {
        HARNESSES.iter().find(|harness| harness.id == id)
    }

    /// The words after the program's name that open an interactive session,
    /// with `prompt` as its first message when there is one. The prompt is
    /// always a whole word of its own: no shell and no option parser reads it.
    pub(crate) fn arguments(&self, prompt: Option<&str>) -> Result<Vec<OsString>, String> {
        let Some(prompt) = prompt else { return Ok(Vec::new()) };
        let bare_word = prompt.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if self.words_are_commands && bare_word {
            return Err(format!(
                "{} would run the one-word prompt {prompt} as its own command; describe the task in a few words",
                self.name
            ));
        }
        Ok(match self.prompt {
            Prompt::After(words) => words.iter().map(OsString::from).chain([OsString::from(prompt)]).collect(),
            Prompt::Option(option) => vec![OsString::from(format!("{option}={prompt}"))],
        })
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

    /// A search path for a test, which must not ask the shell of whoever runs it.
    #[cfg(test)]
    pub(crate) fn with(own: OsString, shell: Option<OsString>) -> Self {
        Self { own, shell: OnceCell::from(shell) }
    }

    /// The harness's program, by its fixed name in a folder named in full.
    /// The shell is asked at most once, and only when this process's own
    /// path does not have the program.
    pub(crate) fn find(&self, harness: &Harness) -> Option<PathBuf> {
        format::find(harness.id, &self.own)
            .or_else(|| format::find(harness.id, self.shell.get_or_init(shell_path).as_ref()?))
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
        assert_eq!(words("claude", Some(prompt)), ["--", prompt]);
        assert_eq!(words("codex", Some(prompt)), ["--", prompt]);
        assert_eq!(words("omp", Some(prompt)), ["launch", "--", prompt]);
        assert_eq!(words("opencode", Some(prompt)), [format!("--prompt={prompt}")]);
        for harness in &HARNESSES {
            assert!(words(harness.id, None).is_empty(), "{} opens idle without a prompt", harness.id);
            let args = words(harness.id, Some("fix it"));
            let one_shot = ["-p", "--print", "exec", "run", "--mode", "--bg"];
            assert!(!args.iter().any(|word| one_shot.contains(&word.as_str())), "{args:?}");
        }
    }

    #[test]
    fn a_prompt_a_harness_would_run_as_its_own_command_is_refused_and_never_changed() {
        let claude = Harness::named("claude").unwrap();
        for word in ["doctor", "update", "auto-mode"] {
            let why = claude.arguments(Some(word)).unwrap_err();
            assert!(why.contains("Claude Code") && why.contains(word), "{why}");
        }
        assert_eq!(words("claude", Some("run doctor")), ["--", "run doctor"]);
        assert_eq!(words("codex", Some("doctor")), ["--", "doctor"]);
        assert_eq!(words("omp", Some("models")), ["launch", "--", "models"]);
    }

    #[test]
    fn only_the_four_local_harnesses_are_known() {
        assert_eq!(ids(), "claude, codex, omp, opencode");
        for other in ["openrouter", "copilot", "Claude", "", "claude "] {
            assert!(Harness::named(other).is_none(), "{other:?}");
        }
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
