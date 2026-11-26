//! Times what a person waits for when a pane opens on a project, in a
//! release build and two generated repositories: an ordinary one of a few
//! hundred files, and a large one of fifty thousand. Both have an upstream,
//! commits not yet pushed, edited files and new ones.
//!
//! Each run starts the outline, the history and the reader in a real terminal
//! and times the first frame that shows the project. In the outline it also
//! times opening the search and a letter typed into it. In the history it
//! rewrites a file and times the slowest of the keys pressed while the pane
//! takes that in, which is when it asks Git for the project's status. Only
//! the command line and the terminal are used, so any build can be timed.
//!
//! ```sh
//! cargo test --release --locked --test open_bench -- --ignored --nocapture
//! ```
//!
//! `tests/bench/report.rs` lists the variables that choose the builds, the
//! runs and the report. The noise allowed before a ratio fails the test is
//! twenty microseconds for a search key and half a millisecond for the rest.

#[path = "bench/report.rs"]
mod report;
#[path = "bench/terminal.rs"]
mod terminal;

use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use report::{Bench, Samples};
use terminal::Terminal;

/// A pane beside a shell, and a reader as wide as a floating one.
const PANE: (u16, u16) = (40, 50);
const READER: (u16, u16) = (100, 50);
/// The files in each top folder.
const FOLDER_FILES: usize = 400;
/// The functions in each file, one to a line.
const FUNCTIONS: usize = 30;
/// A pane started as another ends shares the machine with its teardown.
const BETWEEN_PANES: Duration = Duration::from_millis(300);
/// The letters typed into the outline's search, each timed.
const QUERY: &[u8] = b"file0012";
/// How long keys are pressed after a file is rewritten. The pane looks at
/// the project within it: it rests at most a few hundred milliseconds first.
const REFRESH_WINDOW: Duration = Duration::from_millis(1500);
const BETWEEN_KEYS: Duration = Duration::from_millis(5);

/// How much of each kind of work a generated project holds.
struct Size {
    name: &'static str,
    files: usize,
    commits: usize,
    unpushed: usize,
    edited: usize,
    new: usize,
}

const SIZES: &[Size] = &[
    Size { name: "ordinary", files: 400, commits: 300, unpushed: 3, edited: 5, new: 3 },
    Size { name: "large", files: 50_000, commits: 2000, unpushed: 20, edited: 200, new: 500 },
];

/// A throwaway home, a repository in it and the repository it pushes to.
struct Project {
    root: PathBuf,
    /// An edited file, for the reader to show and for a rewrite to wake the history.
    edited: String,
}

