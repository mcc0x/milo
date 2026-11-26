//! Hands the session URL to the default browser without putting its bearer in a process argument.
//! The browser is told only the path of a private page that forwards it to the editor. The shell
//! opens that page with the program registered for it, reading the path as a file name and never
//! as a command line for cmd.exe, and the browser gets no share of Milo's console, so Ctrl-C in
//! Milo does not reach it.

use std::ffi::{OsStr, c_void};
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::fs_ops::Temp;

// A cold browser on a slow machine can take far longer than a few seconds to read the file.
// The file is deleted as soon as the browser authenticates, so this only bounds an unused one.
const LIFETIME: Duration = Duration::from_secs(120);
const OPENER_CHECK: Duration = Duration::from_millis(100);

pub(super) struct Launch {
    opener: Option<JoinHandle<io::Result<()>>>,
    handoff: Option<(Temp, Instant)>,
}

impl Launch {
    pub(super) fn start(url: &str) -> io::Result<Self> {
        // Milo's state folder is in the user's own profile. The page itself is private to the user.
        let root = crate::paths::state_dir().ok_or(io::ErrorKind::NotFound)?;
        fs::create_dir_all(&root)?;
        crate::fs_ops::open_dir(&root)?;
        Self::spawn(&root, open_with_shell, url)
    }

    /// Writes the page and asks `open`, on a thread of its own, to show it.
    pub(super) fn spawn(
        root: &Path,
        open: impl FnOnce(&Path) -> io::Result<()> + Send + 'static,
        url: &str,
    ) -> io::Result<Self> {
        let name = super::random_token()?;
        let (handoff, mut file) = Temp::create(root.join(format!("milo-web-{name}.html")))?;
        // The editor URL is constructed from a numeric loopback address and a hex token.
        // A zero-delay refresh replaces this local page in the tab's history.
        write!(
            file,
            "<!doctype html><meta charset=utf-8><meta name=referrer content=no-referrer>\
             <meta http-equiv=refresh content=\"0;url={url}\"><title>Opening Milo</title>\
             <p>If Milo does not open, use the private URL printed in your terminal.</p>"
        )?;
        drop(file);
        let expires = Instant::now() + LIFETIME;
        let page = handoff.path().to_path_buf();
        let opener = std::thread::Builder::new().name("browser opener".into()).spawn(move || open(&page))?;
        Ok(Self { opener: Some(opener), handoff: Some((handoff, expires)) })
    }

    pub(super) fn connected(&mut self) {
        self.handoff = None;
    }

    pub(super) fn poll(&mut self, now: Instant) -> Option<Instant> {
        if self.handoff.as_ref().is_some_and(|(_, expires)| *expires <= now) {
            self.handoff = None;
        }
        if let Some(opener) = self.opener.take_if(|opener| opener.is_finished()) {
            match opener.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.failed(error),
                Err(_) => self.failed("the opener stopped"),
            }
        }
        if self.opener.is_some() {
            let check = now + OPENER_CHECK;
            return Some(self.handoff.as_ref().map_or(check, |(_, expires)| (*expires).min(check)));
        }
        // A successful open does not mean the browser has read the file yet.
        self.handoff.as_ref().map(|(_, expires)| *expires)
    }

    fn failed(&mut self, error: impl fmt::Display) {
        self.handoff = None;
        eprintln!("milo web: could not open the browser ({error}); open the URL above");
    }
}

#[repr(C)]
struct ShellExecuteInfo {
    size: u32,
    mask: u32,
    window: *mut c_void,
    verb: *const u16,
    file: *const u16,
    parameters: *const u16,
    directory: *const u16,
    show: i32,
    instance: *mut c_void,
    id_list: *mut c_void,
    class: *const u16,
    class_key: *mut c_void,
    hot_key: u32,
    icon_or_monitor: *mut c_void,
    process: *mut c_void,
}

