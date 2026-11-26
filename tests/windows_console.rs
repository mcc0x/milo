#![cfg(windows)]
//! Drives the built Milo executable through a Windows pseudoconsole, the way Windows Terminal
//! runs it: open a working text file, edit and save it, refuse to write over a change another
//! program made, quit, and leave the console's modes as they were. The Home, Outline, History and
//! Usage panes open the same way, draw what they should, move on a key and quit with exit code 0,
//! and the Reader's own Markdown view renders a working file and follows a save made elsewhere.

use std::ffi::{OsStr, c_void};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Handle = *mut c_void;

#[repr(C)]
struct StartupInfoEx {
    size: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    numbers: [u32; 7],
    flags: u32,
    show_window: u16,
    reserved2_size: u16,
    reserved2: *mut u8,
    std_input: Handle,
    std_output: Handle,
    std_error: Handle,
    attributes: *mut c_void,
}

#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const INVALID_HANDLE_VALUE: Handle = usize::MAX as Handle;
const WAIT_OBJECT_0: u32 = 0;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreatePseudoConsole(size: u32, input: Handle, output: Handle, flags: u32, console: *mut Handle) -> i32;
    fn ClosePseudoConsole(console: Handle);
    fn InitializeProcThreadAttributeList(list: *mut c_void, count: u32, flags: u32, size: *mut usize) -> i32;
    fn UpdateProcThreadAttribute(
        list: *mut c_void,
        flags: u32,
        attribute: usize,
        value: *const c_void,
        size: usize,
        previous: *mut c_void,
        returned: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: *mut c_void);
    fn CreateProcessW(
        application: *const u16,
        command: *mut u16,
        process_attributes: *const c_void,
        thread_attributes: *const c_void,
        inherit: i32,
        flags: u32,
        environment: *const c_void,
        directory: *const u16,
        startup: *const StartupInfoEx,
        information: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

/// A pseudoconsole running one command line, with everything it draws collected.
struct Console {
    console: Handle,
    input: io::PipeWriter,
    screen: Arc<Mutex<Vec<u8>>>,
    process: OwnedHandle,
    _thread: OwnedHandle,
}

impl Console {
    fn start(command: &str, directory: &Path, environment: &[(String, PathBuf)]) -> Self {
        let (console_input, input) = io::pipe().unwrap();
        let (mut output, console_output) = io::pipe().unwrap();
        let mut console = std::ptr::null_mut();
        let size = 100u32 | (30u32 << 16);
        // SAFETY: Both pipe ends are open for the call; the console duplicates what it keeps.
        let created = unsafe {
            CreatePseudoConsole(
                size,
                console_input.as_raw_handle(),
                console_output.as_raw_handle(),
                0,
                &raw mut console,
            )
        };
        assert_eq!(created, 0, "CreatePseudoConsole failed: {created:#x}");
        drop((console_input, console_output));
        let screen = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&screen);
        // The console stops while its output is full, so it is read all the time.
        std::thread::spawn(move || {
            let mut chunk = [0; 4096];
            while let Ok(n) = output.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                collected.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });

        let mut list_size = 0;
        // SAFETY: A null list asks only for the size one attribute needs.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut list_size) };
        let mut list = vec![0u64; list_size.div_ceil(8)];
        // SAFETY: The buffer is at least the size asked for and lives until the process starts.
        assert_ne!(unsafe { InitializeProcThreadAttributeList(list.as_mut_ptr().cast(), 1, 0, &raw mut list_size) }, 0);
        // SAFETY: The attribute's value is the pseudoconsole handle itself, which outlives the list.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                list.as_mut_ptr().cast(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                console,
                size_of::<Handle>(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_ne!(updated, 0, "{}", io::Error::last_os_error());

        let mut block = Vec::new();
        for (name, value) in std::env::vars_os().filter(|(name, _)| {
            ![
                "HOME",
                "USERPROFILE",
                "APPDATA",
                "LOCALAPPDATA",
                "XDG_CONFIG_HOME",
                "XDG_STATE_HOME",
                "XDG_DATA_HOME",
                "CODEX_HOME",
                "CLAUDE_CONFIG_DIR",
                "PI_CODING_AGENT_DIR",
                "OPENCODE_DB",
                "MILO_DEEPSEEK_API_KEY",
                "MILO_OPENROUTER_API_KEY",
                "MILO_DEVIN_API_KEY",
                "MILO_DEVIN_ORG",
                "MILO_GITHUB_TOKEN",
                "MILO_GITHUB_USER",
                "TERM",
            ]
            .iter()
            .any(|isolated| name.eq_ignore_ascii_case(isolated))
        }) {
            block.extend(name.encode_wide().chain("=".encode_utf16()).chain(value.encode_wide()).chain([0]));
        }
        for (name, value) in environment {
            block.extend(
                name.encode_utf16().chain("=".encode_utf16()).chain(value.as_os_str().encode_wide()).chain([0]),
            );
        }
        block.push(0);

        let startup = StartupInfoEx {
            size: u32::try_from(size_of::<StartupInfoEx>()).unwrap(),
            reserved: std::ptr::null_mut(),
            desktop: std::ptr::null_mut(),
            title: std::ptr::null_mut(),
            numbers: [0; 7],
            // Without explicit handles the child takes this test's own redirected output instead
            // of the pseudoconsole's, so Milo would not see a terminal. Invalid handles make it
            // use the console it is attached to.
            flags: STARTF_USESTDHANDLES,
            show_window: 0,
            reserved2_size: 0,
            reserved2: std::ptr::null_mut(),
            std_input: INVALID_HANDLE_VALUE,
            std_output: INVALID_HANDLE_VALUE,
            std_error: INVALID_HANDLE_VALUE,
            attributes: list.as_mut_ptr().cast(),
        };
        let mut command = wide(OsStr::new(command));
        let directory = wide(directory.as_os_str());
        let mut information = ProcessInformation {
            process: std::ptr::null_mut(),
            thread: std::ptr::null_mut(),
            process_id: 0,
            thread_id: 0,
        };
        // SAFETY: Every buffer lives for the call, the command line is writable as required, and
        // the environment block is UTF-16 and double-terminated.
        let started = unsafe {
            CreateProcessW(
                std::ptr::null(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                block.as_ptr().cast(),
                directory.as_ptr(),
                &raw const startup,
                &raw mut information,
            )
        };
        assert_ne!(started, 0, "CreateProcessW failed: {}", io::Error::last_os_error());
        // SAFETY: The list was initialized above and the process no longer needs it.
        unsafe { DeleteProcThreadAttributeList(list.as_mut_ptr().cast()) };
        Self {
            console,
            input,
            screen,
            // SAFETY: CreateProcessW returned these handles for this caller to close.
            process: unsafe { OwnedHandle::from_raw_handle(information.process) },
            // SAFETY: As above.
            _thread: unsafe { OwnedHandle::from_raw_handle(information.thread) },
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()).into_owned()
    }

    /// Waits until the console has drawn `expected` since `from`, a length of `text()`.
    fn see(&self, expected: &str, from: usize) -> usize {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let text = self.text();
            if let Some(at) = text.get(from..).and_then(|tail| tail.find(expected)) {
                return from + at + expected.len();
            }
            assert!(Instant::now() < deadline, "missing {expected:?} after byte {from}; the console drew:\n{text}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn keys(&mut self, keys: &[u8]) {
        self.input.write_all(keys).unwrap();
        self.input.flush().unwrap();
        // Separate keys arrive as separate reads, as typing does.
        std::thread::sleep(Duration::from_millis(200));
    }

    fn exit_code(&self, within: Duration) -> Option<u32> {
        let millis = u32::try_from(within.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: The process handle is open.
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), millis) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0;
        // SAFETY: The handle is open and the output is writable.
        assert_ne!(unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &raw mut code) }, 0);
        Some(code)
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        // SAFETY: The pseudoconsole was created by this value and is closed once.
        unsafe { ClosePseudoConsole(self.console) };
    }
}

