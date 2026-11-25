//! Runs the real `milo task new` beside stand-ins for `rex` and the four agents.
//!
//! The stand-in `rex` records every call word for word and answers as Rex
//! does, so these tests show what the launcher asks Rex for, in which order
//! against Git, and what it reports when a step fails. No Rex server is
//! asked and no agent runs. `tests/rex_lab.rs` starts tasks on a real server.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const SESSION: &str = "session:01a212ce-82f4-7ed0-bdb4-88aa2edd12fa";
const BLOCK: &str = "block:01a212ce-82f4-7ad5-9ef1-d470b260a844";
const AGENTS: [&str; 4] = ["claude", "codex", "omp", "opencode"];
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
        for agent in AGENTS {
            scene.program(agent, "#!/bin/sh\nexit 0\n");
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

    /// Runs `milo task new` in the project, away from the Rex session and settings of whoever runs the test.
    fn task(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_milo"))
            .args(["task", "new"])
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
            .stdin(Stdio::null())
            .output()
            .unwrap()
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

#[test]
fn each_agent_starts_interactively_in_a_new_worktree_and_a_background_session() {
    // Shell syntax, an option, quotes and a second line: all of it is the prompt and none of it is run.
    let prompt = "--print $(touch pwned); `touch pwned` \"q\" 'q' | & ;\nsecond line";
    for agent in AGENTS {
        let scene = Scene::new();
        let branch = format!("task/{agent}");
        let (ok, out, err) =
            said(&scene.task(&["--agent", agent, "--branch", &branch, "--base", "main", "--prompt", prompt]));
        let tree = scene.tree(&format!("task-{agent}"));
        assert!(ok, "{agent}: {err}");
        assert_eq!(out, format!("worktree {}\nsession {SESSION}\n", tree.display()));

        let program = scene.root.join("bin").join(agent).display().to_string();
        let dir = tree.display().to_string();
        let label = format!("project/task-{agent}");
        let mut expected = words(&["new", &label, "-c", &dir, "--interactive", "--json", "--", &program]);
        expected.extend(match agent {
            "claude" | "codex" => words(&["--", prompt]),
            "omp" => words(&["launch", "--", prompt]),
            _ => vec![format!("--prompt={prompt}")],
        });
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
            "no Rex server answered; nothing was created",
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
