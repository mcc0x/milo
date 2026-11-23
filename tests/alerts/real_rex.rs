//! End-to-end counterpart to the deterministic stream fixtures. Never discover
//! a server: both Milo's `rex` and the test client are pinned to the lab socket.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use super::{DISCORD, ENABLED, Home, PAUSED, RECOVERED, TELEGRAM, settle, soon};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            // SAFETY: This is our unreaped disposable server child, not a discovered PID.
            unsafe { libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM) };
            if !soon(|| self.0.try_wait().is_ok_and(|status| status.is_some())) {
                let _ = self.0.kill();
            }
        }
        let _ = self.0.wait();
    }
}

fn command(program: &Path, home: &Home) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear()
        .env("HOME", &home.root)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("XDG_CONFIG_HOME", home.root.join(".config"))
        .env("XDG_STATE_HOME", home.root.join(".local/state"))
        .env("XDG_RUNTIME_DIR", &home.root)
        .env("TMPDIR", &home.root)
        .env("ZDOTDIR", &home.root)
        .current_dir(&home.root)
        .stdin(Stdio::null());
    cmd
}

fn recover(origin: bool) {
    let home = Home::new_unbound().unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let rex = std::env::var_os("MILO_TEST_REX")
        .map(PathBuf::from)
        .or_else(|| std::env::split_paths(&path).map(|dir| dir.join("rex")).find(|path| path.is_file()))
        .expect("set MILO_TEST_REX to the rex executable")
        .canonicalize()
        .unwrap();
    let socket = home.rex_endpoint();
    let wrapper = home.root.join("bin/rex");
    assert!(!rex.to_string_lossy().contains('\'') && !socket.contains('\''));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nrex='{}'\nendpoint='{socket}'\n{}",
            rex.display(),
            r#"selecting=no
for arg do
    if [ "$selecting" = yes ]; then
        [ "$arg" = "$endpoint" ] || exit 1
        selecting=no
        continue
    fi
    case "$arg" in
        -S|--server) selecting=yes ;;
        -S*|--server=*|--autostart) exit 1 ;;
        --autostart=*) [ "$arg" = --autostart=false ] || exit 1 ;;
        --) break ;;
    esac
done
[ "$selecting" = no ] && [ -S "${endpoint#unix://}" ] || exit 1
exec "$rex" --autostart=false -S "$endpoint" "$@"
"#,
        ),
    )
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
        let server = Server(child);
        assert!(
            soon(|| command(&wrapper, &home)
                .args(["whoami", "--json"])
                .output()
                .is_ok_and(|out| { out.status.success() && String::from_utf8_lossy(&out.stdout).contains(&socket) })),
            "disposable Rex did not start"
        );
        server
    };
    let emit = |label, state| {
        // Rex does not replay events that predate the subscription. Repeat the
        // same OSC in the same terminal; repeats must not create extra alerts.
        let script = format!(
            "for i in 1 2 3 4 5 6 7 8; do \
             printf '\\033]7501;state={state}:app=synthetic:title=cHJpdmF0ZSBwcm9tcHQ=\\033\\\\'; sleep 1; done"
        );
        let result = command(&wrapper, &home)
            .args(["new", label, "--shell", "none", "--", "/bin/sh", "-c", &script])
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    };
    let server = start();
    home.save_telegram();
    home.save_discord();
    if origin {
        home.run(&["alerts", "origin", "on"]);
    }
    let mut telegram = Vec::new();
    let mut discord = Vec::new();
    for provider in ["telegram", "discord"] {
        if origin {
            home.run(&["alerts", "notices", "on", provider]);
        }
        home.run(&["alerts", "on", provider]);
    }
    if origin {
        assert!(home.received(2), "{}", home.run(&["alerts"]));
        telegram.push(ENABLED);
        discord.push(ENABLED);
    }
    home.wait_for_health("Listening");
    emit("before-restart", "blocked:kind=permission");
    telegram.push(if origin {
        "Rex: <code>synthetic</code> is waiting for permission · <code>before-restart</code>"
    } else {
        "Rex: <code>synthetic</code> is waiting for permission"
    });
    discord.push(if origin {
        "`synthetic` **is waiting for permission** · `before-restart`"
    } else {
        "`synthetic` **is waiting for permission**"
    });
    assert!(home.received(telegram.len() + discord.len()), "{}", home.run(&["alerts"]));
    assert_eq!(home.destination.texts_at(TELEGRAM), telegram);
    assert_eq!(home.destination.texts_at(DISCORD), discord);

    drop(server);
    home.wait_for_health("Waiting for Rex");
    home.run(&["alerts", "off", "telegram"]);
    if origin {
        telegram.push(PAUSED);
        assert!(home.received(telegram.len() + discord.len()));
    }
    let _restarted = start();
    home.wait_for_health("Listening");
    if origin {
        discord.push(RECOVERED);
        assert!(home.received(telegram.len() + discord.len()));
    }
    emit("after-restart", "done");
    discord.push(if origin { "`synthetic` **finished** · `after-restart`" } else { "`synthetic` **finished**" });
    assert!(home.received(telegram.len() + discord.len()), "{}", home.run(&["alerts"]));
    settle();
    assert_eq!(home.destination.texts_at(TELEGRAM), telegram);
    assert_eq!(home.destination.texts_at(DISCORD), discord);
}

#[test]
#[ignore = "requires installed Rex; runs a disposable server and loopback destinations"]
fn forwards_once_after_restarting_the_server_without_another_enable() {
    recover(false);
}

#[test]
#[ignore = "requires installed Rex; runs a disposable server and loopback destinations"]
fn refreshes_origin_after_restarting_the_server_without_leaking_it_into_notices() {
    recover(true);
}