/// A folder per test, removed afterwards.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("milo-windows-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("project")).unwrap();
        fs::create_dir_all(path.join("profile")).unwrap();
        Self(path)
    }

    fn environment(&self) -> Vec<(String, PathBuf)> {
        let profile = self.0.join("profile");
        vec![
            ("USERPROFILE".into(), profile.clone()),
            ("APPDATA".into(), profile.join("Roaming")),
            ("LOCALAPPDATA".into(), profile.join("Local")),
        ]
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Prints the console's input and output modes for the test below. Run only by that test,
/// inside its pseudoconsole, through this test binary.
#[test]
#[ignore = "run by edits_save_and_refuse_an_external_change_then_restore_the_console"]
fn print_console_modes() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleMode(console: Handle, mode: *mut u32) -> i32;
    }
    let mode = |name: &str| {
        let file = fs::OpenOptions::new().read(true).write(true).open(name).unwrap();
        let mut mode = 0;
        // SAFETY: The handle is open and the output is writable.
        assert_ne!(unsafe { GetConsoleMode(file.as_raw_handle(), &raw mut mode) }, 0);
        mode
    };
    let when = std::env::var("MILO_CONSOLE_PROBE").unwrap();
    println!("console-modes {when}={:#x}/{:#x}.", mode("CONIN$"), mode("CONOUT$"));
}

