//! Native previews are attached only to the local Rex server of their terminal.

use std::ffi::OsStr;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::json::Value;
use crate::preferences::{Preferences, PreviewPlacement};

use super::{CALL_WAIT, Target, exact_block, exact_id, json, program, scripted};

const SCRIPT: &str = include_str!("preview.lua");

struct Connection {
    target: Target,
    server: String,
    uid: u32,
}

impl Connection {
    fn local() -> Result<Self, String> {
        if ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"].iter().any(|name| std::env::var_os(name).is_some()) {
            return Err("preview isn't available from an SSH session".into());
        }
        let valid = |target: &Target| {
            exact_id(&target.session, "session:").is_some() && exact_block(&target.block).is_some()
        };
        let target = Target::current().filter(valid).ok_or("open preview from a local Rex terminal")?;
        let server = std::env::var("REX_SERVER").map_err(|_| "preview needs the terminal's explicit local Rex socket")?;
        let path = socket_path(&server).ok_or("preview needs the terminal's explicit local Rex socket")?;
        // SAFETY: geteuid reads the process's own effective user ID.
        let uid = unsafe { libc::geteuid() };
        if !path.symlink_metadata().is_ok_and(|meta| meta.file_type().is_socket() && meta.uid() == uid) {
            return Err("the terminal's local Rex socket is unavailable or belongs to another user".into());
        }
        Ok(Self { target, server, uid })
    }

    fn command(&self) -> Command {
        let rex = std::env::var_os("REX_BIN_DIR")
            .map(PathBuf::from)
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join("rex"))
            .filter(|path| path.is_file())
            .map_or_else(program, PathBuf::into_os_string);
        let mut command = Command::new(rex);
        command
            .args(["--autostart=false", "--timeout", "3s", "--server", &self.server, "do", "--session"])
            .arg(&self.target.session)
            .arg("--block")
            .arg(&self.target.block)
            .args(["--fail-fast", "-e", SCRIPT])
            .env_remove("REX_CLIENT")
            .env_remove("REX_SERVER");
        command
    }

    fn args(&self, mode: &str, extra: &str) -> String {
        format!(
            r#"{{"mode":{},"server":{},"uid":{},"pid":{},"session":{},"block":{}{extra}}}"#,
            json(mode),
            json(&self.server),
            self.uid,
            std::process::id(),
            json(self.target.session.to_str().expect("exact session ID")),
            json(self.target.block.to_str().expect("exact block ID")),
        )
    }

    fn request(&self, mode: &str, extra: &str) -> Result<String, String> {
        scripted(self.command().args(["--args", "-"]), &self.args(mode, extra), CALL_WAIT)
            .ok_or_else(|| "couldn't reach the terminal's local Rex server".into())
    }

    fn call(&self, mode: &str, extra: &str) -> Result<(), String> {
        let reply = self.request(mode, extra)?;
        if reply == "ok" { Ok(()) } else { Err(problem(&reply).into()) }
    }
}

fn problem(reply: &str) -> &'static str {
    match reply {
        "transport" => "couldn't prove Rex is connected directly through the terminal's local socket",
        "tab" => "open the terminal's Rex tab and run preview there",
        "zoom" => "unzoom the Rex tab before opening a preview",
        "floating_caller" => "run preview from the tiled working terminal or Milo sidebar, not a floating pane",
        "caller" => "the running Milo process is not in the specified Rex terminal",
        "client" => "the terminal isn't owned by a connected Rex app on this Mac",
        "shell" => "couldn't identify one interactive local working terminal in this Rex tab",
        "stale" => "the sidebar's working terminal is stale or is not in this Rex tab",
        "remote" => "preview is not available for a remote Rex terminal",
        "focus" => "Rex couldn't return focus to the working terminal",
        "placement" => "Rex couldn't place a preview in the working terminal's tab",
        "changed" => "the working terminal or its directory changed; run preview again",
        _ => "the local Rex server returned an unexpected preview response",
    }
}

/// Rex exports an escaped Unix socket URI. Do not accept host labels, network
/// addresses, URI authorities, or relative paths which the CLI could resolve elsewhere.
fn socket_path(server: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;

    let mut uri = server.strip_prefix("unix://")?.as_bytes();
    if !uri.starts_with(b"/") || uri.starts_with(b"//") {
        return None;
    }
    let mut path = Vec::with_capacity(uri.len());
    while let Some((&first, rest)) = uri.split_first() {
        uri = rest;
        let byte = match first {
            b'?' | b'#' => return None,
            b'%' => {
                let (&hi, rest) = uri.split_first()?;
                let (&lo, rest) = rest.split_first()?;
                uri = rest;
                let hex = |c| match c {
                    b'0'..=b'9' => Some(c - b'0'),
                    b'a'..=b'f' => Some(c - b'a' + 10),
                    b'A'..=b'F' => Some(c - b'A' + 10),
                    _ => None,
                };
                (hex(hi)? << 4) | hex(lo)?
            }
            byte => byte,
        };
        if byte == 0 || byte.is_ascii_control() {
            return None;
        }
        path.push(byte);
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(path));
    (path.is_absolute() && path.file_name().is_some()).then_some(path)
}

