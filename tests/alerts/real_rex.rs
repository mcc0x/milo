//! End-to-end counterpart to the deterministic stream fixtures. Never discover
//! a server: both Milo's `rex` and the test client are pinned to the lab socket.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use super::{Home, settle, soon};

struct Server {
    child: Child,
    stop: Command,
}

impl Drop for Server {
    fn drop(&mut self) {
        if !self.stop.output().is_ok_and(|out| out.status.success()) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn command(program: &Path, home: &Home) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear()
        .env("HOME", &home.root)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("XDG_CONFIG_HOME", home.root.join(".config"))
        .env("XDG_STATE_HOME", home.root.join(".local/state"))
        .env("TMPDIR", &home.root)
        .env("ZDOTDIR", &home.root)
        .current_dir(&home.root)
        .stdin(Stdio::null());
    cmd
}

#[test]
#[ignore = "requires installed Rex; runs a disposable server and a loopback destination"]
fn forwards_once_after_restarting_the_server_without_another_enable() {
    let home = Home::new().unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let rex = std::env::var_os("MILO_TEST_REX")
        .map(PathBuf::from)
        .or_else(|| std::env::split_paths(&path).map(|dir| dir.join("rex")).find(|path| path.is_file()))
        .expect("set MILO_TEST_REX to the rex executable")
        .canonicalize()
        .unwrap();
    let socket = format!("unix://{}", home.root.join("r.sock").display());
    let wrapper = home.root.join("bin/rex");
    assert!(!rex.to_string_lossy().contains('\'') && !socket.contains('\''));
    fs::write(&wrapper, format!("#!/bin/sh\nexec '{}' --autostart=false -S '{socket}' \"$@\"\n", rex.display()))
        .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let config = home.root.join("init.lua");
    fs::write(&config, "").unwrap();
    let start = || {
        let child = command(&rex, &home)
            .args(["server", "run", "--restore=false", "--listen", &socket, "--state-dir"])
            .arg(home.root.join("rex-state"))
            .arg("--config")
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stop = command(&wrapper, &home);
        stop.args(["server", "stop"]);
        let server = Server { child, stop };
        assert!(
            soon(|| command(&wrapper, &home)
                .args(["whoami", "--json"])
                .output()
                .is_ok_and(|out| { out.status.success() && String::from_utf8_lossy(&out.stdout).contains(&socket) })),
            "disposable Rex did not start"
        );
        server
    };
    let emit = |state| {
        // Rex does not replay events that predate the subscription. Repeat the
        // same OSC in the same terminal; repeats must not create extra alerts.
        let script = format!(
            "for i in 1 2 3 4 5 6 7 8; do printf '\\033]7501;state={state}:app=synthetic\\033\\\\'; sleep 1; done"
        );
        let result = command(&wrapper, &home)
            .args(["new", "synthetic", "--shell", "none", "--", "/bin/sh", "-c", &script])
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    };
    let server = start();
    home.save_discord();
    home.run(&["alerts", "on", "discord"]);
    emit("blocked:kind=permission");
    assert!(home.received(1), "{}", home.run(&["alerts"]));
    drop(server);
    assert!(soon(|| home.run(&["alerts"]).to_ascii_lowercase().contains("waiting for rex")));
    let _restarted = start();
    emit("done");
    assert!(home.received(2), "{}", home.run(&["alerts"]));
    settle();
    assert_eq!(home.destination.texts(), ["`synthetic` **is waiting for permission**", "`synthetic` **finished**"]);
}