#[test]
fn edits_save_and_refuse_an_external_change_then_restore_the_console() {
    let scratch = Scratch::new("edit");
    let file = scratch.0.join("project").join("notes.txt");
    let original = "\u{feff}héllo wörld\r\nsecond line ✓\r\n";
    fs::write(&file, original).unwrap();
    let milo = env!("CARGO_BIN_EXE_milo");
    let probe = std::env::current_exe().unwrap();
    let probe = format!("\"{}\" print_console_modes --exact --ignored --nocapture --quiet", probe.display());
    let command = format!(
        "cmd.exe /d /s /c \"set MILO_CONSOLE_PROBE=before& {probe} & \"{milo}\" view notes.txt --edit & set MILO_CONSOLE_PROBE=after& {probe}\""
    );
    let mut console = Console::start(&command, &scratch.0.join("project"), &scratch.environment());

    let at = console.see("console-modes before=", 0);
    let at = console.see("second line ✓", at);
    console.see("^S save", at);

    // Typing goes into the draft; only Ctrl-S writes it, in place, keeping the mark and line ends.
    console.keys(b"X");
    assert_eq!(fs::read_to_string(&file).unwrap(), original, "typing must not write the file");
    console.keys(b"\x13");
    let saved = "\u{feff}Xhéllo wörld\r\nsecond line ✓\r\n";
    let deadline = Instant::now() + Duration::from_secs(30);
    while fs::read_to_string(&file).unwrap() != saved {
        assert!(Instant::now() < deadline, "the edit was not saved: {:?}", fs::read(&file).unwrap());
        std::thread::sleep(Duration::from_millis(50));
    }

    // Another program changes the file. The next save must ask, not write.
    let external = "written elsewhere\r\n";
    fs::write(&file, external).unwrap();
    let mark = console.text().len();
    console.keys(b"Y");
    console.keys(b"\x13");
    console.see("Changed on disk", mark);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(fs::read_to_string(&file).unwrap(), external, "an external change was overwritten");

    // Back out of the question, stop editing without saving, and quit.
    console.keys(b"\x1b");
    console.keys(b"\x1b");
    console.see("Unsaved edits", mark);
    console.keys(b"d");
    console.keys(b"q");
    let ended = console.see("console-modes after=", mark);
    console.see(".", ended);
    assert_eq!(console.exit_code(Duration::from_secs(30)), Some(0), "the console drew:\n{}", console.text());
    assert_eq!(fs::read_to_string(&file).unwrap(), external);

    // The pseudoconsole repaints lines it already drew, so each report is found by its label.
    let text = console.text();
    let modes = |when: &str| {
        let label = format!("console-modes {when}=");
        let at = text.find(&label).unwrap() + label.len();
        text[at..].split('.').next().unwrap().to_owned()
    };
    assert_eq!(modes("before"), modes("after"), "Milo left the console in different modes");
}

