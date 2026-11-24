//! Launches an explicit Markdown preview in a tiled terminal on the local Rex host.

use std::ffi::{CString, OsStr, OsString};
use std::fs::{DirBuilder, File};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use super::{Target, exact_block, exact_id, json, parent_program, scripted, socket_path, uuid};
use crate::fs_ops;
use crate::native::trusted;

const CONTEXT: &str = "Open Milo in a local Rex pane to preview Markdown.";
const ANCHOR: &str = "Could not identify this tab's shell for Markdown preview.";
const FAILED: &str = "Rex could not open Markdown preview.";
const RECORD: &str = "Milo could not record the preview pane.";
const WAIT: Duration = Duration::from_secs(3);
const SOURCE: &str = "MILO_PREVIEW_SOURCE";
static ANCHOR_ID: OnceLock<OsString> = OnceLock::new();

/// Remember the shell supplied by Rex at launch, not whichever pane gains focus later.
pub(crate) fn preview_anchor(anchor: &OsStr) {
    let _ = ANCHOR_ID.set(anchor.to_owned());
}

pub(super) fn anchor_argument() -> Option<&'static str> {
    ANCHOR_ID.get().map(|id| exact_block(id).unwrap_or("-"))
}

struct Project {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Project {
    fn open(root: &Path) -> Option<Self> {
        let path = root.canonicalize().ok()?;
        let meta = path.metadata().ok()?;
        Some(Self { path, device: meta.dev(), inode: meta.ino() })
    }

