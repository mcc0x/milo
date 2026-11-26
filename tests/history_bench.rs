//! Times opening a commit in History of a release build, in a generated
//! repository: a commit that touches five files, and one that reformats and
//! moves thousands. A second repository has commits with subjects of two
//! megabytes each.
//!
//! Each run starts `milo history` in a real terminal and times what a person
//! waits for after Enter on a commit: the frame that lists its files, and the
//! frame after Down when it is pressed straight after Enter. It also times
//! Down on the opened list, and the first frame and Down among the long
//! subjects. Only the command line and the terminal are used, so any build
//! can be timed.
//!
//! ```sh
//! cargo test --release --locked --test history_bench -- --ignored --nocapture
//! ```
//!
//! `tests/bench/report.rs` lists the variables that choose the builds, the
//! runs and the report. The noise allowed before a ratio fails the test is
//! half a millisecond for a frame that waits for Git and twenty microseconds
//! for Down alone.

#![cfg(unix)]

#[path = "bench/report.rs"]
mod report;
#[path = "bench/terminal.rs"]
mod terminal;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use report::{Bench, Samples};
use terminal::Terminal;

/// History is a narrow pane beside a shell.
const COLUMNS: u16 = 60;
const ROWS: u16 = 50;
/// The files the large commit touches, and the functions in each.
const SWEEP_FILES: usize = 6000;
const FUNCTIONS: usize = 60;
/// Every file this far apart is moved by the large commit, not edited.
const MOVED_EVERY: usize = 40;
/// How often Down is timed on an opened commit.
const DOWNS: usize = 20;
/// The commits with long subjects, and the words in each subject.
const LONG_SUBJECTS: usize = 40;
const SUBJECT_WORDS: usize = 400_000;

/// A commit to open, and how many rows below the newest it is.
struct Scenario {
    name: &'static str,
    below_newest: usize,
    files: usize,
}

const SCENARIOS: &[Scenario] = &[
    Scenario { name: "five-files", below_newest: 0, files: 5 },
    Scenario { name: "formatter-sweep", below_newest: 1, files: SWEEP_FILES },
];

/// A throwaway home and repository holding the generated commits.
struct Project {
    root: PathBuf,
}

impl Project {
    /// Three commits, newest first: five files edited, every source file
    /// reformatted or moved, and the files as they began.
    fn new() -> Self {
        let project = Self::empty("files");
        let source = project.folder().join("src");
        fs::create_dir_all(&source).unwrap();
        for file in 0..SWEEP_FILES {
            fs::write(source.join(format!("m{file:04}.rs")), module(file, "    ")).unwrap();
        }
        for file in 0..5 {
            fs::write(project.folder().join(format!("five{file}.rs")), "pub fn five() {}\n").unwrap();
        }
        project.commit("benchbase every file as it began");
        for file in 0..SWEEP_FILES {
            let path = source.join(format!("m{file:04}.rs"));
            if file % MOVED_EVERY == 0 {
                fs::rename(path, source.join(format!("moved{file:04}.rs"))).unwrap();
            } else {
                fs::write(path, module(file, "        ")).unwrap();
            }
        }
        project.commit("benchsweep reformat every source file");
        for file in 0..5 {
            fs::write(project.folder().join(format!("five{file}.rs")), "pub fn five() {}\npub fn six() {}\n").unwrap();
        }
        project.commit("benchfive add a function to five files");
        project
    }

    /// Commits whose first paragraph, which Git gives as the subject, is megabytes long.
    fn long_subjects() -> Self {
        let project = Self::empty("subjects");
        let message = project.root.join("message");
        for commit in 0..LONG_SUBJECTS {
            fs::write(&message, format!("benchsubject{commit:02} {}\n", "word ".repeat(SUBJECT_WORDS))).unwrap();
            let identity = ["-c", "user.name=Milo Test", "-c", "user.email=test@example.com"];
            let commit = ["commit", "-q", "--allow-empty", "-F", message.to_str().unwrap()];
            project.git(&[&identity[..], &commit[..]].concat());
        }
        project
    }