const DOWN: &[u8] = b"\x1b[B";
const ENTER: &[u8] = b"\r";

fn git(project: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["-c", "user.name=Milo Test", "-c", "user.email=milo@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// A repository with two commits: README.md, docs/guide.md and src/lib.rs, then a change to the guide.
fn repository(scratch: &Scratch) {
    let project = scratch.0.join("project");
    git(&project, &["init", "-q", "--template=", "-b", "main"]);
    fs::create_dir_all(project.join("docs")).unwrap();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("README.md"), "# Notes\n").unwrap();
    fs::write(project.join("docs").join("guide.md"), "guide\n").unwrap();
    fs::write(project.join("src").join("lib.rs"), "fn main() {}\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-q", "-m", "Add the project files"]);
    fs::write(project.join("docs").join("guide.md"), "guide\nmore\n").unwrap();
    git(&project, &["commit", "-q", "-a", "-m", "Extend the guide"]);
}

/// Runs Milo with the arguments in the project folder, between the console mode reports, and
/// reports Milo's own exit code. Returns the console and where the first report ended.
fn open_pane(scratch: &Scratch, arguments: &str) -> (Console, usize) {
    let milo = env!("CARGO_BIN_EXE_milo");
    let probe = std::env::current_exe().unwrap();
    let probe = format!("\"{}\" print_console_modes --exact --ignored --nocapture --quiet", probe.display());
    let command = format!(
        "cmd.exe /d /v:on /s /c \"set MILO_CONSOLE_PROBE=before& {probe} & \"{milo}\" {arguments}& echo milo-exit=!ERRORLEVEL!& set MILO_CONSOLE_PROBE=after& {probe}\""
    );
    let console = Console::start(&command, &scratch.0.join("project"), &scratch.environment());
    let at = console.see("console-modes before=", 0);
    (console, at)
}

/// After the pane quits: Milo exited with 0, and the console is in the modes it started in.
fn finish(console: &Console, from: usize) {
    let exit = console.see("milo-exit=", from);
    let ended = console.see("console-modes after=", exit);
    console.see(".", ended);
    let text = console.text();
    assert_eq!(&text[exit..exit + 1], "0", "Milo exited with another code; the console drew:\n{text}");
    assert_eq!(console.exit_code(Duration::from_secs(30)), Some(0), "the console drew:\n{text}");
    let modes = |when: &str| {
        let label = format!("console-modes {when}=");
        let at = text.find(&label).unwrap() + label.len();
        text[at..].split('.').next().unwrap().to_owned()
    };
    assert_eq!(modes("before"), modes("after"), "Milo left the console in different modes");
}

#[test]
fn outline_lists_the_files_opens_a_folder_and_quits() {
    let scratch = Scratch::new("outline");
    repository(&scratch);
    let (mut console, at) = open_pane(&scratch, "outline");
    console.see("docs", at);
    console.see("README.md", at);
    console.see("q quit", at);

    // The first row is docs; one step down is src, and Enter opens it.
    let mark = console.text().len();
    console.keys(DOWN);
    console.keys(ENTER);
    console.see("lib.rs", mark);
    console.keys(b"q");
    finish(&console, mark);
}

#[test]
fn history_lists_the_commits_shows_the_files_of_one_and_quits() {
    let scratch = Scratch::new("history");
    repository(&scratch);
    let (mut console, at) = open_pane(&scratch, "history");
    let at = console.see("Extend the guide", at);
    console.see("Add the project files", at);
    console.see("q quit", at);

    let mark = console.text().len();
    console.keys(DOWN);
    console.keys(ENTER);
    console.see("lib.rs", mark);
    console.keys(b"q");
    finish(&console, mark);
}