impl Project {
    fn new(size: &Size) -> Self {
        let root = std::env::temp_dir().join(format!("milo-open-bench-{}-{}", std::process::id(), size.name));
        let _ = fs::remove_dir_all(&root);
        let mut project = Self { root, edited: String::new() };
        fs::create_dir_all(project.folder()).unwrap();
        project.git(&["init", "-q", "--template=", "-b", "main"]);
        project.import(size);
        project.git(&["reset", "-q", "--hard", "main"]);
        let origin = project.root.join("origin.git");
        project.git(&["clone", "-q", "--bare", ".", origin.to_str().unwrap()]);
        project.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
        project.git(&["fetch", "-q", "origin"]);
        project.git(&["branch", "-q", "--set-upstream-to=origin/main"]);
        for commit in 0..size.unpushed {
            project.append(&path(size, commit * 13 + 5), &format!("// unpushed {commit}\n"));
            let identity = ["-c", "user.name=Milo Test", "-c", "user.email=test@example.com"];
            let message = format!("benchunpushed {commit}");
            project.git(&[&identity[..], &["commit", "-q", "-a", "-m", &message]].concat());
        }
        for edit in 0..size.edited {
            project.append(&path(size, edit * 101 + 9), &format!("// benchedit {edit}\nfn edit{edit}() {{}}\n"));
        }
        for new in 0..size.new {
            let folder = project.folder().join(format!("pkg{:03}/new", new % folders(size)));
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join(format!("fresh{new:04}.rs")), module(new, 1)).unwrap();
        }
        project.edited = path(size, 9);
        project
    }

    /// Commits every file and then a history of small edits, without a checkout for each.
    fn import(&self, size: &Size) {
        let mut stream = String::new();
        let mut commit = |number: usize, message: &str, files: &[(String, String)]| {
            let when = 1_700_000_000 + number * 600;
            writeln!(stream, "commit refs/heads/main\nmark :{number}").unwrap();
            writeln!(stream, "committer Milo Test <test@example.com> {when} +0000").unwrap();
            writeln!(stream, "data {}\n{message}", message.len()).unwrap();
            if number > 1 {
                writeln!(stream, "from :{}", number - 1).unwrap();
            }
            for (path, text) in files {
                writeln!(stream, "M 100644 inline {path}\ndata {}\n{text}", text.len()).unwrap();
            }
        };
        let every: Vec<_> = (0..size.files).map(|file| (path(size, file), module(file, 0))).collect();
        commit(1, "benchbase every file as it began", &every);
        // The same commits every run: a fixed sequence stands in for chance.
        let mut state = 1_usize;
        for number in 2..=size.commits {
            let touched: Vec<_> = (0..1 + number % 5)
                .map(|_| {
                    state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                    let file = (state >> 33) % size.files;
                    (path(size, file), module(file, number))
                })
                .collect();
            commit(number, &format!("benchcommit {number} touch a few files"), &touched);
        }
        let file = self.root.join("stream");
        fs::write(&file, stream).unwrap();
        let mut import = self.command("git");
        import.args(["fast-import", "--quiet"]).stdin(File::open(&file).unwrap());
        assert!(import.status().unwrap().success(), "git fast-import");
        fs::remove_file(file).unwrap();
    }

    fn folder(&self) -> PathBuf {
        self.root.join("project")
    }

    fn append(&self, path: &str, text: &str) {
        let file = self.folder().join(path);
        let mut whole = fs::read_to_string(&file).unwrap();
        whole.push_str(text);
        fs::write(file, whole).unwrap();
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

    fn git(&self, args: &[&str]) {
        let output = self.command("git").args(args).stdin(Stdio::null()).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }

    /// Starts a pane and returns it with how long its first frame showing `text` took.
    fn open(&self, binary: &Path, args: &[&str], (columns, rows): (u16, u16), text: &str) -> (Terminal, Duration) {
        std::thread::sleep(BETWEEN_PANES);
        let began = Instant::now();
        let mut pane = Terminal::start(self.command(binary).args(args), columns, rows);
        pane.frame_showing(0, text);
        (pane, began.elapsed())
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn folders(size: &Size) -> usize {
    (size.files / FOLDER_FILES).max(1)
}

fn path(size: &Size, file: usize) -> String {
    let file = file % size.files;
    format!("pkg{:03}/mod{:02}/file{file:06}.rs", file * 7 % folders(size), file % 37)
}

/// A source file whose every function differs with `version`.
fn module(file: usize, version: usize) -> String {
    (0..FUNCTIONS).fold(String::new(), |mut text, function| {
        writeln!(text, "fn f{file}_{function}(a: u32) -> u32 {{ a + {function} + {version} }}").unwrap();
        text
    })
}

fn run(samples: &mut Samples, binary: &Path, project: &Project, size: &Size) {
    let (mut outline, took) = project.open(binary, &["outline"], PANE, "pkg000");
    samples.push(size.name, "outline-first-frame", took);
    outline.settle();
    samples.push(size.name, "outline-search-open", outline.press_until(b"/", "Find "));
    for &letter in QUERY {
        samples.push(size.name, "outline-search-key", outline.press(&[letter]));
    }
    drop(outline);

    let (reader, took) = project.open(binary, &["view", &project.edited], READER, "benchedit");
    samples.push(size.name, "reader-first-frame", took);
    drop(reader);

    let (mut history, took) = project.open(binary, &["history"], PANE, "benchunpushed");
    samples.push(size.name, "history-first-frame", took);
    history.settle();
    // The same text again changes the file's time and nothing Git reports.
    let file = project.folder().join(&project.edited);
    fs::write(&file, fs::read(&file).unwrap()).unwrap();
    let until = Instant::now() + REFRESH_WINDOW;
    let (mut slowest, mut down) = (Duration::ZERO, true);
    while Instant::now() < until {
        slowest = slowest.max(history.press(if down { b"j" } else { b"k" }));
        down = !down;
        std::thread::sleep(BETWEEN_KEYS);
    }
    samples.push(size.name, "history-slowest-key-in-refresh", slowest);
}

fn noise(measure: &str) -> Duration {
    Duration::from_micros(if measure == "outline-search-key" { 20 } else { 500 })
}

#[test]
#[ignore = "a benchmark; run it alone on a release build"]
fn opening_a_project() {
    let bench = Bench::from_env(env!("CARGO_BIN_EXE_milo"), "open_bench");
    let ((columns, rows), (wide, tall)) = (PANE, READER);
    let mut heading = bench.heading(&format!("milo open bench: {columns}x{rows} panes, {wide}x{tall} reader"));
    for Size { name, files, commits, unpushed, edited, new } in SIZES {
        writeln!(
            heading,
            "# {name}: {files} files, {commits} commits, {unpushed} unpushed, {edited} edited, {new} new"
        )
        .unwrap();
    }
    writeln!(heading, "# history-slowest-key-in-refresh: Down and Up for {REFRESH_WINDOW:?} after a file is rewritten")
        .unwrap();
    let mut samples = [Samples::default(), Samples::default()];
    for size in SIZES {
        let project = Project::new(size);
        for turn in 0..bench.runs {
            for (binary, build) in bench.builds(turn) {
                run(&mut samples[build], binary, &project, size);
            }
        }
    }
    bench.report(heading, &samples, noise);
}