    /// A new repository in a home of its own.
    fn empty(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("milo-history-bench-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let project = Self { root };
        fs::create_dir_all(project.folder()).unwrap();
        project.git(&["init", "-q", "--template=", "-b", "main"]);
        project
    }

    fn folder(&self) -> PathBuf {
        self.root.join("project")
    }

    /// Neither the developer's settings nor the Rex they work in reach the pane.
    fn command(&self, program: impl AsRef<Path>) -> Command {
        let mut command = Command::new(program.as_ref());
        command
            .env_clear()
            .current_dir(self.folder())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TERM", "xterm-256color");
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self.command("git").args(args).stdin(Stdio::null()).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["-c", "user.name=Milo Test", "-c", "user.email=test@example.com", "commit", "-q", "-m", message]);
    }

    /// History with the cursor on the scenario's commit, still closed.
    fn history(&self, binary: &Path, scenario: &Scenario) -> Terminal {
        let mut pane = Terminal::start(self.command(binary).arg("history"), COLUMNS, ROWS);
        pane.frame_showing(0, "benchbase");
        pane.settle();
        for _ in 0..scenario.below_newest {
            pane.press(b"j");
        }
        pane
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A source file whose every function body is indented by `indent`.
fn module(file: usize, indent: &str) -> String {
    let mut text = format!("// module {file}\n");
    for function in 0..FUNCTIONS {
        writeln!(text, "fn f{file}_{function}(a: u32) -> u32 {{").unwrap();
        writeln!(text, "{indent}let b = a + {function};\n{indent}b * {file}\n}}\n").unwrap();
    }
    text
}

fn run(samples: &mut Samples, binary: &Path, project: &Project, scenario: &Scenario) {
    // History says how many files a commit touched once Git has listed them.
    let listed = format!("{} files", scenario.files);
    let mut pane = project.history(binary, scenario);
    samples.push(scenario.name, "show-files", pane.press_until(b"\r", &listed));
    pane.settle();
    for _ in 0..DOWNS {
        samples.push(scenario.name, "down", pane.press(b"j"));
        pane.press(b"k");
    }
    drop(pane);
    // A commit's files are kept once read, so the second measure needs a pane of its own.
    let mut pane = project.history(binary, scenario);
    samples.push(scenario.name, "down-while-opening", pane.press(b"\rj"));
}

/// Opens History on the long subjects and moves among them.
fn run_subjects(samples: &mut Samples, binary: &Path, project: &Project) {
    let began = Instant::now();
    let mut pane = Terminal::start(project.command(binary).arg("history"), COLUMNS, ROWS);
    pane.frame_showing(0, "benchsubject00");
    samples.push("long-subjects", "first-paint", began.elapsed());
    pane.settle();
    for _ in 0..DOWNS {
        samples.push("long-subjects", "down", pane.press(b"j"));
        pane.press(b"k");
    }
}

fn noise(measure: &str) -> Duration {
    Duration::from_micros(if measure == "down" { 20 } else { 500 })
}

#[test]
#[ignore = "a benchmark; run it alone on a release build"]
fn history_commit_files() {
    let bench = Bench::from_env(env!("CARGO_BIN_EXE_milo"), "history_bench");
    let mut heading = bench.heading(&format!("milo history bench: {COLUMNS}x{ROWS} terminal"));
    writeln!(heading, "# formatter-sweep: {SWEEP_FILES} files of {FUNCTIONS} functions, one in {MOVED_EVERY} moved")
        .unwrap();
    writeln!(heading, "# down-while-opening: Enter and Down sent together, to the next frame").unwrap();
    let project = Project::new();
    let mut samples = [Samples::default(), Samples::default()];
    for scenario in SCENARIOS {
        for turn in 0..bench.runs {
            for (binary, build) in bench.builds(turn) {
                run(&mut samples[build], binary, &project, scenario);
            }
        }
    }
    writeln!(heading, "# long-subjects: {LONG_SUBJECTS} commits whose subjects are {SUBJECT_WORDS} words each")
        .unwrap();
    let subjects = Project::long_subjects();
    for turn in 0..bench.runs {
        for (binary, build) in bench.builds(turn) {
            run_subjects(&mut samples[build], binary, &subjects);
        }
    }
    bench.report(heading, &samples, noise);
}
