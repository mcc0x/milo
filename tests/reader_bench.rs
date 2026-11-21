//! Times the reader of a release build on generated files: long lines, syntax
//! that spans lines, diffs just under the size Milo will load, and edits near
//! the end of a large draft.
//!
//! Each run starts `milo view` in a real terminal, answers its questions as a
//! terminal would, and times what a person waits for: the first frame that
//! shows the file, and the next frame after a key. Only the command line and
//! the terminal are used, so any build can be timed, whatever its revision.
//!
//! ```sh
//! cargo test --release --locked --test reader_bench -- --ignored --nocapture
//! ```
//!
//! - `MILO_TEST_BINARY` times that build instead of the one Cargo just made.
//! - `MILO_BENCH_BASE` also times that build, turn about with the first, and
//!   reports each median beside it. `tests/reader-compare.sh` builds one from
//!   a revision.
//! - `MILO_BENCH_MAX_RATIO`, with a base, fails the test when a median is more
//!   than that many times the base's and also slower by more than the noise
//!   allowed for its kind: half a millisecond for a first frame, twenty
//!   microseconds for a key.
//! - `MILO_BENCH_RUNS` sets how often each file is opened (10).
//! - `MILO_BENCH_REPORT` names a file to write the report to as well.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
const COLUMNS: u16 = 160;
const ROWS: u16 = 50;
/// Milo ends every frame it sends with this.
const FRAME_END: &[u8] = b"\x1b[?2026l";
/// The last question Milo asks a terminal as it starts.
const ASKED: &[u8] = b"\x1b[c";
/// A dark background, a cell size and a terminal type, as a terminal answers.
const ANSWERS: &[u8] = b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[6;16;8t\x1b[?62;22c";
/// Every generated file starts with a comment holding this word.
const MARK: &str = "miloreaderbench";
/// The largest diff of one file that Milo loads.
const DIFF_LIMIT: usize = 8 << 20;
/// A first frame's median must exceed the base's by this much, as well as by
/// the ratio, before it fails the test.
const PAINT_NOISE: Duration = Duration::from_micros(500);
/// The same allowance for the frame after a key.
const KEY_NOISE: Duration = Duration::from_micros(20);

/// A key pressed `times` times and timed under `measure`. After each press
/// `back` is pressed, untimed, to return to where the key acts again.
struct Keys {
    measure: Option<&'static str>,
    key: u8,
    back: Option<u8>,
    times: usize,
}

const fn keys(measure: &'static str, key: u8, times: usize) -> Keys {
    Keys { measure: Some(measure), key, back: None, times }
}

const fn there_and_back(measure: &'static str, key: u8, back: u8, times: usize) -> Keys {
    Keys { measure: Some(measure), key, back: Some(back), times }
}

const fn setup(key: u8) -> Keys {
    Keys { measure: None, key, back: None, times: 1 }
}

struct Scenario {
    name: &'static str,
    file: &'static str,
    at_commit: bool,
    keys: &'static [Keys],
}

const SCENARIOS: &[Scenario] = &[
    // What starting the reader costs before any file does.
    Scenario { name: "small-file", file: "small.rs", at_commit: false, keys: &[] },
    Scenario {
        name: "long-lines",
        file: "long.rs",
        at_commit: false,
        keys: &[keys("pan", b'l', 40), keys("line", b'j', 40), there_and_back("page", b' ', b'b', 8)],
    },
    Scenario {
        name: "multiline-rust",
        file: "blocks.rs",
        at_commit: false,
        keys: &[keys("page", b' ', 30), keys("line", b'j', 40), there_and_back("bottom", b'G', b'g', 10)],
    },
    Scenario {
        name: "multiline-python",
        file: "blocks.py",
        at_commit: false,
        keys: &[keys("page", b' ', 30), keys("line", b'j', 40), there_and_back("bottom", b'G', b'g', 10)],
    },
    // Lua is plain text to a reader that does not know it. The report's
    // syntax column says which reader was timed.
    Scenario {
        name: "multiline-lua",
        file: "blocks.lua",
        at_commit: false,
        keys: &[keys("page", b' ', 30), keys("line", b'j', 40), there_and_back("bottom", b'G', b'g', 10)],
    },
    Scenario {
        name: "near-limit-diff",
        file: "big.rs",
        at_commit: false,
        keys: &[keys("change", b'n', 30), keys("page", b' ', 30), there_and_back("bottom", b'G', b'g', 10)],
    },
    Scenario {
        name: "near-limit-commit",
        file: "big.rs",
        at_commit: true,
        keys: &[keys("change", b'n', 30), keys("page", b' ', 30), there_and_back("bottom", b'G', b'g', 10)],
    },
    Scenario {
        name: "long-line-diff",
        file: "pairs.rs",
        at_commit: false,
        keys: &[keys("change", b'n', 20), keys("pan", b'l', 40), keys("line", b'k', 40)],
    },
    Scenario {
        name: "edit-near-bottom",
        file: "blocks.rs",
        at_commit: false,
        keys: &[
            setup(b'G'),
            setup(b'e'),
            there_and_back("type", b'x', b'\x1a', 24),
            there_and_back("split-line", b'\r', b'\x1a', 24),
            setup(b'x'),
            there_and_back("undo", b'\x1a', b'\x19', 24),
            setup(b'\x1a'),
            there_and_back("redo", b'\x19', b'\x1a', 24),
            setup(b'\x1b'),
        ],
    },
];

/// A throwaway home and repository holding the generated files.
struct Project {
    root: PathBuf,
    head: String,
}

impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("milo-reader-bench-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("project")).unwrap();
        let mut project = Self { root, head: String::new() };
        project.git(&["init", "-q", "--template=", "-b", "main"]);
        // The reader shows a file without a diff only below two mebibytes.
        for (name, text) in [
            ("small.rs", format!("{}fn main() {{\n    let answer = 42;\n}}\n", header("a small file"))),
            ("long.rs", long_lines()),
            ("blocks.rs", rust_blocks()),
            ("blocks.py", python_blocks()),
            ("blocks.lua", lua_blocks()),
        ] {
            assert!(text.len() < 2 << 20, "{name} is {} bytes", text.len());
            project.write(name, &text);
        }
        project.write("big.rs", &big("compute"));
        project.write("pairs.rs", &pairs(false));
        project.commit("Add the files");
        project.write("big.rs", &big("process"));
        project.commit("Change three lines in every twenty of the big file");
        project.head = project.git(&["rev-parse", "HEAD"]).trim().to_owned();
        project.write("big.rs", &big("resolve"));
        project.write("pairs.rs", &pairs(true));
        project
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.root.join("project").join(name), text).unwrap();
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["-c", "user.name=Milo Test", "-c", "user.email=test@example.com", "commit", "-q", "-m", message]);
    }

    /// Neither the developer's settings nor the Rex they work in reach the reader.
    fn command(&self, program: impl AsRef<Path>) -> Command {
        let mut command = Command::new(program.as_ref());
        command
            .current_dir(self.root.join("project"))
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TERM", "xterm-256color")
            .env_remove("MILO_ICONS")
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK");
        for name in [
            "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self.command("git").args(args).stdin(Stdio::null()).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The size of the whole-file diff the reader asks Git for.
    fn diff_bytes(&self, file: &str, at_commit: bool) -> usize {
        let against = if at_commit { "HEAD^" } else { "HEAD" };
        let mut args = vec!["diff", "--unified=8388608", against];
        if at_commit {
            args.push("HEAD");
        }
        args.extend(["--", file]);
        self.git(&args).len()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn header(what: &str) -> String {
    format!("// {MARK} {what}\n")
}

/// Lines far wider than any pane, in a file just under the two mebibytes
/// the reader will show without a diff.
fn long_lines() -> String {
    let mut text = header("long lines");
    for line in 0..120 {
        let mut column = 0;
        while column < 16_000 {
            let piece = format!("let value_{line}_{column} = compute(alpha, \"text {column}\", {column} + 0x2a); ");
            column += piece.len();
            text.push_str(&piece);
        }
        text.push('\n');
    }
    text
}

/// Nested block comments and raw strings that hold each other's closing marks.
fn rust_blocks() -> String {
    let mut text = header("comments and raw strings across lines");
    for unit in 0..700 {
        writeln!(text, "/* Unit {unit} begins with a comment").unwrap();
        for line in 0..14 {
            writeln!(text, " * /* nested {line} */ still a comment with \"a quote\" and r#\"no string\"#").unwrap();
        }
        writeln!(
            text,
            " */\npub fn unit_{unit}(input: &str) -> usize {{\n    let text = r#\"raw {unit} with \"quotes\""
        )
        .unwrap();
        for line in 0..18 {
            writeln!(text, "        line {line} of the string, with */ and // and 'c' inside").unwrap();
        }
        writeln!(text, "    \"#;\n    let quote = '\"';\n    let number = 0x2a + 1_000 + {unit};").unwrap();
        writeln!(text, "    text.len() + input.len() + number + quote as usize // the end\n}}\n").unwrap();
    }
    text
}

/// Triple-quoted strings that hold quotes and comment marks.
fn python_blocks() -> String {
    let mut text = format!("# {MARK} strings across lines\n");
    for unit in 0..900 {
        writeln!(text, "def unit_{unit}(value: int) -> str:\n    \"\"\"Unit {unit} has a docstring").unwrap();
        for line in 0..12 {
            writeln!(text, "    line {line} with 'quotes', \"more\" and # no comment").unwrap();
        }
        writeln!(text, "    \"\"\"\n    text = '''single {unit}").unwrap();
        for line in 0..10 {
            writeln!(text, "    line {line} holding \"\"\" and a # mark").unwrap();
        }
        writeln!(text, "    '''\n    return f\"{{value + {unit}:>8}} {{text!r}}\"  # the end\n").unwrap();
    }
    text
}

/// Long-bracket comments and strings, whose closing marks depend on their openers.
fn lua_blocks() -> String {
    let mut text = format!("-- {MARK} long brackets across lines\n");
    for unit in 0..900 {
        writeln!(text, "--[==[ Unit {unit} begins with a comment").unwrap();
        for line in 0..12 {
            writeln!(text, "  line {line} with ]] and ]=] and \"a quote\" but no end").unwrap();
        }
        writeln!(text, "]==]\nlocal function unit_{unit}(value)\n  local text = [[long {unit}").unwrap();
        for line in 0..10 {
            writeln!(text, "    line {line} holding -- no comment and 'quotes'").unwrap();
        }
        writeln!(text, "  ]]\n  return string.format(\"%8d %s\", value + {unit}, text) -- the end\nend\n").unwrap();
    }
    text
}

/// Seventy thousand hundred-byte lines. Three lines in every twenty call
/// `edit`, so changing it adds three twentieths to the file's diff, which
/// starts at the top where the reader opens.
fn big(edit: &str) -> String {
    let mut text = header("a diff near the limit");
    for line in 0..70_000 {
        let call = if line % 20 < 3 { edit } else { "compute" };
        let code = format!("    let v{line:05} = {call}(alpha_{line:05}, \"text {line:05}\", {line} * 3) + offset;");
        writeln!(text, "{code:<99}").unwrap();
    }
    text
}

/// Lines far wider than a pane, a fifth of them changed in three places.
fn pairs(changed: bool) -> String {
    let mut text = header("changes within long lines");
    for line in 0..150 {
        let edited = changed && line % 5 == 2;
        let mut column = 0;
        while column < 20_000 {
            let word = if edited && [0, 9_984, 19_968].contains(&column) { "变更 edited é" } else { "original" };
            let piece = format!("let c = f({column}, \"{word}\"); ");
            column += 32;
            write!(text, "{piece:<32}").unwrap();
        }
        text.push('\n');
    }
    text
}

/// The reader running in a terminal of its own.
struct Reader {
    child: Child,
    terminal: File,
    output: Vec<u8>,
    chunk: Vec<u8>,
    answered: bool,
}

impl Reader {
    fn start(command: &mut Command) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize { ws_row: ROWS, ws_col: COLUMNS, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: Both descriptor outputs and the size are live, and optional outputs are null.
        let opened =
            unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
        assert_eq!(opened, 0, "{}", io::Error::last_os_error());
        // SAFETY: Successful openpty gives the caller ownership of this descriptor.
        let terminal = unsafe { File::from_raw_fd(master) };
        // SAFETY: The other end is a different descriptor, owned the same way.
        let slave = unsafe { File::from_raw_fd(slave) };
        for fd in [terminal.as_raw_fd(), slave.as_raw_fd()] {
            // SAFETY: The descriptor is open; the reader gets only its end, on its standard streams.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }, 0);
        }
        let child =
            command.stdin(slave.try_clone().unwrap()).stdout(slave.try_clone().unwrap()).stderr(slave).spawn().unwrap();
        Self { child, terminal, output: Vec::new(), chunk: vec![0; 65536], answered: false }
    }

    /// Reads what the reader has sent, waiting until `until` for more.
    fn read(&mut self, until: Instant) -> bool {
        let mut ready = libc::pollfd { fd: self.terminal.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let wait = i32::try_from(left.as_millis()).unwrap();
            // SAFETY: The single live pollfd points to an open descriptor.
            match unsafe { libc::poll(&mut ready, 1, wait) } {
                -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
                1 => break,
                _ => return false,
            }
        }
        let count = loop {
            match self.terminal.read(&mut self.chunk) {
                Ok(count) => break count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                // A terminal whose program has gone has nothing more to say.
                Err(_) => return false,
            }
        };
        self.output.extend_from_slice(&self.chunk[..count]);
        if !self.answered && find(&self.output, ASKED).is_some() {
            self.terminal.write_all(ANSWERS).unwrap();
            self.answered = true;
        }
        count > 0
    }

    /// Waits for a frame to end at or after `from` and returns where it ends.
    fn frame_end(&mut self, from: usize) -> Option<usize> {
        let until = Instant::now() + WAIT;
        loop {
            if let Some(end) = find(&self.output[from..], FRAME_END) {
                return Some(from + end + FRAME_END.len());
            }
            if !self.read(until) {
                return None;
            }
        }
    }

    /// Lets a repaint that follows the terminal's answers finish before timing keys.
    fn settle(&mut self) {
        while self.read(Instant::now() + Duration::from_millis(40)) {}
    }

    /// Presses a key and returns how long the next frame took to arrive.
    fn press(&mut self, key: u8) -> Duration {
        let from = self.output.len();
        let began = Instant::now();
        self.terminal.write_all(&[key]).unwrap();
        let drawn = self.frame_end(from);
        let took = began.elapsed();
        assert!(drawn.is_some(), "no frame after the key {:?}", char::from(key));
        took
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = self.terminal.write_all(b"q");
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && self.child.try_wait().is_ok_and(|status| status.is_none()) {
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find(bytes: &[u8], wanted: &[u8]) -> Option<usize> {
    bytes.windows(wanted.len()).position(|window| window == wanted)
}

/// Whether any text in the frame is magenta, the colour the reader gives
/// keywords. Milo writes each style as `ESC [ 0`, its attributes, then a colour.
fn colours_keywords(frame: &[u8]) -> bool {
    frame.split(|&byte| byte == 0x1b).any(|sequence| {
        let Some(style) = sequence.strip_prefix(b"[0;") else { return false };
        let style = &style[..style.iter().position(|&byte| byte == b'm').unwrap_or(0)];
        style.split(|&byte| byte == b';').find(|code| code.len() != 1) == Some(&b"35"[..])
    })
}

/// One thing timed: a scenario's first frame, or one of its keys.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Measure {
    scenario: usize,
    /// Zero for the first frame, then the scenario's keys in order.
    step: usize,
}

impl Measure {
    fn name(self) -> &'static str {
        self.step.checked_sub(1).map_or("first-paint", |key| SCENARIOS[self.scenario].keys[key].measure.unwrap())
    }

    fn noise(self) -> Duration {
        if self.step == 0 { PAINT_NOISE } else { KEY_NOISE }
    }
}

/// One build's times, and whether its reader coloured each file as code.
#[derive(Default)]
struct Times {
    samples: BTreeMap<Measure, Vec<Duration>>,
    coloured: BTreeMap<usize, bool>,
}

impl Times {
    fn run(&mut self, binary: &Path, project: &Project, index: usize) {
        let scenario = &SCENARIOS[index];
        let mut command = project.command(binary);
        command.args(["view", scenario.file]);
        if scenario.at_commit {
            command.args(["--commit", &project.head]);
        }
        let began = Instant::now();
        let mut reader = Reader::start(&mut command);
        let until = began + WAIT;
        // The frame that holds the mark is the first to show the file.
        let shown = loop {
            let mark = find(&reader.output, MARK.as_bytes());
            if let Some(end) = mark.and_then(|mark| reader.frame_end(mark)) {
                break end;
            }
            if mark.is_some() || !reader.read(until) {
                let seen = String::from_utf8_lossy(&reader.output);
                panic!("{} never showed {}: {seen:?}", binary.display(), scenario.file);
            }
        };
        self.samples.entry(Measure { scenario: index, step: 0 }).or_default().push(began.elapsed());
        let coloured = colours_keywords(&reader.output[..shown]);
        *self.coloured.entry(index).or_insert(true) &= coloured;
        reader.settle();
        for (step, keys) in scenario.keys.iter().enumerate() {
            if keys.measure.is_none() {
                reader.press(keys.key);
                continue;
            }
            let samples = self.samples.entry(Measure { scenario: index, step: step + 1 }).or_default();
            for _ in 0..keys.times {
                samples.push(reader.press(keys.key));
                if let Some(back) = keys.back {
                    reader.press(back);
                }
            }
        }
    }

    fn median(&self, measure: Measure) -> Duration {
        let mut sorted = self.samples[&measure].clone();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    }

    /// The tab-separated columns for one measure: samples, median, least and most.
    fn columns(&self, measure: Measure) -> String {
        let samples = &self.samples[&measure];
        let (min, max) = (samples.iter().min().unwrap(), samples.iter().max().unwrap());
        let syntax = if self.coloured[&measure.scenario] { "yes" } else { "no" };
        let median = self.median(measure).as_micros();
        format!("{}\t{median}\t{}\t{}\t{syntax}", samples.len(), min.as_micros(), max.as_micros())
    }
}

fn version(binary: &Path) -> String {
    let output = Command::new(binary).arg("--version").output().unwrap_or_else(|e| panic!("{}: {e}", binary.display()));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn named(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable).filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// What the environment asks for.
struct Bench {
    binary: PathBuf,
    base: Option<PathBuf>,
    runs: usize,
    max_ratio: Option<f64>,
}

impl Bench {
    fn from_env() -> Self {
        let binary = named("MILO_TEST_BINARY").unwrap_or_else(|| {
            if cfg!(debug_assertions) {
                panic!("time a release build: cargo test --release --test reader_bench -- --ignored");
            }
            env!("CARGO_BIN_EXE_milo").into()
        });
        let number = |variable: &str| std::env::var(variable).ok();
        Self {
            binary,
            base: named("MILO_BENCH_BASE"),
            runs: number("MILO_BENCH_RUNS").map_or(10, |runs| runs.parse().expect("a number of runs")),
            max_ratio: number("MILO_BENCH_MAX_RATIO").map(|ratio| ratio.parse().expect("a ratio such as 1.25")),
        }
    }

    /// Says what was timed, and checks that the big diffs are as near the limit as their names claim.
    fn heading(&self, project: &Project) -> String {
        let Self { binary, base, runs, .. } = self;
        let mut text = format!("# milo reader bench: {COLUMNS}x{ROWS} terminal, {runs} runs, microseconds\n");
        writeln!(text, "# {} {}; timed: {}", std::env::consts::OS, std::env::consts::ARCH, version(binary)).unwrap();
        if let Some(base) = base {
            writeln!(text, "# base: {}", version(base)).unwrap();
        }
        for scenario in SCENARIOS.iter().filter(|scenario| scenario.name.starts_with("near-limit")) {
            let bytes = project.diff_bytes(scenario.file, scenario.at_commit);
            assert!((DIFF_LIMIT * 19 / 20..DIFF_LIMIT).contains(&bytes), "{}: a diff of {bytes} bytes", scenario.name);
            writeln!(text, "# {}: a diff of {bytes} bytes; the reader loads up to {DIFF_LIMIT}", scenario.name)
                .unwrap();
        }
        text
    }

    /// Opens every file `runs` times in each build and returns the timed build's times, then the base's.
    fn time(&self, project: &Project) -> [Times; 2] {
        let mut times = [Times::default(), Times::default()];
        for index in 0..SCENARIOS.len() {
            for run in 0..self.runs {
                // Take turns going first, so neither build always runs on a warmer machine.
                let mut order = [(Some(&self.binary), 0), (self.base.as_ref(), 1)];
                if run % 2 == 1 {
                    order.reverse();
                }
                for (binary, build) in order {
                    if let Some(binary) = binary {
                        times[build].run(binary, project, index);
                    }
                }
            }
        }
        times
    }

    /// The table, and each measure that is slower than the base allows.
    fn table(&self, timed: &Times, based: &Times) -> (String, Vec<String>) {
        let mut slower = Vec::new();
        let mut table = String::from("scenario\tmeasure\tsamples\tmedian\tmin\tmax\tsyntax");
        table.push_str(if self.base.is_some() { "\tbase_median\tbase_syntax\tratio\n" } else { "\n" });
        for &measure in timed.samples.keys() {
            let (scenario, name) = (SCENARIOS[measure.scenario].name, measure.name());
            write!(table, "{scenario}\t{name}\t{}", timed.columns(measure)).unwrap();
            if self.base.is_some() {
                let (median, before) = (timed.median(measure), based.median(measure));
                let ratio = median.as_secs_f64() / before.as_secs_f64();
                let syntax = if based.coloured[&measure.scenario] { "yes" } else { "no" };
                write!(table, "\t{}\t{syntax}\t{ratio:.2}", before.as_micros()).unwrap();
                if self.max_ratio.is_some_and(|max| ratio > max) && median > before + measure.noise() {
                    slower.push(format!("{scenario} {name}: {median:?} against {before:?}"));
                }
            }
            table.push('\n');
        }
        (table, slower)
    }
}

#[test]
#[ignore = "a benchmark; run it alone on a release build"]
fn reader_first_paint_and_scrolling() {
    let bench = Bench::from_env();
    let project = Project::new();
    let heading = bench.heading(&project);
    let [timed, based] = bench.time(&project);
    let (table, slower) = bench.table(&timed, &based);
    let report = heading + &table;
    print!("{report}");
    if let Some(path) = named("MILO_BENCH_REPORT") {
        fs::write(path, &report).unwrap();
    }
    assert!(slower.is_empty(), "slower than the base allows: {slower:#?}");
}