#[test]
fn home_lists_the_panes_opens_history_and_quits() {
    let scratch = Scratch::new("home");
    repository(&scratch);
    let (mut console, at) = open_pane(&scratch, "");
    let at = console.see("Outline", at);
    let at = console.see("Git history", at);
    let at = console.see("Usage", at);
    console.see("q quit", at);

    // One step down is Git history; q comes back from it to Home, and q again leaves.
    let mark = console.text().len();
    console.keys(DOWN);
    console.keys(ENTER);
    console.see("Extend the guide", mark);
    let mark = console.text().len();
    console.keys(b"q");
    console.see("Git history", mark);
    console.keys(b"q");
    finish(&console, mark);
}

#[test]
fn markdown_renders_in_the_reader_follows_a_save_elsewhere_and_shows_its_source() {
    let scratch = Scratch::new("markdown");
    let file = scratch.0.join("project").join("field-log.md");
    fs::write(&file, "# Field log\r\n\r\nA **steady** climb past [the hut](https://example.invalid/hut).\r\n").unwrap();
    let (mut console, at) = open_pane(&scratch, "view field-log.md");
    let at = console.see("Field log", at);
    console.see("steady", at);
    console.see("m source", at);
    let shown = console.text();
    assert!(!shown.contains("# Field log"), "the heading was drawn as source:\n{shown}");
    assert!(!shown.contains("example.invalid"), "a link destination reached the console:\n{shown}");

    // Another program replaces the file; the view redraws the new text without a key.
    let next = scratch.0.join("project").join("field-log.md.next");
    fs::write(&next, "# Field log\r\n\r\nA **steady** climb.\r\n\r\n## Evening camp\r\n").unwrap();
    fs::rename(&next, &file).unwrap();
    let mark = console.text().len();
    console.see("Evening camp", mark);

    let mark = console.text().len();
    console.keys(b"m");
    console.see("## Evening camp", mark);
    console.keys(b"q");
    finish(&console, mark);
}

