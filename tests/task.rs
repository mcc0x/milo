//! Runs the real `milo task new` beside stand-ins for `rex` and every agent.
//!
//! The stand-in `rex` records every call word for word and answers as Rex
//! does, so these tests show what the launcher asks Rex for, in which order
//! against Git, and what it reports when a step fails. No Rex server is
//! asked and no agent runs. `tests/rex_lab.rs` starts tasks on a real server.

use std::fs::{self, File};
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const SESSION: &str = "session:01a212ce-82f4-7ed0-bdb4-88aa2edd12fa";
const BLOCK: &str = "block:01a212ce-82f4-7ad5-9ef1-d470b260a844";
/// Each agent that takes a first prompt: its word, its program, and the words
/// it is started with. The prompt is a word of its own after them, or joined
/// to the last when that ends in `=`.
const AGENTS: [(&str, &str, &[&str]); 13] = [
    ("claude", "claude", &["--"]),
    ("codex", "codex", &["--"]),
    ("omp", "omp", &["launch", "--"]),
    ("opencode", "opencode", &["--prompt="]),
    ("pi", "pi", &["--"]),
    ("copilot", "copilot", &["--interactive="]),
    ("cursor", "cursor-agent", &["agent", "--"]),
    ("antigravity", "agy", &["--prompt-interactive="]),
    ("kiro", "kiro-cli", &["chat", "--"]),
    ("cline", "cline", &["-i", "--"]),
    ("droid", "droid", &["--"]),
    ("goose", "goose", &["run", "--interactive", "--text="]),
    ("qwen", "qwen", &["--prompt-interactive="]),
];
static NEXT: AtomicU32 = AtomicU32::new(0);

/// Each call is one line of words, each word ending in a tab. Files beside
/// the log choose the answers: `down`, `refuse`, `process`, `status`, `screen`.
const REX: &str = r#"#!/bin/sh
here=$(dirname "$0")/..
for word in "$@"; do printf '%s\t' "$(printf '%s' "$word" | od -An -tx1 | tr -d ' \n')" >> "$here/calls"; done
echo >> "$here/calls"
for word in "$@"; do
  case "$word" in
    whoami) [ -e "$here/down" ] && exit 1; echo '{}'; exit 0 ;;
    new) [ -e "$here/refuse" ] && exit 1
      echo '{"session_id":"SESSION","initial_windows":[{"window_id":"window:1","block_ids":["BLOCK"]}]}'; exit 0 ;;
    block) cat "$here/process"; exit 0 ;;
    wait) exit "$(cat "$here/status")" ;;
    capture) cat "$here/screen"; exit 0 ;;
    kill) exit 0 ;;
  esac
done
exit 1
"#;

/// A throwaway project with one commit on `main`, and a folder of stand-ins first on the path.
struct Scene {
    root: PathBuf,
}

