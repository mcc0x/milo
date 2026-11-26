//! Hands the session URL to the browser without putting its bearer in process arguments.

use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::fs_ops::Temp;

// A cold browser on a slow machine can take far longer than a few seconds to read the file.
// The file is deleted as soon as the browser authenticates, so this only bounds an unused one.
const LIFETIME: Duration = Duration::from_secs(120);
const CHILD_CHECK: Duration = Duration::from_millis(100);

pub(super) struct Launch {
    child: Option<Child>,
    handoff: Option<(Temp, Instant)>,
}

impl Launch {
    pub(super) fn start(url: &str) -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        let program = "/usr/bin/open";
        #[cfg(not(target_os = "macos"))]
        let program = "xdg-open";

        let root = handoff_root()?;
        Self::spawn(&root, Command::new(program), url)
    }

    pub(super) fn spawn(root: &Path, mut command: Command, url: &str) -> io::Result<Self> {
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
        // Its own process group keeps a browser the opener starts alive when Ctrl-C stops Milo.
        let child = command
            .arg(file_url(handoff.path()))
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self { child: Some(child), handoff: Some((handoff, expires)) })
    }

    pub(super) fn connected(&mut self) {
        self.handoff = None;
    }

    pub(super) fn poll(&mut self, now: Instant) -> Option<Instant> {
        if self.handoff.as_ref().is_some_and(|(_, expires)| *expires <= now) {
            self.handoff = None;
        }
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => {
                    let check = now + CHILD_CHECK;
                    return Some(self.handoff.as_ref().map_or(check, |(_, expires)| (*expires).min(check)));
                }
                Ok(Some(status)) if status.success() => {}
                Ok(Some(status)) => self.failed(status),
                Err(error) => self.failed(error),
            }
            self.child = None;
        }
        // A successful opener exit does not mean the browser has read the file yet.
        self.handoff.as_ref().map(|(_, expires)| *expires)
    }

    fn failed(&mut self, error: impl fmt::Display) {
        self.handoff = None;
        eprintln!("milo web: could not open the browser ({error}); open the URL above");
    }
}

/// Where the browser can read the one-use page. Do not put a bearer under an
/// environment-selected TMPDIR: only a directory its owner alone can change is safe.
fn handoff_root() -> io::Result<PathBuf> {
    // A confined browser, such as Ubuntu's Snap Firefox, has its own /tmp and cannot read
    // hidden folders, but it can read the user's home folder. The file lives there for
    // at most a couple of minutes, mode 0600, and goes as soon as the browser connects.
    #[cfg(not(target_os = "macos"))]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute()) {
        // SAFETY: `geteuid` takes no arguments and only reads this process's user ID.
        let user = unsafe { libc::geteuid() };
        if owner_only(&home, user) {
            return Ok(home);
        }
    }
    // In the system temp directory, only the owner or root should be able to remove our file.
    let root = PathBuf::from("/tmp");
    let metadata = fs::metadata(&root)?;
    let mode = metadata.mode();
    if !metadata.is_dir() || metadata.uid() != 0 || (mode & 0o022 != 0 && mode & 0o1000 == 0) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsafe system temporary directory"));
    }
    Ok(root)
}

#[cfg(not(target_os = "macos"))]
fn owner_only(directory: &Path, user: u32) -> bool {
    fs::metadata(directory).is_ok_and(|metadata| metadata.is_dir() && metadata.uid() == user && metadata.mode() & 0o022 == 0)
}

