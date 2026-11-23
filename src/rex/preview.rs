//! Native previews are attached only to the local Rex server of their terminal.

use std::ffi::OsStr;
use std::os::fd::RawFd;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::json::Value;
use crate::preferences::{Preferences, PreviewPlacement};
use crate::preview::{Anchor, StartupSocket};

use super::{CALL_WAIT, Target, exact_block, exact_id, json, program, scripted};

const SCRIPT: &str = include_str!("preview.lua");
const PUBLICATION_WAIT: Duration = Duration::from_secs(5);

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
        self.request_within(mode, extra, CALL_WAIT)
    }

    fn request_within(&self, mode: &str, extra: &str, wait: Duration) -> Result<String, String> {
        scripted(self.command().args(["--args", "-"]), &self.args(mode, extra), wait)
            .ok_or_else(|| "couldn't reach the terminal's local Rex server".into())
    }

    fn call(&self, mode: &str, extra: &str) -> Result<(), String> {
        parse_status(self.request(mode, extra)?)
    }

    fn publish(&self, extra: &str) -> Result<(), String> {
        publish_with_readback(|mode| self.request_within(mode, extra, PUBLICATION_WAIT))
    }
}

fn parse_status(reply: String) -> Result<(), String> {
    if reply == "ok" { Ok(()) } else { Err(problem(&reply).into()) }
}

