//! Hands a verified working-tree path to the local macOS opener on explicit input.

mod local;

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{fs_ops, keys::Key};

const ROOT_CHANGED: &str = "Can't verify the original project folder";
const INVALID_PATH: &str = "This is not a path inside the project";
const MISSING: &str = "The selected path no longer exists";
const LINK: &str = "Can't use a symbolic link or a path through one";
const UNVERIFIED: &str = "Can't verify the selected path";
const LAUNCHABLE: &str = "Use Reveal for launchable files and aliases";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Open,
    Reveal,
}

impl Action {
    pub(crate) fn key(key: Key) -> Option<Self> {
        match key {
            Key::Ctrl(b'o') => Some(Self::Open),
            Key::Ctrl(b'r') => Some(Self::Reveal),
            _ => None,
        }
    }

    pub(crate) fn run(self, root: &Path, identity: Option<(u64, u64)>, relative: &Path) -> Result<(), &'static str> {
        #[cfg(test)]
        tests::locality().unwrap_or_else(local::check)?;
        #[cfg(not(test))]
        local::check()?;

        let target = self.target(root, identity, relative)?;
        let command = self.command(&target);
        #[cfg(test)]
        if tests::capture(self, &command) {
            return Ok(());
        }
        launch(command)
    }

    fn target(self, root: &Path, identity: Option<(u64, u64)>, relative: &Path) -> Result<PathBuf, &'static str> {
        let expected = identity.ok_or(ROOT_CHANGED)?;
        let mut dir = root_dir(root).ok_or(ROOT_CHANGED)?;
        if dir_id(&dir) != Some(expected) {
            return Err(ROOT_CHANGED);
        }

        let mut parts = relative.as_os_str().as_bytes().rsplitn(2, |&byte| byte == b'/');
        let name = child_name(parts.next().unwrap_or_default())?;
        if let Some(parent) = parts.next() {
            for part in parent.split(|&byte| byte == b'/') {
                let child = child_name(part)?;
                dir = fs_ops::open_at(&dir, &child, libc::O_RDONLY | libc::O_DIRECTORY, 0).map_err(path_error)?;
            }
        }
        let stat = stat_at(&dir, &name).map_err(path_error)?;
        match stat.st_mode & libc::S_IFMT {
            libc::S_IFLNK => return Err(LINK),
            libc::S_IFDIR if self == Self::Reveal => {}
            libc::S_IFDIR => return Err("Use Reveal for folders"),
            libc::S_IFREG if self == Self::Open => check_open(&dir, &name, &stat)?,
            libc::S_IFREG => {}
            _ => return Err("Only regular files and folders can be revealed"),
        }
        // Holding directory descriptors keeps validation from following links.
        // The opener resolves a pathname again, so a checked entry can still be
        // replaced before LaunchServices uses it.
        Ok(root.join(relative))
    }

    fn command(self, target: &Path) -> Command {
        let mut command = Command::new("/usr/bin/open");
        command.env_clear();
        if self == Self::Reveal {
            command.arg("-R");
        }
        command.arg(target.as_os_str());
        command
    }
}

/// Capture the original directory before the user can request an action.
pub(crate) fn identity(root: &Path) -> Option<(u64, u64)> {
    dir_id(&root_dir(root)?)
}

fn dir_id(dir: &File) -> Option<(u64, u64)> {
    let metadata = dir.metadata().ok()?;
    Some((metadata.dev(), metadata.ino()))
}

fn root_dir(root: &Path) -> Option<File> {
    let mut components = root.components();
    if components.next() != Some(Component::RootDir) {
        return None;
    }
    let mut dir = fs_ops::open_dir(Path::new("/")).ok()?;
    for part in components {
        let Component::Normal(part) = part else { return None };
        let name = child_name(part.as_bytes()).ok()?;
        dir = fs_ops::open_at(&dir, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0).ok()?;
    }
    Some(dir)
}

fn child_name(bytes: &[u8]) -> Result<CString, &'static str> {
    if matches!(bytes, b"" | b"." | b"..") {
        return Err(INVALID_PATH);
    }
    CString::new(bytes).map_err(|_| INVALID_PATH)
}

fn path_error(error: io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::ENOENT) => MISSING,
        Some(libc::ELOOP) => LINK,
        Some(libc::ENOTDIR) => "A parent is a link or is not a folder",
        _ => UNVERIFIED,
    }
}

