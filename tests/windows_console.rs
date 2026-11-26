#![cfg(windows)]
//! Drives the built Milo executable through a Windows pseudoconsole, the way Windows Terminal
//! runs it: open a working text file, edit and save it, refuse to write over a change another
//! program made, quit, and leave the console's modes as they were.

use std::ffi::{OsStr, c_void};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Handle = *mut c_void;

#[repr(C)]
struct StartupInfoEx {
    size: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    numbers: [u32; 8],
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
            !["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "TERM"]
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
            numbers: [0; 8],
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
    println!("console-modes={:#x}/{:#x}.", mode("CONIN$"), mode("CONOUT$"));
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
    let command = format!("cmd.exe /d /s /c \"{probe} & \"{milo}\" view notes.txt --edit & {probe}\"");
    let mut console = Console::start(&command, &scratch.0.join("project"), &scratch.environment());

    let at = console.see("console-modes=", 0);
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
    let ended = console.see("console-modes=", mark);
    console.see(".", ended);
    assert_eq!(console.exit_code(Duration::from_secs(30)), Some(0), "the console drew:\n{}", console.text());
    assert_eq!(fs::read_to_string(&file).unwrap(), external);

    let text = console.text();
    let modes: Vec<&str> =
        text.match_indices("console-modes=").map(|(at, _)| text[at..].split('.').next().unwrap()).collect();
    assert_eq!(modes.len(), 2, "{text}");
    assert_eq!(modes[0], modes[1], "Milo left the console in different modes");
}