/// Finish before returning, since this thread pumps no messages.
const SEE_MASK_NOASYNC: u32 = 0x100;
const SEE_MASK_FLAG_NO_UI: u32 = 0x400;
/// A console program started for the page gets a console of its own instead of Milo's.
const SEE_MASK_NO_CONSOLE: u32 = 0x8000;
const SW_SHOWNORMAL: i32 = 1;
const COINIT_APARTMENTTHREADED: u32 = 0x2;
const COINIT_DISABLE_OLE1DDE: u32 = 0x4;

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfo) -> i32;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
    fn CoUninitialize();
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

/// Opens the page with the program the user chose for web pages, as Explorer would.
fn open_with_shell(page: &Path) -> io::Result<()> {
    let (verb, file) = (wide(OsStr::new("open")), wide(page.as_os_str()));
    let null = std::ptr::null_mut;
    let mut info = ShellExecuteInfo {
        size: u32::try_from(size_of::<ShellExecuteInfo>()).expect("the record fits in a u32"),
        mask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI | SEE_MASK_NO_CONSOLE,
        window: null(),
        verb: verb.as_ptr(),
        file: file.as_ptr(),
        parameters: std::ptr::null(),
        directory: std::ptr::null(),
        show: SW_SHOWNORMAL,
        instance: null(),
        id_list: null(),
        class: std::ptr::null(),
        class_key: null(),
        hot_key: 0,
        icon_or_monitor: null(),
        process: null(),
    };
    // Some handlers are shell extensions that need COM on the calling thread.
    // SAFETY: COM is started for this thread only, and ended below if this call started it.
    let com = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    // SAFETY: The record is sized for the call, and the strings it names are NUL-terminated and outlive it.
    let opened = unsafe { ShellExecuteExW(&raw mut info) } != 0;
    let error = io::Error::last_os_error();
    if com >= 0 {
        // SAFETY: This balances the successful CoInitializeEx above, on the same thread.
        unsafe { CoUninitialize() };
    }
    if opened { Ok(()) } else { Err(error) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::path::PathBuf;
    use std::sync::mpsc;

    const PRIVATE_URL: &str = "http://127.0.0.1:6110/#0123456789abcdef";

    /// An opener that reports the page it was given and what that page said.
    fn reading(send: mpsc::Sender<(PathBuf, String)>) -> impl FnOnce(&Path) -> io::Result<()> + Send + 'static {
        move |page: &Path| {
            let text = fs::read_to_string(page)?;
            send.send((page.to_path_buf(), text)).map_err(io::Error::other)
        }
    }

    fn finish(launch: &mut Launch) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !launch.opener.as_ref().is_some_and(JoinHandle::is_finished) {
            assert!(Instant::now() < deadline, "the opener did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn opener_only_receives_the_private_page_path() {
        let folder = Fixture::folder();
        let (send, receive) = mpsc::channel();
        let mut launch = Launch::spawn(&folder.root, reading(send), PRIVATE_URL).unwrap();
        let (page, text) = receive.recv_timeout(Duration::from_secs(10)).unwrap();
        let (handoff, expires) = launch.handoff.as_ref().unwrap();
        let expires = *expires;
        assert_eq!(page, handoff.path());
        assert_eq!(page.parent(), Some(folder.root.as_path()));
        let name = page.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("milo-web-") && name.ends_with(".html"), "{name}");
        assert!(!name.contains("0123456789abcdef"));
        assert!(text.contains(PRIVATE_URL));
        let file = fs::File::open(&page).unwrap();
        assert_eq!(crate::sys::file_security(&file).unwrap(), crate::sys::private_security().unwrap());
        drop(file);

        finish(&mut launch);
        assert_eq!(launch.poll(expires - Duration::from_secs(1)), Some(expires));
        assert!(page.exists(), "the browser can read the file after the opener returns");
        assert_eq!(launch.poll(expires), None);
        assert!(!page.exists());
    }

    #[test]
    fn a_slow_browser_can_still_read_the_handoff() {
        let folder = Fixture::folder();
        let (send, receive) = mpsc::channel();
        let mut launch = Launch::spawn(&folder.root, reading(send), PRIVATE_URL).unwrap();
        let (page, _) = receive.recv_timeout(Duration::from_secs(10)).unwrap();
        let started = launch.handoff.as_ref().unwrap().1 - LIFETIME;
        finish(&mut launch);
        assert!(launch.poll(started + Duration::from_secs(16)).is_some(), "a cold start must outlast 15 seconds");
        assert!(launch.poll(started + Duration::from_secs(119)).is_some());
        assert!(page.exists());
        launch.connected();
        assert!(!page.exists(), "authentication removes the file at once");
        assert_eq!(launch.poll(started + Duration::from_secs(16)), None);
    }

    #[test]
    fn deletes_handoff_on_connection_failure_and_shutdown() {
        let folder = Fixture::folder();
        for cleanup in ["connection", "failure", "shutdown"] {
            let failing = |_: &Path| Err(io::Error::from(io::ErrorKind::NotFound));
            let mut launch = Launch::spawn(&folder.root, failing, PRIVATE_URL).unwrap();
            let page = launch.handoff.as_ref().unwrap().0.path().to_path_buf();
            finish(&mut launch);
            match cleanup {
                "connection" => launch.connected(),
                "failure" => assert_eq!(launch.poll(Instant::now()), None),
                "shutdown" => drop(launch),
                _ => unreachable!(),
            }
            assert!(!page.exists(), "leftover handoff after {cleanup}");
        }
        assert_eq!(fs::read_dir(&folder.root).unwrap().count(), 0);
    }

    #[test]
    fn the_shell_reports_a_page_it_cannot_open_without_asking_anyone() {
        let folder = Fixture::folder();
        let error = open_with_shell(&folder.root.join("missing.html")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn the_shell_request_has_the_layout_windows_reads() {
        assert_eq!(size_of::<ShellExecuteInfo>(), 112);
        assert_eq!(std::mem::offset_of!(ShellExecuteInfo, show), 48);
        assert_eq!(std::mem::offset_of!(ShellExecuteInfo, process), 104);
    }

    #[test]
    #[ignore = "requires MILO_TEST_CHROMIUM pointing to a headless Chromium executable"]
    fn headless_browser_authenticates_through_the_handoff() {
        use crate::setup::web::Editor;
        use crate::web::{http::Access, serve};
        use std::net::{Ipv4Addr, TcpListener};
        use std::process::{Command, Stdio};
        use std::thread;

        let folder = Fixture::folder();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let origin = format!("http://{host}");
        let token = super::super::random_token().unwrap();
        let bearer = format!("Bearer {token}");
        let url = format!("{origin}/#{token}");
        let access = Access { host: &host, origin: &origin, bearer: &bearer };
        let editor = Editor::test_outside(&folder);
        let (wake, writer) = io::pipe().unwrap();

        let chromium = std::env::var_os("MILO_TEST_CHROMIUM").expect("set MILO_TEST_CHROMIUM");
        let mut profile = std::ffi::OsString::from("--user-data-dir=");
        profile.push(folder.root.join("chromium-profile"));
        let (started, chrome) = mpsc::channel();
        let open = move |page: &Path| {
            let child = Command::new(chromium)
                .args(["--headless", "--no-first-run", "--no-default-browser-check", "--disable-background-networking"])
                .arg(profile)
                .arg(page)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            started.send(child).map_err(io::Error::other)
        };
        let launch = Launch::spawn(&folder.root, open, &url).unwrap();
        let path = launch.handoff.as_ref().unwrap().0.path().to_path_buf();
        let mut chrome = chrome.recv_timeout(Duration::from_secs(10)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let watcher = thread::spawn(move || {
            let connected = loop {
                if Instant::now() >= deadline || chrome.try_wait().unwrap().is_some() {
                    break false;
                }
                if !path.exists() {
                    break true;
                }
                thread::sleep(Duration::from_millis(25));
            };
            let _ = chrome.kill();
            chrome.wait().unwrap();
            drop(writer);
            connected
        });
        serve(listener, &wake, &access, editor, Some(launch)).unwrap();
        assert!(watcher.join().unwrap(), "Chromium did not authenticate before the handoff expired");
    }
}
