//! Runs an installed formatter over text the user is editing.
//!
//! The text goes in on standard input and the formatted text comes back on
//! standard output: the file on disk is not given to the formatter to write.
//! A formatter runs only when the user asks, never when a file is opened. It
//! is found by a fixed name in the folders of `PATH` that are named in full,
//! so a project cannot choose the program, and nothing is installed or fetched.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::term;

/// A formatter that has not finished in this long is stopped.
const WAIT: Duration = Duration::from_secs(10);
/// More formatted text than this cannot be a file milo shows.
const OUTPUT_LIMIT: usize = 2 << 20;
const ERROR_LIMIT: usize = 4096;
/// The longest part of a formatter's complaint that is shown.
const MESSAGE_WIDTH: usize = 100;

/// One word of a formatter's command line.
enum Arg {
    Text(&'static str),
    /// The file's name as `./name`, so a name cannot be read as an option.
    Name,
    /// The file's name straight after a prefix such as `--assume-filename=`.
    Named(&'static str),
    /// The file's extension, for a formatter that is told the language.
    Extension,
    /// `--edition` and the Rust edition of the package, which rustfmt does not look up for text on standard input.
    Edition,
}

/// A program that reads text on standard input and writes it formatted to standard output.
pub struct Tool {
    pub name: &'static str,
    args: &'static [Arg],
}

use Arg::{Edition, Extension, Name, Named, Text};

const RUSTFMT: Tool = Tool { name: "rustfmt", args: &[Edition, Text("--emit"), Text("stdout")] };
const GOFMT: Tool = Tool { name: "gofmt", args: &[] };
// Ruff keeps no cache for text on standard input, and is told not to start one in the project.
const RUFF: Tool =
    Tool { name: "ruff", args: &[Text("format"), Text("--no-cache"), Text("--stdin-filename"), Name, Text("-")] };
const BLACK: Tool = Tool { name: "black", args: &[Text("--quiet"), Text("--stdin-filename"), Name, Text("-")] };
const PRETTIER: Tool = Tool { name: "prettier", args: &[Text("--stdin-filepath"), Name] };
const BIOME: Tool = Tool { name: "biome", args: &[Text("format"), Text("--stdin-file-path"), Name] };
const DENO: Tool = Tool { name: "deno", args: &[Text("fmt"), Text("--ext"), Extension, Text("-")] };
const CLANG_FORMAT: Tool = Tool { name: "clang-format", args: &[Named("--assume-filename=")] };
const SHFMT: Tool = Tool { name: "shfmt", args: &[Text("--filename"), Name] };
const STYLUA: Tool = Tool { name: "stylua", args: &[Text("--stdin-filepath"), Name, Text("-")] };
const TAPLO: Tool = Tool { name: "taplo", args: &[Text("format"), Text("--stdin-filepath"), Name, Text("-")] };
const SWIFT_FORMAT: Tool =
    Tool { name: "swift-format", args: &[Text("format"), Text("--assume-filename"), Name, Text("-")] };
const TERRAFORM: Tool = Tool { name: "terraform", args: &[Text("fmt"), Text("-no-color"), Text("-")] };

/// The formatters for a kind of file, tried in order, and their names for a sentence.
struct Kind {
    extensions: &'static [&'static str],
    tools: &'static [Tool],
    /// The tools as they are named when none is installed.
    names: &'static str,
}

const WEB: &[Tool] = &[PRETTIER, BIOME, DENO];
const WEB_NAMES: &str = "prettier, biome or deno";
const PROSE: &[Tool] = &[PRETTIER, DENO];
const PROSE_NAMES: &str = "prettier or deno";

/// What milo can format. `docs/panes.md` lists the same table for users.
const KINDS: &[Kind] = &[
    Kind { extensions: &["rs"], tools: &[RUSTFMT], names: "rustfmt" },
    Kind { extensions: &["go"], tools: &[GOFMT], names: "gofmt" },
    Kind { extensions: &["py", "pyi"], tools: &[RUFF, BLACK], names: "ruff or black" },
    Kind { extensions: &["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx"], tools: WEB, names: WEB_NAMES },
    Kind { extensions: &["json", "jsonc", "css"], tools: WEB, names: WEB_NAMES },
    Kind { extensions: &["scss", "less", "html", "md", "yaml", "yml"], tools: PROSE, names: PROSE_NAMES },
    Kind { extensions: &["vue"], tools: &[PRETTIER], names: "prettier" },
    Kind { extensions: &["toml"], tools: &[TAPLO], names: "taplo" },
    Kind { extensions: &["c", "h", "cc", "cpp", "cxx", "hpp", "hh"], tools: &[CLANG_FORMAT], names: "clang-format" },
    Kind { extensions: &["sh", "bash"], tools: &[SHFMT], names: "shfmt" },
    Kind { extensions: &["lua"], tools: &[STYLUA], names: "stylua" },
    Kind { extensions: &["swift"], tools: &[SWIFT_FORMAT], names: "swift-format" },
    Kind { extensions: &["tf", "tfvars"], tools: &[TERRAFORM], names: "terraform" },
];

fn extension(name: &str) -> Option<&str> {
    name.rsplit_once('.').map(|(_, extension)| extension).filter(|extension| !extension.is_empty())
}

fn kind_of(name: &str) -> Option<&'static Kind> {
    let extension = extension(name)?;
    KINDS.iter().find(|kind| kind.extensions.iter().any(|known| extension.eq_ignore_ascii_case(known)))
}