/// The seconds since the epoch as the UTC timestamp Codex and Claude Code write.
fn utc(seconds: u64) -> String {
    let (days, rest) = (i64::try_from(seconds / 86_400).unwrap(), seconds % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 { shifted + 3 } else { shifted - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

#[test]
fn usage_reads_local_codex_sessions_and_quits() {
    let scratch = Scratch::new("usage");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let profile = scratch.0.join("profile");
    let rollout = profile.join(".codex").join("sessions").join("2026").join("11").join("18");
    fs::create_dir_all(&rollout).unwrap();
    fs::write(
        rollout.join("rollout-a.jsonl"),
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"rate_limits\":{{\"primary\":{{\"used_percent\":23,\"window_minutes\":10080,\"resets_at\":{}}},\"secondary\":null}}}}}}\n",
            utc(now),
            now + 7200
        ),
    )
    .unwrap();
    let transcripts = profile.join(".claude").join("projects").join("project");
    fs::create_dir_all(&transcripts).unwrap();
    fs::write(
        transcripts.join("session.jsonl"),
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"assistant\",\"message\":{{\"usage\":{{\"input_tokens\":21000,\"output_tokens\":25000,\"cache_creation_input_tokens\":2000,\"cache_read_input_tokens\":0}},\"model\":\"Claude Test\"}}}}\n",
            utc(now)
        ),
    )
    .unwrap();
    let (mut console, at) = open_pane(&scratch, "usage");
    let at = console.see("Codex", at);
    console.see("resets in", at);
    console.see("q quit", at);

    let mark = console.text().len();
    console.keys(b"r");
    console.keys(b"q");
    finish(&console, mark);
}

/// The status line command Milo writes into Claude Code's settings, run the way Claude Code runs
/// it on Windows, through Git Bash: it reads the limits from standard input, prints them, and saves
/// them where doctor and the Usage pane read them.
#[test]
fn claude_status_line_runs_through_git_bash_and_the_usage_pane_shows_its_limits() {
    let scratch = Scratch::new("claude-status");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let transcripts = scratch.0.join("profile").join(".claude").join("projects").join("project");
    fs::create_dir_all(&transcripts).unwrap();
    fs::write(
        transcripts.join("session.jsonl"),
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"assistant\",\"message\":{{\"usage\":{{\"input_tokens\":21000,\"output_tokens\":25000,\"cache_creation_input_tokens\":2000,\"cache_read_input_tokens\":0}},\"model\":\"Claude Test\"}}}}\n",
            utc(now)
        ),
    )
    .unwrap();
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git.exe"))
        .find(|git| git.is_file())
        .expect("git.exe is not on PATH");
    let bash = git.parent().unwrap().parent().unwrap().join("bin").join("bash.exe");
    assert!(bash.is_file(), "Git Bash is not beside {git:?}");
    let milo = env!("CARGO_BIN_EXE_milo").replace('\\', "/");
    let command = format!("'{milo}' claude-status");
    let milo_command = |program: &Path| {
        let mut command = Command::new(program);
        for name in ["HOME", "CLAUDE_CONFIG_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "CODEX_HOME"] {
            command.env_remove(name);
        }
        command.envs(scratch.environment()).current_dir(scratch.0.join("project"));
        command
    };
    let mut child =
        milo_command(&bash).args(["-c", &command]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(br#"{"rate_limits":{"seven_day":{"used_percentage":61}}}"#).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "week 39% left\n");

    let doctor = milo_command(Path::new(env!("CARGO_BIN_EXE_milo"))).arg("doctor").output().unwrap();
    let report = String::from_utf8(doctor.stdout).unwrap();
    assert!(report.contains("Claude Code limits: Milo Claude status line"), "{report}");

    let (mut console, at) = open_pane(&scratch, "usage");
    // The saved week is drawn as a large figure beside its name, and without a reset time.
    console.see("week", at);
    let at = console.see("Claude", at);
    console.see("Reset time unknown", at);
    let mark = console.text().len();
    console.keys(b"q");
    finish(&console, mark);
}

/// Run by the native workflow after tests/web-smoke-windows.cjs, with MILO_SAVED_PREFERENCES naming
/// the preferences file the browser saved. Claude Code's tokens are only shown when it is chosen to
/// always show, so they appear with that file and not without it.
#[test]
#[ignore = "needs the preferences saved by tests/web-smoke-windows.cjs"]
fn usage_follows_preferences_saved_in_the_browser() {
    let saved = PathBuf::from(std::env::var_os("MILO_SAVED_PREFERENCES").expect("MILO_SAVED_PREFERENCES is not set"));
    let scratch = Scratch::new("usage-preferences");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let transcripts = scratch.0.join("profile").join(".claude").join("projects").join("project");
    fs::create_dir_all(&transcripts).unwrap();
    fs::write(
        transcripts.join("session.jsonl"),
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"assistant\",\"message\":{{\"usage\":{{\"input_tokens\":21000,\"output_tokens\":25000,\"cache_creation_input_tokens\":2000,\"cache_read_input_tokens\":0}},\"model\":\"Claude Test\"}}}}\n",
            utc(now)
        ),
    )
    .unwrap();

    let (mut console, at) = open_pane(&scratch, "usage");
    console.see("q quit", at);
    // Long enough for the transcript walk the saved choice makes visible below.
    std::thread::sleep(Duration::from_secs(3));
    let mark = console.text().len();
    console.keys(b"q");
    finish(&console, mark);
    assert!(!console.text().contains("tokens today"), "Claude Code was shown without the saved choice");

    let folder = scratch.0.join("profile").join("Roaming").join("milo");
    fs::create_dir_all(&folder).unwrap();
    fs::copy(&saved, folder.join("preferences.conf")).unwrap();
    let (mut console, at) = open_pane(&scratch, "usage");
    let at = console.see("tokens today", at);
    console.see("q quit", at);
    let mark = console.text().len();
    console.keys(b"q");
    finish(&console, mark);
}