fn stat_at(dir: &File, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = MaybeUninit::uninit();
    // SAFETY: The descriptor and NUL-terminated name are live and the output is writable.
    let result = unsafe { libc::fstatat(dir.as_raw_fd(), name.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: A successful fstatat initializes the output.
    Ok(unsafe { stat.assume_init() })
}

fn check_open(dir: &File, name: &CStr, stat: &libc::stat) -> Result<(), &'static str> {
    // Some types are launched based on their name even without POSIX execute bits.
    const EXTENSIONS: &[&[u8]] = &[
        b"action", b"alias", b"app", b"applescript", b"appref-ms", b"bash", b"bat", b"bundle", b"cjs", b"cmd", b"com",
        b"command", b"csh", b"dmg", b"exe", b"fileloc", b"fish", b"ftploc", b"inetloc", b"iso", b"jar", b"jnlp", b"js",
        b"jse", b"ksh", b"lnk", b"lua", b"mjs", b"mobileconfig", b"mpkg", b"pkg", b"pl", b"plugin", b"prefpane",
        b"ps1", b"py", b"pyw", b"rb", b"scpt", b"scptd", b"sh", b"sparseimage", b"tcsh", b"term", b"terminal", b"tool",
        b"url", b"vb", b"vbe", b"vbs", b"webloc", b"workflow", b"wsf", b"zsh",
    ];
    let mut parts = name.to_bytes().rsplitn(2, |&byte| byte == b'.');
    let extension = parts.next().unwrap_or_default();
    let launchable_name = parts.next().is_some() && EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(extension));
    if stat.st_mode & 0o111 != 0 || launchable_name {
        return Err(LAUNCHABLE);
    }
    #[cfg(target_os = "macos")]
    check_finder_metadata(dir, name, stat)?;
    #[cfg(not(target_os = "macos"))]
    let _ = dir;
    Ok(())
}

#[cfg(target_os = "macos")]
fn check_finder_metadata(dir: &File, name: &CStr, stat: &libc::stat) -> Result<(), &'static str> {
    // Read Finder metadata, not document data. In particular, don't download
    // an offline file just to decide whether it can be opened.
    let file = fs_ops::open_at(dir, name, libc::O_EVTONLY, 0).map_err(path_error)?;
    let metadata = file.metadata().map_err(path_error)?;
    if !metadata.is_file() || u64::try_from(stat.st_dev).ok() != Some(metadata.dev()) || stat.st_ino != metadata.ino() {
        return Err(UNVERIFIED);
    }
    if metadata.mode() & 0o111 != 0 {
        return Err(LAUNCHABLE);
    }
    let mut finder = [0u8; 32];
    if xattr(&file, c"com.apple.FinderInfo", &mut finder)?.is_some_and(|size| {
        size != finder.len() || finder[8] & 0x80 != 0 || matches!(&finder[..4], b"APPL" | b"alis" | b"osas")
    }) {
        return Err(LAUNCHABLE);
    }
    // A checkout must not supply its own binding to an application. An empty
    // buffer asks for the attribute's length without reading the binding.
    if xattr(&file, c"com.apple.LaunchServices.OpenWith", &mut [])?.is_some() {
        return Err("Use Reveal for files with a custom app binding");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn xattr(file: &File, name: &CStr, buffer: &mut [u8]) -> Result<Option<usize>, &'static str> {
    let value = if buffer.is_empty() { std::ptr::null_mut() } else { buffer.as_mut_ptr().cast() };
    // SAFETY: The descriptor and name are live; value is null for a size query
    // or writable for buffer.len() bytes.
    let size = unsafe { libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), value, buffer.len(), 0, 0) };
    if size >= 0 {
        return usize::try_from(size).map(Some).map_err(|_| UNVERIFIED);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ENOATTR | libc::ENOTSUP) => Ok(None),
        _ => Err(UNVERIFIED),
    }
}