/// Whether text in a file of this name can be formatted here.
pub enum Support {
    /// This formatter is installed and is the one that will run.
    Ready(&'static Tool, PathBuf),
    /// A formatter is known for the file, but none of these is installed.
    Missing(&'static str),
    /// Milo knows no formatter for this kind of file.
    Unknown,
}

/// Looks for a formatter for the file in the folders of `path`, as `PATH` lists them.
pub fn support(name: &str, path: Option<&OsStr>) -> Support {
    let Some(kind) = kind_of(name) else { return Support::Unknown };
    kind.tools
        .iter()
        .find_map(|tool| Some(Support::Ready(tool, find(tool.name, path?)?)))
        .unwrap_or(Support::Missing(kind.names))
}

/// Whether milo knows a formatter for a file of this name, installed or not.
pub fn known(name: &str) -> bool {
    kind_of(name).is_some()
}

/// The first program of this name in a folder that `PATH` names in full. A
/// folder named from wherever the command happens to run, such as `.` or
/// `node_modules/.bin`, would let the project being read choose the program.
fn find(program: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path).filter(|dir| dir.is_absolute()).map(|dir| dir.join(program)).find(|file| {
        let Ok(name) = std::ffi::CString::new(file.as_os_str().as_bytes()) else { return false };
        // SAFETY: The path is a live NUL-terminated string; access only reports on it.
        file.is_file() && unsafe { libc::access(name.as_ptr(), libc::X_OK) } == 0
    })
}

/// The text to format and the file it belongs to.
pub struct Draft<'a> {
    pub text: &'a str,
    /// The file's own name, which tells a formatter the language and which settings apply.
    pub name: &'a str,
    /// The folder the file is in, where the formatter runs so that it finds the project's settings.
    pub dir: &'a Path,
    /// The project's folder, above which nothing is looked for.
    pub root: &'a Path,
}

/// Why the text was left as it was.
#[derive(Debug)]
pub enum Failure {
    TimedOut,
    /// The formatter's own words, or none if it gave none.
    Refused(String),
    /// The formatter reported success without giving text milo can show.
    NoText,
    NotStarted,
}