impl Scene {
    fn new() -> Self {
        let name = format!("milo-task-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let scene = Self { root: std::env::temp_dir().canonicalize().unwrap().join(name) };
        let _ = fs::remove_dir_all(&scene.root);
        fs::create_dir_all(scene.root.join("bin")).unwrap();
        fs::create_dir_all(scene.project()).unwrap();
        scene.program("rex", &REX.replace("SESSION", SESSION).replace("BLOCK", BLOCK));
        for program in AGENTS.map(|(_, program, _)| program).into_iter().chain(["amp"]) {
            scene.program(program, "#!/bin/sh\nexit 0\n");
        }
        // The login shell's search path, asked for when an agent is not on the path above.
        scene.program("shell", "#!/bin/sh\necho /nowhere\n");
        scene.answer("process", r#"{"last_exit":null,"child":{"pid":7}}"#);
        scene.git(&["init", "-q", "--template=", "-b", "main"]);
        fs::write(scene.project().join("page.txt"), "one\n").unwrap();
        scene.git(&["add", "."]);
        scene.git(&["-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", "first"]);
        scene
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn program(&self, name: &str, script: &str) {
        let path = self.root.join("bin").join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn answer(&self, name: &str, text: &str) {
        fs::write(self.root.join(name), text).unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.project())
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    /// `milo` in the project, away from the Rex session and settings of whoever runs the test.
    fn milo(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_milo"));
        command
            .args(args)
            .current_dir(self.project())
            // Only the stand-ins and Git: an agent installed on this machine must not be found.
            .env("PATH", format!("{}:/usr/bin:/bin", self.root.join("bin").display()))
            .env("HOME", &self.root)
            .env("SHELL", self.root.join("bin/shell"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove("REX_SERVER")
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK")
            .stdin(Stdio::null());
        command
    }

    /// `milo task new` there.
    fn command(&self, args: &[&str]) -> Command {
        self.milo(&[&["task", "new"], args].concat())
    }

    /// Runs it as a script would, with no terminal to read from.
    fn task(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Runs it as a shell would, with a terminal on every standard stream.
    /// Gives how the program in that terminal ended and what the terminal was sent.
    fn task_in_a_terminal(&self, args: &[&str]) -> (Option<i32>, String) {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: Both descriptor outputs are live, and the optional name, modes and size are null.
        let opened = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
        };
        assert_eq!(opened, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: A successful openpty gives the caller the first descriptor.
        let mut master = unsafe { File::from_raw_fd(master) };
        // SAFETY: It gives the second too, which is a different descriptor.
        let slave = unsafe { File::from_raw_fd(slave) };
        let stream = || Stdio::from(slave.try_clone().unwrap());
        let status = self.command(args).stdin(stream()).stdout(stream()).stderr(stream()).status().unwrap();
        // This side still holds the terminal open, so what was written is there to read.
        let mut sent = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let mut ready = libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: One live pollfd names an open descriptor.
            if unsafe { libc::poll(&mut ready, 1, 200) } <= 0 {
                break;
            }
            match master.read(&mut buffer) {
                Ok(count) if count > 0 => sent.extend_from_slice(&buffer[..count]),
                _ => break,
            }
        }
        (status.code().or(status.signal().map(|signal| -signal)), String::from_utf8_lossy(&sent).replace('\r', ""))
    }

    /// Every call the stand-in `rex` received, as lists of words.
    fn calls(&self) -> Vec<Vec<String>> {
        let unhex = |word: &str| {
            let bytes = (0..word.len()).step_by(2).map(|i| u8::from_str_radix(&word[i..i + 2], 16).unwrap());
            String::from_utf8(bytes.collect()).unwrap()
        };
        let log = fs::read_to_string(self.root.join("calls")).unwrap_or_default();
        log.lines().map(|line| line.split_terminator('\t').map(unhex).collect()).collect()
    }

    /// The words of the one call that names `command`, after Rex's own options.
    fn call(&self, command: &str) -> Vec<String> {
        let mut found = self.calls().into_iter().filter(|call| call.iter().any(|word| word == command));
        let call = found.next().unwrap_or_else(|| panic!("no rex {command} in {:?}", self.calls()));
        assert!(found.next().is_none(), "more than one rex {command}");
        let at = call.iter().position(|word| word == command).unwrap();
        call[at..].to_vec()
    }

    fn worktrees(&self) -> usize {
        self.git(&["worktree", "list", "--porcelain"]).lines().filter(|line| line.starts_with("worktree ")).count()
    }

    fn tree(&self, name: &str) -> PathBuf {
        self.root.join("project-worktrees").join(name)
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn said(output: &Output) -> (bool, String, String) {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    (output.status.success(), text(&output.stdout), text(&output.stderr))
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| (*word).to_owned()).collect()
}

/// The words an agent is started with for `prompt`.
fn started_with(before: &[&str], prompt: &str) -> Vec<String> {
    let (last, rest) = before.split_last().unwrap();
    let mut args = words(rest);
    if last.ends_with('=') {
        args.push(format!("{last}{prompt}"));
    } else {
        args.extend(words(&[last, prompt]));
    }
    args
}

#[test]
fn each_agent_starts_interactively_in_a_new_worktree_and_a_background_session() {
    // Each agent has a scene of its own, so none waits while another's session settles.
    std::thread::scope(|agents| {
        for (agent, program, before) in AGENTS {
            agents.spawn(move || starts_in_a_session(agent, program, before));
        }
    });
}

fn starts_in_a_session(agent: &str, program: &str, before: &[&str]) {
    // Shell syntax, an option, quotes and a second line: all of it is the prompt and none of it is run.
    let literal = "--print $(touch pwned); `touch pwned` \"q\" 'q' | & ;\nsecond line";
    // Cline is the one agent refused a prompt that begins with a dash.
    let prompt = if agent == "cline" { &literal["--print ".len()..] } else { literal };
    {
        let scene = Scene::new();
        let branch = format!("task/{agent}");
        let (ok, out, err) =
            said(&scene.task(&["--agent", agent, "--branch", &branch, "--base", "main", "--prompt", prompt]));
        let tree = scene.tree(&format!("task-{agent}"));
        assert!(ok, "{agent}: {err}");
        assert_eq!(out, format!("worktree {}\nsession {SESSION}\n", tree.display()));

        let program = scene.root.join("bin").join(program).display().to_string();
        let dir = tree.display().to_string();
        let label = format!("project/task-{agent}");
        let mut expected = words(&["new", &label, "-c", &dir, "--interactive", "--json", "--", &program]);
        expected.extend(started_with(before, prompt));
        assert_eq!(scene.call("new"), expected, "{agent}");
        // The session is made and left where it is: nothing selects, focuses, renames or types into it.
        let asked: Vec<String> = scene.calls().iter().map(|call| call[3].clone()).collect();
        assert!(asked.iter().all(|command| ["whoami", "new", "block"].contains(&command.as_str())), "{asked:?}");
        assert_eq!(asked.iter().filter(|command| *command == "new").count(), 1);
        for call in scene.calls() {
            assert_eq!(call[..3], words(&["--autostart=false", "--timeout", "10s"]), "milo must never start a server");
        }

        assert_eq!(fs::read_to_string(tree.join("page.txt")).unwrap(), "one\n");
        assert_eq!(scene.git(&["-C", &dir, "branch", "--show-current"]).trim(), branch);
        assert_eq!(scene.git(&["branch", "--show-current"]).trim(), "main", "the caller's checkout was switched");
        assert_eq!(scene.git(&["status", "--porcelain"]), "", "the caller's checkout was changed");
        assert!(!scene.project().join("pwned").exists() && !tree.join("pwned").exists());
    }
}

#[test]
fn an_existing_worktree_is_used_as_it_is_and_no_prompt_opens_the_agent_idle() {
    let scene = Scene::new();
    let other = scene.root.join("other tree");
    scene.git(&["worktree", "add", "-q", "-b", "topic", other.to_str().unwrap()]);
    fs::write(other.join("draft.txt"), "unsaved work\n").unwrap();
    let (ok, out, err) = said(&scene.task(&["--agent", "opencode", "--worktree", "../other tree"]));
    assert!(ok, "{err}");
    assert_eq!(out, format!("worktree {}\nsession {SESSION}\n", other.display()));
    let program = scene.root.join("bin/opencode").display().to_string();
    let expected =
        words(&["new", "project/other tree", "-c", other.to_str().unwrap(), "--interactive", "--json", "--", &program]);
    assert_eq!(scene.call("new"), expected);
    assert_eq!(scene.worktrees(), 2);
    assert_eq!(fs::read_to_string(other.join("draft.txt")).unwrap(), "unsaved work\n");
    assert_eq!(scene.git(&["-C", other.to_str().unwrap(), "branch", "--show-current"]).trim(), "topic");
}

#[test]
fn an_agent_opens_idle_by_its_own_words_and_one_that_takes_no_first_prompt_is_refused_with_one() {
    let scene = Scene::new();
    let tree = scene.root.join("tree");
    scene.git(&["worktree", "add", "-q", "-b", "topic", tree.to_str().unwrap()]);
    let place = ["--worktree", tree.to_str().unwrap()];
    for (agent, program, idle) in [
        ("amp", "amp", &[][..]),
        ("kiro", "kiro-cli", &["chat"]),
        ("cline", "cline", &["-i"]),
        ("goose", "goose", &["session"]),
        ("cursor", "cursor-agent", &[]),
    ] {
        let _ = fs::remove_file(scene.root.join("calls"));
        let (ok, _, err) = said(&scene.task(&[&["--agent", agent], &place[..]].concat()));
        assert!(ok, "{agent}: {err}");
        let program = scene.root.join("bin").join(program).display().to_string();
        let after = scene.call("new").into_iter().skip_while(|word| *word != program).skip(1).collect::<Vec<_>>();
        assert_eq!(after, words(idle), "{agent}");
    }

    // Refusals that depend on the agent come before Git or Rex is asked for anything.
    let _ = fs::remove_file(scene.root.join("calls"));
    let new = ["--branch", "task/one", "--base", "main", "--prompt"];
    for (agent, prompt, why) in [
        ("amp", "fix the tests", "Amp takes no first prompt when it starts; leave out --prompt"),
        ("droid", "doctor", "Factory Droid would run the one-word prompt doctor as its own command"),
        ("cline", "update", "Cline CLI would run the one-word prompt update as its own command"),
        ("cline", "--yolo fix it", "Cline CLI would look for its own options in a prompt that begins with a dash"),
    ] {
        let (ok, out, err) = said(&scene.task(&[&["--agent", agent], &new[..], &[prompt]].concat()));
        assert!(!ok && out.is_empty() && err.contains(why), "{agent}: {out}{err}");
    }
    assert!(scene.calls().is_empty() && scene.worktrees() == 2 && !scene.root.join("project-worktrees").exists());
}

#[test]
fn here_gives_the_agent_this_terminal_in_the_worktree_and_asks_rex_for_nothing() {
    let scene = Scene::new();
    // No Rex server answers, and none is needed.
    scene.answer("down", "");
    let record = scene.root.join("ran");
    let agent = format!(
        "#!/bin/sh\nmine=pipe; [ -t 0 ] && [ -t 1 ] && mine=terminal\n\
         {{ pwd; echo \"$mine\"; printf '%s\\n' \"$#\" \"$@\"; }} >'{}'\necho agent screen\nexit 7\n",
        record.display()
    );
    scene.program("goose", &agent);
    let prompt = "--quiet $(touch pwned); `touch pwned` 'q'";
    let args = ["--agent", "goose", "--branch", "task/here", "--base", "main", "--prompt", prompt, "--here"];
    let (status, sent) = scene.task_in_a_terminal(&args);
    let tree = scene.tree("task-here");
    // The agent ran in Milo's place: its screen follows the folder, and its status is the command's.
    assert_eq!(sent, format!("worktree {}\nagent screen\n", tree.display()));
    assert_eq!(status, Some(7));
    let expected = format!("{}\nterminal\n3\nrun\n--interactive\n--text={prompt}\n", tree.display());
    assert_eq!(fs::read_to_string(&record).unwrap(), expected);
    assert!(scene.calls().is_empty(), "{:?}", scene.calls());
    assert_eq!(scene.git(&["-C", tree.to_str().unwrap(), "branch", "--show-current"]).trim(), "task/here");
    assert_eq!(scene.git(&["branch", "--show-current"]).trim(), "main", "the caller's checkout was switched");
    assert!(!scene.project().join("pwned").exists() && !tree.join("pwned").exists());
}

#[test]
fn here_without_a_terminal_or_with_another_host_is_refused_before_anything_is_created() {
    let scene = Scene::new();
    let new = ["--agent", "codex", "--branch", "task/one", "--base", "main", "--here"];
    let (ok, out, err) = said(&scene.task(&new));
    assert!(!ok && out.is_empty(), "{out}");
    assert_eq!(err, "milo: --here gives the agent this terminal, and this is not one; nothing was created\n");
    let (ok, _, err) = said(&scene.task(&[&new[..], &["--host", "devbox", "--project", "/srv/app"]].concat()));
    assert!(!ok && err.contains("cannot be combined with --host"), "{err}");
    // A missing agent is refused the same way whichever screen was asked for.
    fs::remove_file(scene.root.join("bin/codex")).unwrap();
    let (status, sent) = scene.task_in_a_terminal(&new);
    assert_eq!(status, Some(1));
    assert_eq!(sent, "milo: Codex (codex) is not installed on this computer; nothing was created\n");
    assert!(scene.calls().is_empty() && scene.worktrees() == 1 && !scene.root.join("project-worktrees").exists());
}

#[test]
fn a_refusal_before_the_launch_creates_no_worktree_and_asks_rex_for_no_session() {
    let scene = Scene::new();
    fs::remove_file(scene.root.join("bin/codex")).unwrap();
    scene.answer("down", "");
    for (args, why) in [
        (
            &["--agent", "codex", "--branch", "task/one", "--base", "main"][..],
            "Codex (codex) is not installed on this computer; nothing was created",
        ),
        (
            &["--agent", "claude", "--branch", "task/one", "--base", "missing"],
            "missing names no commit in this project",
        ),
        (&["--agent", "claude", "--branch", "main", "--base", "main"], "main is already checked out at"),
        (&["--agent", "claude", "--worktree", ".."], "is not a worktree of this project"),
        (
            &["--agent", "claude", "--branch", "task/one", "--base", "main", "--prompt", "doctor"],
            "one-word prompt doctor",
        ),
        (
            &["--agent", "claude", "--branch", "task/one", "--base", "main"],
            "no Rex server answered; nothing was created. Without Rex, add --here to run the agent in this terminal",
        ),
    ] {
        let (ok, out, err) = said(&scene.task(args));
        assert!(!ok && out.is_empty() && err.contains(why), "{args:?}: {out}{err}");
    }
    assert!(scene.worktrees() == 1 && !scene.root.join("project-worktrees").exists());
    assert_eq!(scene.git(&["branch", "--list", "task/*"]), "");
    assert!(scene.calls().iter().all(|call| call[3] == "whoami"), "{:?}", scene.calls());
}

#[test]
fn a_failure_after_the_worktree_exists_says_what_was_created_and_removes_nothing() {
    let scene = Scene::new();
    scene.answer("refuse", "");
    let (ok, out, err) = said(&scene.task(&["--agent", "omp", "--branch", "task/one", "--base", "main"]));
    let tree = scene.tree("task-one");
    let kept = format!("Created worktree {} on new branch task/one; it was not removed.", tree.display());
    assert!(!ok && out.is_empty(), "{out}");
    assert_eq!(err, format!("milo: Rex started no session. {kept}\n"));
    assert!(tree.join("page.txt").is_file() && scene.worktrees() == 2);

    // An agent that ends as soon as it starts leaves its session for the person to read.
    fs::remove_file(scene.root.join("refuse")).unwrap();
    scene.answer("process", r#"{"last_exit":{"exit_code":2,"error":"exit status 2"}}"#);
    let (ok, out, err) = said(&scene.task(&["--agent", "omp", "--worktree", tree.to_str().unwrap()]));
    assert!(!ok && out.is_empty(), "{out}");
    let expected = format!(
        "milo: oh-my-pi exited at once with status 2; Rex session {SESSION} may still show why. The worktree {} is as it was.\n",
        tree.display()
    );
    assert_eq!(err, expected);
    assert!(tree.join("page.txt").is_file() && scene.worktrees() == 2);
    assert!(scene.calls().iter().all(|call| call[3] != "kill"), "a session Milo made was ended");
}

#[test]
fn a_task_for_another_host_is_started_by_that_hosts_milo_and_changes_nothing_here() {
    let scene = Scene::new();
    scene.answer("status", "0");
    scene.answer("screen", &format!("worktree /srv/app-worktrees/task-one\nsession {SESSION}\n"));
    let prompt = "fix `it`; $(reboot)";
    let args =
        ["--agent", "claude", "--host", "devbox", "--project", "/srv/app", "--branch", "task/one", "--base", "main"];
    let (ok, out, err) = said(&scene.task(&[&args[..], &["--prompt", prompt]].concat()));
    assert!(ok, "{err}");
    assert_eq!(out, format!("worktree /srv/app-worktrees/task-one\nsession {SESSION}\nhost devbox\n"));
    let carried = words(&[
        "new", "milo-task", "--keep-open", "--json", "--", "milo", "task", "new", "--agent", "claude", "--project",
        "/srv/app", "--branch", "task/one", "--base", "main", "--prompt", prompt,
    ]);
    assert_eq!(scene.call("new"), carried);
    for call in scene.calls() {
        assert_eq!(call[3..5], words(&["-S", "devbox"]), "every call names the host: {call:?}");
    }
    assert_eq!(scene.call("kill"), words(&["kill", SESSION]), "only the carrying session is ended");
    assert!(scene.worktrees() == 1 && !scene.root.join("project-worktrees").exists());

    // The host's own refusal comes back as the reason, and nothing is invented in its place.
    scene.answer("status", "1");
    scene.answer("screen", "milo: /srv/app is not in a Git project; nothing was created\n");
    let (ok, out, err) = said(&scene.task(&args));
    assert!(!ok && out.is_empty());
    assert_eq!(err, "milo: on devbox: /srv/app is not in a Git project; nothing was created\n");
}

#[test]
fn the_list_and_the_doctor_name_the_agents_found_and_the_doctor_starts_no_shell() {
    let scene = Scene::new();
    for gone in ["claude", "kiro-cli", "amp"] {
        fs::remove_file(scene.root.join("bin").join(gone)).unwrap();
    }
    // A shell that is asked for its search path leaves a mark.
    let asked = scene.root.join("asked");
    scene.program("shell", &format!("#!/bin/sh\necho asked >>'{}'\necho /nowhere\n", asked.display()));

    let (ok, report, err) = said(&scene.milo(&["doctor"]).output().unwrap());
    assert!(ok, "{err}");
    let found = "Agent tasks: 11 of 14 agents on this search path \
                 (codex, omp, opencode, pi, copilot, cursor, antigravity, cline, droid, goose, qwen).";
    assert!(report.contains(found), "{report}");
    assert!(!asked.exists(), "milo doctor started the login shell");

    // The list is asked for, so it looks where a task would: the shell's path too.
    let (ok, list, err) = said(&scene.milo(&["task", "agents"]).output().unwrap());
    assert!(ok, "{err}");
    assert!(asked.exists());
    let cursor = format!("  installed   {}\n", scene.root.join("bin/cursor-agent").display());
    assert!(list.contains(&cursor) && list.contains("  installed   kiro-cli is not on the search path\n"), "{list}");
    assert_eq!(list.lines().filter(|line| line.starts_with("  starts as ")).count(), 14);
    // Neither starts anything: the doctor may ask Rex who it is, and no more.
    assert!(scene.calls().iter().all(|call| !call.contains(&"new".to_owned())), "{:?}", scene.calls());
    assert_eq!(scene.worktrees(), 1);
}