fn file_url(path: &Path) -> String {
    let path = path.as_os_str().as_bytes();
    let mut url = String::with_capacity(7 + path.len());
    url.push_str("file://");
    for &byte in path {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            url.push(char::from(byte));
        } else {
            write!(url, "%{byte:02X}").unwrap();
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    const PRIVATE_URL: &str = "http://127.0.0.1:6110/#0123456789abcdef";

    fn shell(folder: &Fixture, script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script, "fake-opener"]).current_dir(&folder.root);
        command
    }

    #[test]
    fn opener_only_receives_the_private_file_path() {
        let folder = Fixture::folder();
        let directory = folder.root.join("path with # and %");
        fs::create_dir(&directory).unwrap();
        let command = shell(&folder, r#"printf '%s\n' "$@" > argv"#);
        let mut launch = Launch::spawn(&directory, command, PRIVATE_URL).unwrap();
        let (handoff, expires) = launch.handoff.as_ref().unwrap();
        let path = handoff.path().to_path_buf();
        let expires = *expires;
        assert!(launch.child.as_mut().unwrap().wait().unwrap().success());
        let argv = fs::read_to_string(folder.root.join("argv")).unwrap();
        assert_eq!(argv.lines().count(), 1);
        assert!(argv.starts_with("file:///"));
        assert!(argv.contains("/path%20with%20%23%20and%20%25/milo-web-"));
        assert!(!argv.contains("0123456789abcdef"));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(fs::read_to_string(&path).unwrap().contains(PRIVATE_URL));

        assert_eq!(launch.poll(expires - Duration::from_secs(1)), Some(expires));
        assert!(path.exists(), "the browser can read the file after the opener returns");
        assert_eq!(launch.poll(expires), None);
        assert!(!path.exists());
    }

    #[test]
    fn a_slow_browser_can_still_read_the_handoff() {
        let folder = Fixture::folder();
        let command = shell(&folder, r#"sleep 1; cat "${1#file://}" > page"#);
        let mut launch = Launch::spawn(&folder.root, command, PRIVATE_URL).unwrap();
        let (handoff, expires) = launch.handoff.as_ref().unwrap();
        let path = handoff.path().to_path_buf();
        let started = *expires - LIFETIME;

        assert!(launch.poll(started + Duration::from_secs(16)).is_some(), "a cold start must outlast 15 seconds");
        assert!(path.exists());
        assert!(launch.child.as_mut().unwrap().wait().unwrap().success());
        assert!(fs::read_to_string(folder.root.join("page")).unwrap().contains(PRIVATE_URL));

        assert_eq!(launch.poll(started + Duration::from_secs(60)), Some(*launch.handoff.as_ref().map(|(_, e)| e).unwrap()));
        assert!(path.exists());
        launch.connected();
        assert!(!path.exists(), "authentication removes the file at once");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn only_a_folder_nobody_else_can_change_may_hold_the_handoff() {
        use std::os::unix::fs::PermissionsExt;

        let folder = Fixture::folder();
        let user = folder.root.metadata().unwrap().uid();
        let home = folder.root.join("home");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(owner_only(&home, user));
        assert!(!owner_only(&home, user + 1), "another owner's folder");
        assert!(!owner_only(&folder.root.join("missing"), user));
        assert!(!owner_only(&home.join("../home/none"), user));
        for open in [0o770, 0o707, 0o777] {
            fs::set_permissions(&home, fs::Permissions::from_mode(open)).unwrap();
            assert!(!owner_only(&home, user), "mode {open:o} lets others replace the file");
        }
        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(owner_only(&home, user), "a usual home folder is fine");
    }

    #[test]
    fn the_opener_does_not_share_the_terminal_process_group() {
        let folder = Fixture::folder();
        let command = shell(&folder, "ps -o pgid= -p $$ > pgid");
        let mut launch = Launch::spawn(&folder.root, command, PRIVATE_URL).unwrap();
        assert!(launch.child.as_mut().unwrap().wait().unwrap().success());
        let own = String::from_utf8(Command::new("ps").args(["-o", "pgid=", "-p", &std::process::id().to_string()]).output().unwrap().stdout).unwrap();
        let opener = fs::read_to_string(folder.root.join("pgid")).unwrap();
        assert_ne!(opener.trim(), own.trim(), "Ctrl-C would also stop the browser");
    }

    #[test]
    fn deletes_handoff_on_connection_failure_and_shutdown() {
        let folder = Fixture::folder();
        for cleanup in ["connection", "failure", "shutdown"] {
            let mut launch = Launch::spawn(&folder.root, shell(&folder, "exit 1"), PRIVATE_URL).unwrap();
            let path = launch.handoff.as_ref().unwrap().0.path().to_path_buf();
            assert!(!launch.child.as_mut().unwrap().wait().unwrap().success());
            match cleanup {
                "connection" => launch.connected(),
                "failure" => assert_eq!(launch.poll(Instant::now()), None),
                "shutdown" => drop(launch),
                _ => unreachable!(),
            }
            assert!(!path.exists(), "leftover handoff after {cleanup}");
        }
        let missing = Command::new(folder.root.join("not-an-opener"));
        assert_eq!(Launch::spawn(&folder.root, missing, PRIVATE_URL).err().unwrap().kind(), io::ErrorKind::NotFound);
        assert_eq!(fs::read_dir(&folder.root).unwrap().count(), 0);
    }

    #[test]
    #[ignore = "requires MILO_TEST_CHROMIUM pointing to a headless Chromium executable"]
    fn headless_browser_authenticates_through_the_handoff() {
        use crate::setup::web::Editor;
        use crate::web::{http::Access, serve};
        use std::net::{Ipv4Addr, TcpListener};
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

        let mut chrome = Command::new(std::env::var_os("MILO_TEST_CHROMIUM").expect("set MILO_TEST_CHROMIUM"));
        let mut profile = std::ffi::OsString::from("--user-data-dir=");
        profile.push(folder.root.join("chromium-profile"));
        chrome.args(["--headless", "--no-first-run", "--no-default-browser-check", "--disable-background-networking"]);
        chrome.arg(profile);
        let mut launch = Launch::spawn(&folder.root, chrome, &url).unwrap();
        let path = launch.handoff.as_ref().unwrap().0.path().to_path_buf();
        // Take the child so failure cannot look like authentication by clearing the handoff.
        let mut chrome = launch.child.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
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