impl Failure {
    /// A sentence for the user, short enough for one row.
    pub fn sentence(&self, tool: &str) -> String {
        match self {
            Self::TimedOut => format!("{tool} took too long and was stopped."),
            Self::Refused(said) if said.is_empty() => format!("{tool} could not format this."),
            Self::Refused(said) => format!("{tool}: {said}"),
            Self::NoText => format!("{tool} gave back nothing usable."),
            Self::NotStarted => format!("{tool} could not be started."),
        }
    }
}

/// The edition of the nearest package at or above `dir`, unless rustfmt's own settings there name one.
fn rust_edition(dir: &Path, root: &Path) -> Option<String> {
    for dir in dir.ancestors().take_while(|dir| dir.starts_with(root)) {
        let set = |name: &str| read_edition(&dir.join(name)).is_some();
        if set("rustfmt.toml") || set(".rustfmt.toml") {
            return None;
        }
        if let Some(edition) = read_edition(&dir.join("Cargo.toml")) {
            return Some(edition);
        }
    }
    Some("2021".to_owned())
}

/// The value of the first `edition = "…"` line of a small settings file.
fn read_edition(file: &Path) -> Option<String> {
    let crate::read::Content::Bytes(bytes) = crate::read::file(file, 1 << 20) else { return None };
    let text = String::from_utf8(bytes).ok()?;
    text.lines().find_map(|line| {
        let value = line.trim_start().strip_prefix("edition")?.trim_start().strip_prefix('=')?.trim_start();
        let (year, _) = value.strip_prefix('"')?.split_once('"')?;
        (year.len() == 4 && year.bytes().all(|c| c.is_ascii_digit())).then(|| year.to_owned())
    })
}

fn arguments(tool: &Tool, draft: &Draft<'_>) -> Vec<OsString> {
    let mut args = Vec::new();
    for arg in tool.args {
        match arg {
            Text(text) => args.push(text.into()),
            Name => args.push(format!("./{}", draft.name).into()),
            Named(prefix) => args.push(format!("{prefix}{}", draft.name).into()),
            Extension => args.push(extension(draft.name).unwrap_or_default().to_ascii_lowercase().into()),
            Edition => {
                if let Some(edition) = rust_edition(draft.dir, draft.root) {
                    args.extend(["--edition".into(), edition.into()]);
                }
            }
        }
    }
    args
}

/// Reads to the end so the writer is never left waiting, keeping at most `limit` bytes.
fn read_capped(mut pipe: impl Read, limit: usize) -> Option<Vec<u8>> {
    let mut kept = Vec::new();
    let mut chunk = [0; 1 << 14];
    let mut whole = true;
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) if whole && kept.len() + read <= limit => kept.extend_from_slice(&chunk[..read]),
            Ok(_) => whole = false,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    whole.then_some(kept)
}

/// The first line a formatter wrote about its failure, without anything that could move the cursor.
fn complaint(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    line.chars().filter(|c| !c.is_control()).take(MESSAGE_WIDTH).collect()
}

/// Formats the draft with a program found by `support`. The draft itself is never changed here.
pub fn run(tool: &Tool, program: &Path, draft: &Draft<'_>) -> Result<String, Failure> {
    run_within(tool, program, draft, WAIT)
}