fn publish_with_readback(mut request: impl FnMut(&str) -> Result<String, String>) -> Result<(), String> {
    let published = request("publish").and_then(parse_status);
    // A lost reply can follow a successful label change. Don't make another
    // mutation: read back the exact, still-authenticated placement once.
    if published.is_err() && request("published").and_then(parse_status).is_ok() {
        Ok(())
    } else {
        published
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
        "changed" => "the Rex tab, viewer, or working directory changed; run preview again",
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

fn resolve_placement(
    working_cwd: &Path,
    override_placement: Option<PreviewPlacement>,
    config_dir: impl FnOnce() -> Option<PathBuf>,
) -> PreviewPlacement {
    override_placement.unwrap_or_else(|| {
        let Some(dir) = config_dir() else { return Preferences::default().preview_placement };
        let root = crate::git::worktree(working_cwd);
        Preferences::for_pane_in(&dir, root.as_deref()).0.preview_placement
    })
}

fn anchor_args(anchor: Anchor<'_>) -> String {
    format!(
        r#","main":{},"window":{},"owner":{},"owner_pid":{},"native_window_id":{}"#,
        json(anchor.main),
        json(anchor.window),
        json(anchor.owner),
        anchor.owner_pid,
        anchor.native_window_id,
    )
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
    let [main, window, cwd, owner, owner_pid] = Value::parse(reply.as_bytes())
        .ok_or_else(|| problem(&reply).to_owned())?
        .fields(["main", "window", "cwd", "owner", "owner_pid"]);
    let main = main
        .and_then(|value| value.string(42))
        .filter(|id| exact_block(OsStr::new(id.as_ref())).is_some())
        .ok_or("Rex did not identify the working terminal")?;
    let window = window
        .and_then(|value| value.string(43))
        .filter(|id| exact_id(OsStr::new(id.as_ref()), "window:").is_some())
        .ok_or("Rex did not identify the working terminal's tab")?;
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
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| (2..=2_147_483_647).contains(pid))
        .ok_or("Rex did not identify the working terminal's viewer PID")?;
    let native_window_id = crate::preview::pin_window(owner_pid)?;
    let placement = resolve_placement(Path::new(cwd.as_ref()), override_placement, crate::paths::config_dir);
    let anchor = anchor_args(Anchor { main: &main, window: &window, owner: &owner, owner_pid, native_window_id });
    let startup = StartupSocket::new().map_err(|_| "couldn't create the private preview startup socket")?;
    let extra = format!(
        r#","exe":{},"startup":{},"placement":{},"cwd":{}{anchor}"#,
        json(exe),
        json(startup.path()),
        json(placement.value()),
        json(&cwd),
    );
    // Rex sees only the one-shot socket name, never the address. Confirm
    // that the connected peer is the placed terminal child before sending
    // bytes, then publish only after the helper has initialized.
    let launched = connection.request("open", &extra)?;
    let tile = exact_block(OsStr::new(&launched)).ok_or_else(|| problem(&launched).to_owned())?;
    let staged = format!(r#","tile":{},"placement":{}{anchor}"#, json(tile), json(placement.value()));
    let approve = |pid| format!(r#"{staged},"anchor_pid":{pid}"#);
    let result = startup.start(
        url,
        |pid| connection.call("authorize", &approve(pid)),
        |pid| connection.publish(&approve(pid)),
    );
    if let Err(error) = result {
        let _ = connection.call("cancel", &staged);
        return Err(error);
    }
    eprintln!(
        "Preview tile opened. Close it before switching sessions or hosts: the page can stay over another session in this Mac window."
    );
    Ok(())
}

pub(crate) fn preview_observer_command(anchor: Anchor<'_>, startup_fd: RawFd) -> Result<Command, String> {
    let connection = Connection::local()?;
    let mut command = connection.command();
    let scope = format!(r#"{},"startup_fd":{startup_fd}"#, anchor_args(anchor));
    command
        .args(["--args", &connection.args("watch", &scope)])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

pub(crate) fn return_preview_focus(anchor: Anchor<'_>) -> Result<(), String> {
    Connection::local()?.call("focus", &anchor_args(anchor))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{publish_with_readback, resolve_placement, socket_path};
    use crate::fixture::Fixture;
    use crate::preferences::{PreviewPlacement, layer_file};

    #[test]
    fn a_lost_publication_reply_requires_one_readback_and_never_republishes() {
        for (publish, verified) in [(Ok("ok"), None), (Err("reply lost"), Some("ok")), (Err("reply lost"), Some("changed"))] {
            let mut calls = Vec::new();
            let result = publish_with_readback(|mode| {
                calls.push(mode.to_owned());
                match mode {
                    "publish" => publish.map(str::to_owned).map_err(str::to_owned),
                    "published" => Ok(verified.expect("readback only after a lost reply").into()),
                    _ => unreachable!(),
                }
            });
            if let Some(verified) = verified {
                assert_eq!(calls, ["publish", "published"]);
                assert_eq!(result, if verified == "ok" { Ok(()) } else { Err("reply lost".into()) });
            } else {
                assert_eq!(calls, ["publish"]);
                assert_eq!(result, Ok(()));
            }
        }
    }

    #[test]
    fn placement_uses_the_working_checkout_and_never_persists_the_override() {
        use PreviewPlacement::{Above, Below, Left, Right};

        let config = Fixture::folder();
        let working = Fixture::new();
        let caller = Fixture::new();
        let working_cwd = working.root.join("nested");
        std::fs::create_dir(&working_cwd).unwrap();
        let working_root = crate::git::worktree(&working_cwd).unwrap();
        let caller_root = crate::git::worktree(&caller.root).unwrap();
        let global = layer_file(&config.root, None);
        let local = layer_file(&config.root, Some(&working_root));
        let caller_local = layer_file(&config.root, Some(&caller_root));
        std::fs::create_dir(local.parent().unwrap()).unwrap();
        let layers = [(global, "left"), (local, "above"), (caller_local, "floating")];
        for (path, value) in &layers {
            std::fs::write(path, format!("preview_placement = {value}\n")).unwrap();
        }
        let dir = || Some(config.root.clone());
        assert_eq!(resolve_placement(&working_cwd, None, dir), Above);
        assert_eq!(resolve_placement(&caller.root, None, dir), Right);
        assert_eq!(resolve_placement(&config.root, None, dir), Left);
        assert_eq!(resolve_placement(&working_cwd, Some(Below), || panic!("override reads settings")), Below);
        for (path, value) in &layers {
            assert_eq!(std::fs::read_to_string(path).unwrap(), format!("preview_placement = {value}\n"));
        }
    }

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
