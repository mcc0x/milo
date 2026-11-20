#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Install,
    Uninstall,
}

#[cfg(not(all(target_os = "macos", milo_menu_payload)))]
pub(crate) fn run(_: Action) -> Result<String, String> {
    Err(if cfg!(target_os = "macos") {
        "This Milo build has no menu app. From a checkout, run sh macos/build-menu.sh and use target/local-menu/milo."
    } else {
        "Milo Usage requires macOS 13 or later."
    }.into())
}

#[cfg(all(target_os = "macos", milo_menu_payload))]
pub(crate) fn run(action: Action) -> Result<String, String> {
    use std::{fs, io::Write, path::PathBuf, process::{Command, Stdio}};
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    if !home.is_absolute() { return Err("HOME must be absolute".into()); }
    let apps = home.join("Applications");
    if action == Action::Uninstall && !apps.exists() { return Ok("Milo Usage is not installed.".into()); }
    fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    // Serialize our own upgrades and removals without leaving a lock file behind.
    let lock = fs::File::open(&apps).map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|e| format!("cannot lock Applications for the menu installer: {e}"))?;
    let mut child = Command::new("/bin/sh").args(["-c", include_str!("menubar.sh"), "milo-menubar"])
        .arg(match action { Action::Install => "install", Action::Uninstall => "uninstall" }).arg(apps)
        .arg(env!("MILO_MENU_HELPER_SHA"))
        .stdin(if action == Action::Install { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    let write = child.stdin.take().map(|mut pipe| pipe.write_all(include_bytes!(env!("MILO_MENU_ARCHIVE"))));
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(error.trim().lines().last().unwrap_or("Menu app operation failed.").to_owned());
    }
    if let Some(Err(e)) = write { return Err(e.to_string()); }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