fn launch(mut command: Command) -> Result<(), &'static str> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Can't start the macOS opener")?;
    if !child.wait().map_err(|_| "Can't wait for the macOS opener")?.success() {
        return Err("The macOS opener couldn't open the selected path");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;
    use crate::fixture::Fixture;

    struct Fake {
        locality: Result<(), &'static str>,
        calls: Vec<(Action, PathBuf)>,
    }

    thread_local! {
        static FAKE: RefCell<Option<Fake>> = const { RefCell::new(None) };
    }

    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            FAKE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    pub(crate) fn with_fake<R>(
        locality: Result<(), &'static str>,
        f: impl FnOnce() -> R,
    ) -> (R, Vec<(Action, PathBuf)>) {
        FAKE.with(|slot| {
            let mut slot = slot.borrow_mut();
            assert!(slot.is_none(), "already recording native actions");
            *slot = Some(Fake { locality, calls: Vec::new() });
        });
        let _reset = Reset;
        let result = f();
        let calls = FAKE.with(|slot| slot.borrow_mut().take().unwrap().calls);
        (result, calls)
    }

    pub(super) fn locality() -> Option<Result<(), &'static str>> {
        FAKE.with(|slot| slot.borrow().as_ref().map(|fake| fake.locality))
    }

    pub(super) fn capture(action: Action, command: &Command) -> bool {
        FAKE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(fake) = slot.as_mut() else { return false };
            let mut args = command.get_args();
            assert_eq!(command.get_program(), "/usr/bin/open");
            if action == Action::Reveal {
                assert_eq!(args.next(), Some(OsStr::new("-R")));
            }
            let target = PathBuf::from(args.next().unwrap());
            assert!(target.is_absolute());
            assert_eq!(args.next(), None);
            fake.calls.push((action, target));
            true
        })
    }

    #[test]
    fn exact_arguments_for_duplicate_basenames_and_folders() {
        let fixture = Fixture::folder();
        let first = "a/-same 'name #%.pdf";
        let second = "b/-same 'name #%.pdf";
        fixture.write(first, "first");
        fixture.write(second, "second");
        let root = &fixture.root;
        let id = identity(root);
        let ((), calls) = with_fake(Ok(()), || {
            assert_eq!(Action::Open.run(root, id, Path::new(first)), Ok(()));
            assert_eq!(Action::Reveal.run(root, id, Path::new(second)), Ok(()));
            assert_eq!(Action::Reveal.run(root, id, Path::new("a")), Ok(()));
            assert_eq!(Action::Open.run(root, id, Path::new("a")), Err("Use Reveal for folders"));
        });
        assert_eq!(
            calls,
            [(Action::Open, root.join(first)), (Action::Reveal, root.join(second)), (Action::Reveal, root.join("a"))]
        );
    }

    #[test]
    fn passes_os_bytes_without_a_lossy_conversion() {
        let path = Path::new(OsStr::from_bytes(b"/project/-name\xff.pdf"));
        let command = Action::Open.command(path);
        assert_eq!(command.get_args().collect::<Vec<_>>(), [path.as_os_str()]);
    }

    #[test]
    fn denies_remote_before_checking_the_path() {
        let (result, calls) =
            with_fake(Err("Can't verify a local viewer"), || Action::Open.run(Path::new("bad"), None, Path::new("..")));
        assert_eq!(result, Err("Can't verify a local viewer"));
        assert!(calls.is_empty());
    }

    #[test]
    fn rejects_missing_traversal_and_special_paths() {
        let fixture = Fixture::folder();
        fixture.write("dir/report.pdf", "data");
        let root = &fixture.root;
        let id = identity(root);
        let pipe = CString::new(root.join("pipe").as_os_str().as_bytes()).unwrap();
        // SAFETY: The name is NUL-terminated and points into the disposable fixture.
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        let ((), calls) = with_fake(Ok(()), || {
            for relative in ["", ".", "..", "/dir/report.pdf", "dir/../pipe", "dir/./report.pdf", "dir//report.pdf"] {
                assert_eq!(Action::Open.run(root, id, Path::new(relative)), Err(INVALID_PATH), "{relative}");
            }
            for relative in ["missing", "missing/report.pdf"] {
                assert_eq!(Action::Reveal.run(root, id, Path::new(relative)), Err(MISSING), "{relative}");
            }
            for action in [Action::Open, Action::Reveal] {
                assert!(action.run(root, id, Path::new("pipe")).is_err());
            }
        });
        assert!(calls.is_empty());
    }

    #[test]
    fn refuses_linked_leaf_directory_and_root_ancestor() {
        let fixture = Fixture::folder();
        fixture.write("holder/repo/sub/report.pdf", "data");
        let holder = fixture.root.join("holder");
        let root = holder.join("repo");
        let id = identity(&root);
        symlink("sub/report.pdf", root.join("link.pdf")).unwrap();
        symlink("sub", root.join("link-dir")).unwrap();
        let ((), calls) = with_fake(Ok(()), || {
            for action in [Action::Open, Action::Reveal] {
                assert_eq!(action.run(&root, id, Path::new("link.pdf")), Err(LINK));
                assert!(action.run(&root, id, Path::new("link-dir/report.pdf")).is_err());
            }
            let moved = fixture.root.join("moved");
            fs::rename(&holder, &moved).unwrap();
            symlink(&moved, &holder).unwrap();
            assert_eq!(identity(&root), None);
            assert_eq!(Action::Open.run(&root, id, Path::new("sub/report.pdf")), Err(ROOT_CHANGED));
        });
        assert!(calls.is_empty());
    }

    #[test]
    fn detects_same_named_root_replacement_and_unavailable_identity() {
        let fixture = Fixture::folder();
        fixture.write("repo/report.pdf", "original");
        let root = fixture.root.join("repo");
        let id = identity(&root);
        assert!(id.is_some());
        fs::rename(&root, fixture.root.join("moved")).unwrap();
        fixture.write("repo/report.pdf", "replacement");
        let ((), calls) = with_fake(Ok(()), || {
            assert_eq!(Action::Open.run(&root, id, Path::new("report.pdf")), Err(ROOT_CHANGED));
            assert_eq!(Action::Reveal.run(&root, None, Path::new("report.pdf")), Err(ROOT_CHANGED));
        });
        assert!(calls.is_empty());
    }

    #[test]
    fn launchable_formats_and_executable_mode_are_reveal_only() {
        let fixture = Fixture::folder();
        let files = ["script.CoMmAnD", "run.pdf", "remote.webloc", "shortcut.terminal", "installer.pkg", "app.jar"];
        for file in files {
            fixture.write(file, "");
        }
        let root = &fixture.root;
        let id = identity(root);
        fs::set_permissions(root.join("run.pdf"), fs::Permissions::from_mode(0o744)).unwrap();
        fs::create_dir(root.join("Application.app")).unwrap();
        let ((), calls) = with_fake(Ok(()), || {
            for file in files {
                assert_eq!(Action::Open.run(root, id, Path::new(file)), Err(LAUNCHABLE), "{file}");
                assert_eq!(Action::Reveal.run(root, id, Path::new(file)), Ok(()), "{file}");
            }
            assert_eq!(Action::Open.run(root, id, Path::new("Application.app")), Err("Use Reveal for folders"));
            assert_eq!(Action::Reveal.run(root, id, Path::new("Application.app")), Ok(()));
        });
        assert_eq!(calls.len(), files.len() + 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn refuses_finder_alias_metadata_and_per_file_app_bindings() {
        let fixture = Fixture::folder();
        let root = &fixture.root;
        let id = identity(root);
        let mut finder = [0u8; 32];
        finder[8] = 0x80;
        let cases = [
            ("alias.pdf", c"com.apple.FinderInfo", finder.as_slice(), LAUNCHABLE),
            (
                "binding.pdf",
                c"com.apple.LaunchServices.OpenWith",
                b"binding",
                "Use Reveal for files with a custom app binding",
            ),
        ];
        let ((), calls) = with_fake(Ok(()), || {
            for (relative, attr, value, expected) in cases {
                fixture.write(relative, "data");
                let path = CString::new(root.join(relative).as_os_str().as_bytes()).unwrap();
                // SAFETY: The fixture path and name are NUL-terminated, and the value is live and readable.
                let rc =
                    unsafe { libc::setxattr(path.as_ptr(), attr.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0) };
                assert_eq!(rc, 0, "{}", io::Error::last_os_error());
                assert_eq!(Action::Open.run(root, id, Path::new(relative)), Err(expected));
                assert_eq!(Action::Reveal.run(root, id, Path::new(relative)), Ok(()));
            }
        });
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn reports_spawn_and_child_failure_without_a_gui() {
        let fixture = Fixture::folder();
        assert_eq!(launch(Command::new(fixture.root.join("missing-opener"))), Err("Can't start the macOS opener"));
        let mut failing = Command::new("/bin/sh");
        failing.args(["-c", "exit 7"]);
        assert_eq!(launch(failing), Err("The macOS opener couldn't open the selected path"));
    }
}