fn run_within(tool: &Tool, program: &Path, draft: &Draft<'_>, wait: Duration) -> Result<String, Failure> {
    let deadline = Instant::now() + wait;
    let mut child = Command::new(program)
        .args(arguments(tool, draft))
        .current_dir(draft.dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A group of its own, so that helpers it starts can be stopped with it.
        .process_group(0)
        .spawn()
        .map_err(|_| Failure::NotStarted)?;
    let (mut stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (status, out, err) = std::thread::scope(|scope| {
        // Written from its own thread: a formatter may answer before it has read everything.
        scope.spawn(|| stdin.take().map(|mut pipe| pipe.write_all(draft.text.as_bytes())));
        let out = scope.spawn(|| stdout.and_then(|pipe| read_capped(pipe, OUTPUT_LIMIT)));
        let err = scope.spawn(|| stderr.and_then(|pipe| read_capped(pipe, ERROR_LIMIT)));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline && !term::TERMINATED.load(Ordering::Relaxed) => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                _ => break None,
            }
        };
        if status.is_none() {
            // SAFETY: The child leads a process group of its own, so this reaches only it and what it started.
            unsafe { libc::killpg(libc::pid_t::try_from(child.id()).unwrap_or(0).max(1), libc::SIGKILL) };
            let _ = child.wait();
        }
        (status, out.join().ok().flatten(), err.join().ok().flatten())
    });
    let status = status.ok_or(Failure::TimedOut)?;
    if !status.success() {
        return Err(Failure::Refused(complaint(&err.unwrap_or_default())));
    }
    let text = out.and_then(|bytes| String::from_utf8(bytes).ok()).ok_or(Failure::NoText)?;
    // An empty answer to text that was not empty is a formatter that printed elsewhere.
    if text.contains('\0') || text.trim().is_empty() != draft.text.trim().is_empty() {
        return Err(Failure::NoText);
    }
    Ok(text)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::fixture::Fixture;

    /// Puts a stand-in for a formatter in a folder of its own and waits until it can be run.
    pub(crate) fn stand_in(dir: &Path, name: &str, script: &str) {
        fs::create_dir_all(dir).unwrap();
        let file = dir.join(name);
        // The check below that the file can be run must not wait for what the stand-in does.
        fs::write(&file, format!("#!/bin/sh\n[ -n \"$MILO_STAND_IN_PROBE\" ] && exit 0\n{script}\n")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
        // Another test's new process can hold the file open for writing for a moment.
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            match Command::new(&file).env("MILO_STAND_IN_PROBE", "1").stdin(Stdio::null()).output() {
                Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                started => {
                    started.unwrap();
                    break;
                }
            }
        }
    }

    fn draft<'a>(f: &'a Fixture, name: &'a str, text: &'a str) -> Draft<'a> {
        Draft { text, name, dir: &f.root, root: &f.root }
    }

    fn format(f: &Fixture, bin: &Path, name: &str, text: &str) -> Result<String, String> {
        match support(name, Some(bin.as_os_str())) {
            Support::Ready(tool, program) => {
                run(tool, &program, &draft(f, name, text)).map_err(|failure| failure.sentence(tool.name))
            }
            Support::Missing(names) => Err(format!("missing {names}")),
            Support::Unknown => Err("unknown".to_owned()),
        }
    }

    #[test]
    fn every_formatter_name_fits_a_row_of_the_help_page() {
        // A help row is 26 columns: two of margin, then `Formatter: ` or the names alone.
        for kind in KINDS {
            assert!(kind.names.len() <= 24, "{}", kind.names);
            for tool in kind.tools {
                assert!(tool.name.len() <= 13, "{}", tool.name);
                assert!(kind.names.contains(tool.name), "{} is not named in {:?}", tool.name, kind.names);
            }
        }
    }

    #[test]
    fn the_first_installed_formatter_for_the_kind_of_file_is_the_one_that_runs() {
        let f = Fixture::folder();
        let bin = f.root.join("bin");
        assert_eq!(format(&f, &bin, "a.py", "x").unwrap_err(), "missing ruff or black");
        assert_eq!(format(&f, &bin, "notes.txt", "x").unwrap_err(), "unknown");
        assert_eq!(format(&f, &bin, "Makefile", "x").unwrap_err(), "unknown");
        assert!(known("A.PY") && !known("a.py.orig") && !known("py"));

        stand_in(&bin, "black", "echo black \"$@\"; cat");
        assert_eq!(format(&f, &bin, "a.py", "x\n").unwrap(), "black --quiet --stdin-filename ./a.py -\nx\n");
        stand_in(&bin, "ruff", "echo ruff \"$@\"; cat");
        assert_eq!(format(&f, &bin, "a.py", "x\n").unwrap(), "ruff format --no-cache --stdin-filename ./a.py -\nx\n");
        // A name that looks like an option is still a file name to the formatter.
        assert!(format(&f, &bin, "--version.py", "x\n").unwrap().contains("--stdin-filename ./--version.py -"));
    }

    #[test]
    fn a_program_is_run_only_from_a_folder_named_in_full() {
        let f = Fixture::folder();
        stand_in(&f.root.join("bin"), "gofmt", "cat");
        stand_in(&f.root, "gofmt", "cat");
        for path in ["", ".", "bin", "./bin:.", "node_modules/.bin"] {
            assert!(matches!(support("a.go", Some(OsStr::new(path))), Support::Missing("gofmt")), "{path}");
        }
        assert!(matches!(support("a.go", None), Support::Missing("gofmt")));
        let mixed = std::env::join_paths([Path::new("bin"), &f.root.join("missing"), &f.root.join("bin")]).unwrap();
        let Support::Ready(tool, program) = support("a.go", Some(&mixed)) else { panic!("not found") };
        assert_eq!((tool.name, program), ("gofmt", f.root.join("bin/gofmt")));

        // A file that cannot be run, and a folder of the same name, are not programs.
        fs::set_permissions(f.root.join("bin/gofmt"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir_all(f.root.join("dirs/gofmt")).unwrap();
        let path = std::env::join_paths([f.root.join("bin"), f.root.join("dirs")]).unwrap();
        assert!(matches!(support("a.go", Some(&path)), Support::Missing(_)));
    }

    #[test]
    fn a_formatter_runs_in_the_folder_of_the_file_and_is_given_the_text_not_the_file() {
        let f = Fixture::folder();
        let bin = f.root.join("bin");
        fs::create_dir_all(f.root.join("src/deep")).unwrap();
        f.write("src/deep/a.go", "on disk\n");
        stand_in(&bin, "gofmt", "pwd; echo \"[$NO_COLOR]\"; tr a-z A-Z");
        let Support::Ready(tool, program) = support("a.go", Some(bin.as_os_str())) else { panic!("not found") };
        let dir = f.root.join("src/deep");
        let text = run(tool, &program, &Draft { text: "draft\n", name: "a.go", dir: &dir, root: &f.root }).unwrap();
        assert_eq!(text, format!("{}\n[1]\nDRAFT\n", dir.display()));
        assert_eq!(fs::read_to_string(dir.join("a.go")).unwrap(), "on disk\n");
    }

    #[test]
    fn a_failed_formatter_reports_its_first_line_without_control_characters() {
        let f = Fixture::folder();
        let bin = f.root.join("bin");
        stand_in(
            &bin,
            "gofmt",
            "cat >/dev/null; printf '\\n  \\033[31mline 3: unexpected }\\033[0m\\nmore\\n' >&2; exit 2",
        );
        assert_eq!(format(&f, &bin, "a.go", "x").unwrap_err(), "gofmt: [31mline 3: unexpected }[0m");
        stand_in(&bin, "gofmt", "cat >/dev/null; exit 1");
        assert_eq!(format(&f, &bin, "a.go", "x").unwrap_err(), "gofmt could not format this.");
        // Success with nothing to show for it, or with bytes that are not text, changes nothing either.
        stand_in(&bin, "gofmt", "cat >/dev/null");
        assert_eq!(format(&f, &bin, "a.go", "x").unwrap_err(), "gofmt gave back nothing usable.");
        stand_in(&bin, "gofmt", "cat >/dev/null; printf 'a\\377b'");
        assert_eq!(format(&f, &bin, "a.go", "x").unwrap_err(), "gofmt gave back nothing usable.");
        stand_in(&bin, "gofmt", "cat >/dev/null; printf 'a\\000b'");
        assert_eq!(format(&f, &bin, "a.go", "x").unwrap_err(), "gofmt gave back nothing usable.");
        // An empty draft may come back empty.
        stand_in(&bin, "gofmt", "cat");
        assert_eq!(format(&f, &bin, "a.go", "").unwrap(), "");
    }

    #[test]
    fn a_formatter_that_writes_before_it_reads_or_says_too_much_does_not_stall() {
        let f = Fixture::folder();
        let bin = f.root.join("bin");
        let big = "x".repeat(1 << 20);
        // The whole draft is echoed before any of it is read from the pipe.
        stand_in(&bin, "gofmt", "head -c 1048576 /dev/zero | tr '\\0' y; cat >/dev/null");
        assert_eq!(format(&f, &bin, "a.go", &big).unwrap().len(), 1 << 20);
        stand_in(&bin, "gofmt", "cat >/dev/null; head -c 4194304 /dev/zero | tr '\\0' y");
        assert_eq!(format(&f, &bin, "a.go", "x").unwrap_err(), "gofmt gave back nothing usable.");
        stand_in(&bin, "gofmt", "cat >/dev/null; head -c 1048576 /dev/zero | tr '\\0' e >&2; exit 1");
        assert_eq!(format(&f, &bin, "a.go", "x").unwrap_err(), "gofmt could not format this.");
    }

    #[test]
    fn rustfmt_is_told_the_edition_of_the_package_unless_its_own_settings_say() {
        let f = Fixture::folder();
        let bin = f.root.parent().unwrap().join("bin");
        stand_in(&bin, "rustfmt", "echo \"$@\"; cat >/dev/null");
        let dir = f.root.join("crates/app/src");
        fs::create_dir_all(&dir).unwrap();
        let args = |dir: &Path| {
            let Support::Ready(tool, program) = support("main.rs", Some(bin.as_os_str())) else { panic!("not found") };
            run(tool, &program, &Draft { text: "x", name: "main.rs", dir, root: &f.root }).unwrap()
        };
        assert_eq!(args(&dir), "--edition 2021 --emit stdout\n");
        // Settings above the project are not the project's.
        fs::write(f.root.parent().unwrap().join("Cargo.toml"), "edition = \"2015\"\n").unwrap();
        assert_eq!(args(&dir), "--edition 2021 --emit stdout\n");
        f.write("Cargo.toml", "[workspace]\n[workspace.package]\nedition = \"2018\"\n");
        assert_eq!(args(&dir), "--edition 2018 --emit stdout\n");
        f.write("crates/app/Cargo.toml", "[package]\nname = \"app\"\n  edition=\"2024\" # new\n");
        assert_eq!(args(&dir), "--edition 2024 --emit stdout\n");
        assert_eq!(args(&f.root), "--edition 2018 --emit stdout\n");
        f.write("crates/app/src/rustfmt.toml", "edition = \"2021\"\n");
        assert_eq!(args(&dir), "--emit stdout\n");
    }

    #[test]
    fn a_formatter_that_does_not_finish_is_stopped_with_what_it_started() {
        let f = Fixture::folder();
        let bin = f.root.join("bin");
        // The helper keeps the pipes open, so stopping only the formatter would wait for it.
        stand_in(&bin, "gofmt", "sleep 600 &\necho $! > helper\nsleep 600");
        let Support::Ready(tool, program) = support("a.go", Some(bin.as_os_str())) else { panic!("not found") };
        let started = Instant::now();
        let failure = run_within(tool, &program, &draft(&f, "a.go", "x"), Duration::from_millis(300)).err().unwrap();
        assert_eq!(failure.sentence(tool.name), "gofmt took too long and was stopped.");
        assert!(started.elapsed() < Duration::from_secs(30));
        let helper: libc::pid_t = fs::read_to_string(f.root.join("helper")).unwrap().trim().parse().unwrap();
        let gone = (0..500).any(|_| {
            std::thread::sleep(Duration::from_millis(10));
            // SAFETY: Signal 0 only asks whether the process still exists.
            unsafe { libc::kill(helper, 0) != 0 }
        });
        assert!(gone, "the helper outlived its formatter");
    }
}