pub(crate) fn open_preview(url: &str, override_placement: Option<PreviewPlacement>) -> Result<(), String> {
    let url = crate::preview::url(url)?;
    let connection = Connection::local()?;
    let exe = std::env::current_exe().map_err(|_| "couldn't find the running Milo executable")?;
    let exe = exe
        .to_str()
        .filter(|path| Path::new(path).is_absolute())
        .ok_or("Milo needs an absolute UTF-8 executable path")?;
    let reply = connection.request("identify", &format!(r#","exe":{}"#, json(exe)))?;
    let [main, cwd, owner, owner_pid] = Value::parse(reply.as_bytes())
        .ok_or_else(|| problem(&reply).to_owned())?
        .fields(["main", "cwd", "owner", "owner_pid"]);
    let main = main
        .and_then(|value| value.string(42))
        .filter(|id| exact_block(OsStr::new(id.as_ref())).is_some())
        .ok_or("Rex did not identify the working terminal")?;
    let cwd = cwd
        .and_then(|value| value.string(4096))
        .filter(|cwd| !cwd.contains('\0') && Path::new(cwd.as_ref()).is_absolute())
        .ok_or("Rex did not identify the working terminal's directory")?;
    let owner = owner
        .and_then(|value| value.string(43))
        .filter(|id| exact_id(OsStr::new(id.as_ref()), "client:").is_some())
        .ok_or("Rex did not identify the working terminal's viewer")?;
    let owner_pid = owner_pid
        .and_then(Value::unsigned)
        .filter(|pid| (2..=2_147_483_647).contains(pid))
        .ok_or("Rex did not identify the working terminal's viewer PID")?;
    crate::preview::require_access()?;
    let placement = override_placement.unwrap_or_else(|| {
        let root = crate::git::worktree(Path::new(cwd.as_ref()));
        Preferences::for_pane(root.as_deref()).0.preview_placement
    });
    let extra = format!(
        r#","exe":{},"url":{},"placement":{},"main":{},"cwd":{},"owner":{},"owner_pid":{owner_pid}"#,
        json(exe),
        json(url),
        json(placement.value()),
        json(&main),
        json(&cwd),
        json(&owner),
    );
    // Arguments go through stdin. The URL is never in Rex CLI argv or an error.
    connection.call("open", &extra)
}

pub(crate) fn preview_watch(owner: &str, owner_pid: u32) -> Result<Child, String> {
    let connection = Connection::local()?;
    let extra = format!(r#","owner":{},"owner_pid":{owner_pid}"#, json(owner));
    connection
        .command()
        .args(["--args", &connection.args("watch", &extra)])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "couldn't start the local Rex preview observer".into())
}

pub(crate) fn return_preview_focus(main: &str, owner: &str, owner_pid: u32) -> Result<(), String> {
    let connection = Connection::local()?;
    let extra = format!(r#","main":{},"owner":{},"owner_pid":{owner_pid}"#, json(main), json(owner));
    connection.call("focus", &extra)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::socket_path;

    #[test]
    fn preview_requires_an_explicit_absolute_unix_socket() {
        assert_eq!(socket_path("unix:///tmp/rex%20lab%23unix.sock").as_deref(), Some(Path::new("/tmp/rex lab#unix.sock")));
        for endpoint in [
            "devbox", "127.0.0.1:7777", "ssh://localhost/rex", "unix://localhost/tmp/rex.sock", "unix://relative",
            "unix:////tmp/rex.sock", "unix:///%00rex.sock", "unix:///tmp/%ZZ", "unix:///tmp/rex.sock?q=x",
        ] {
            assert!(socket_path(endpoint).is_none(), "accepted {endpoint}");
        }
    }

    #[test]
    #[ignore = "requires Lua 5.1"]
    fn preview_rex_lua_fixtures() {
        let lua = std::env::var_os("LUA51").unwrap_or_else(|| "lua5.1".into());
        assert!(
            std::process::Command::new(lua)
                .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src/rex"))
                .args(["preview_test.lua", "preview.lua"])
                .status()
                .expect("install Lua 5.1 or set LUA51 to its executable")
                .success()
        );
    }
}