    /// Also reject aliases for the project on case-insensitive filesystems.
    fn outside(&self, path: &Path) -> bool {
        !path.starts_with(&self.path)
            && !path.ancestors().any(|p| p.metadata().is_ok_and(|m| (m.dev(), m.ino()) == (self.device, self.inode)))
    }
}

fn trusted_rex(root: &Path) -> Option<PathBuf> {
    let cwd = Path::new(".");
    let from_parent = parent_program();
    let from_rex = std::env::var_os("REX_BIN_DIR").map(|dir| PathBuf::from(dir).join("rex"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    trusted::find("rex", Some(root), cwd, from_parent.as_deref(), OsStr::new(""))
        .or_else(|| trusted::find("rex", Some(root), cwd, from_rex.as_deref(), &path))
}

fn executable(project: &Project, path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    trusted::find(name, Some(&project.path), Path::new("."), Some(path), OsStr::new(""))
}

struct Rex {
    exe: PathBuf,
    project: Project,
    session: String,
    caller: String,
    endpoint: Option<String>,
    uid: u32,
}

impl Rex {
    fn local(root: &Path) -> Option<Self> {
        let target = Target::current()?;
        let session = exact_id(&target.session, "session:")?.to_owned();
        let caller = exact_block(&target.block)?.to_owned();
        let project = Project::open(root)?;
        let exe = trusted_rex(root)?;
        let endpoint = match std::env::var_os("REX_SERVER") {
            Some(endpoint) if !endpoint.is_empty() && endpoint != "localhost" => {
                let path = socket_path(endpoint.to_str()?)?.canonicalize().ok()?;
                if !project.outside(&path) || super::socket_identity(path.to_str()?).is_none() {
                    return None;
                }
                Some(format!("unix://{}", path.to_str()?))
            }
            _ => None,
        };
        // SAFETY: Getting our own effective user ID touches no memory.
        let uid = unsafe { libc::geteuid() };
        Some(Self { exe, project, session, caller, endpoint, uid })
    }

    fn request(&self, args: &str) -> Option<String> {
        let mut command = Command::new(&self.exe);
        command.env_clear().env("PATH", "/usr/bin:/bin").current_dir("/").arg("--autostart=false");
        // Keep local socket discovery, not the project's loader, Lua, shell or Rex overrides.
        for name in ["HOME", "TMPDIR", "XDG_RUNTIME_DIR", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
            if let Some(dir) = std::env::var_os(name).map(PathBuf::from)
                && dir.is_absolute()
                && dir.canonicalize().is_ok_and(|path| self.project.outside(&path))
            {
                command.env(name, dir);
            }
        }
        if let Some(endpoint) = &self.endpoint {
            command.args(["-S", endpoint]);
        }
        command.args(["--timeout", "1s", "do", "-", "-s", &self.session, "-b", &self.caller, "--args", args]);
        scripted(&mut command, include_str!("preview.lua"), WAIT)
    }

    fn arguments(&self) -> String {
        format!(r#"{{"uid":{},"session":"{}","caller":"{}""#, self.uid, self.session, self.caller)
    }
}

fn source(encoded: Option<&str>) -> Option<String> {
    let mut words = encoded?.split_ascii_whitespace();
    let session = exact_id(OsStr::new(words.next()?), "session:")?;
    let window = exact_id(OsStr::new(words.next()?), "window:")?;
    let explicit = match words.next()? {
        "shell" => true,
        "caller" => false,
        _ => return None,
    };
    let block = exact_block(OsStr::new(words.next()?))?;
    if words.next().is_some() {
        return None;
    }
    Some(format!(r#"{{"session":"{session}","window":"{window}","block":"{block}","explicit":{explicit}}}"#))
}

fn tab(reply: &str) -> Option<(&str, &str)> {
    let mut words = reply.split_ascii_whitespace();
    let instance = words.next()?;
    let window = exact_id(OsStr::new(words.next()?), "window:")?;
    (uuid(instance) && words.next().is_none()).then_some((instance, window))
}

/// The private, per-user ownership ledger is only meaningful until Rex restarts.
/// A lock serializes requests from independent Milo processes in the same tab.
fn record(rex: &Rex, instance: &str, window: &str) -> Result<File, &'static str> {
    let cwd = Project::open(Path::new(".")).ok_or(RECORD)?;
    // A login shell and a floating Reader can have different TMPDIRs.
    // Use one base so independent Milo processes share the tab's lock and ID.
    let base = Path::new("/tmp").canonicalize().map_err(|_| RECORD)?;
    let meta = base.metadata().map_err(|_| RECORD)?;
    let private_or_sticky = meta.mode() & 0o022 == 0 || meta.mode() & 0o1000 != 0;
    if !meta.is_dir() || (meta.uid() != 0 && meta.uid() != rex.uid) || !private_or_sticky {
        return Err(RECORD);
    }
    let dir = base.join(format!(".milo-rex-preview-{}", rex.uid));
    if !rex.project.outside(&dir) || !cwd.outside(&dir) {
        return Err(RECORD);
    }
    match DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(RECORD),
    }
    let dir = fs_ops::open_dir(&dir).map_err(|_| RECORD)?;
    let meta = dir.metadata().map_err(|_| RECORD)?;
    if meta.uid() != rex.uid || meta.mode() & 0o077 != 0 {
        return Err(RECORD);
    }
    let name = CString::new(format!("{instance}-{}-{window}", rex.session)).map_err(|_| RECORD)?;
    let file = fs_ops::open_at(&dir, &name, libc::O_RDWR | libc::O_CREAT, 0o600).map_err(|_| RECORD)?;
    let meta = file.metadata().map_err(|_| RECORD)?;
    if !meta.is_file() || meta.uid() != rex.uid || meta.nlink() != 1 || meta.mode() & 0o077 != 0 {
        return Err(RECORD);
    }
    // SAFETY: The descriptor remains open until the request and record update are complete.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Another Markdown preview is opening. Try again.");
    }
    Ok(file)
}

/// Cheap preflight for the action menu. The actual request proves the local Rex context.
pub(crate) fn available(root: &Path) -> bool {
    Rex::local(root).is_some()
}

pub(crate) fn launch(root: &Path, tool: &Path, file: &Path) -> Result<(), &'static str> {
    let rex = Rex::local(root).ok_or(CONTEXT)?;
    let tool = executable(&rex.project, tool).ok_or("Could not find a trusted tkmp executable.")?;
    let env = executable(&rex.project, Path::new("/usr/bin/env")).ok_or(FAILED)?;
    let file = file.to_str().filter(|_| file.is_absolute()).ok_or(FAILED)?;
    let base = rex.arguments();
    let reply = rex.request(&format!("{base}}}")).ok_or(CONTEXT)?;
    let (instance, window) = tab(&reply).ok_or(CONTEXT)?;
    let mut record = record(&rex, instance, window)?;
    let mut bytes = [0; 64];
    let len = record.read(&mut bytes).map_err(|_| RECORD)?;
    let old = exact_block(OsStr::from_bytes(&bytes[..len])).unwrap_or("");
    let anchor = anchor_argument().map_or_else(|| "false".to_owned(), json);
    let origin = source(std::env::var(SOURCE).ok().as_deref()).unwrap_or_else(|| "false".to_owned());
    // The terminal creator inherits the server's environment. Env removes it
    // before tkmp starts, including the knobs that can write logs and dumps.
    let argv = [env.to_str().ok_or(FAILED)?, "-i", "TERM=xterm-256color", tool.to_str().ok_or(FAILED)?, file];
    let command = argv.map(json).join(",");
    let request = format!(
        r#"{base},"window":"{window}","instance":"{instance}","old":"{old}","anchor":{anchor},"source":{origin},"command":[{command}]}}"#
    );
    let reply = rex.request(&request).ok_or(FAILED)?;
    let new = exact_block(OsStr::new(&reply)).ok_or(match reply.as_str() {
        "context" => CONTEXT,
        "zoomed" => "Unzoom this Rex tab before opening Markdown preview.",
        "anchor" => ANCHOR,
        _ => FAILED,
    })?;
    record.rewind().and_then(|()| record.write_all(new.as_bytes())).map_err(|_| RECORD)?;
    record.set_len(new.len() as u64).map_err(|_| RECORD)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "session:01a20a0f-d685-72ce-b834-f985f0907d37";
    const WINDOW: &str = "window:01a20a0f-d685-72ce-b834-f985f0907d37";
    const BLOCK: &str = "block:01a20a0f-d6a9-7ee0-b4a4-d390ae1f1fd7";
    const SERVER: &str = "79319aaa-05e3-4661-ab8f-3965027c0b4e";

    #[test]
    fn ids_in_reader_provenance_and_rex_replies_are_complete() {
        let framed = format!("{SESSION} {WINDOW} shell {BLOCK}");
        assert!(source(Some(&framed)).is_some());
        assert!(source(Some(&framed.replace(BLOCK, "block:1fd7"))).is_none());
        assert!(source(Some(&framed.replace("shell", "focus"))).is_none());
        assert!(source(Some(&format!("{framed} extra"))).is_none());
        assert_eq!(tab(&format!("{SERVER}\t{WINDOW}")), Some((SERVER, WINDOW)));
        assert_eq!(tab(&format!("{SERVER} {BLOCK}")), None);
        assert_eq!(tab(&format!("{SERVER} {WINDOW} {BLOCK}")), None);
    }
}
