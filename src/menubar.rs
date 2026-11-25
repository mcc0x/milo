mod attention;

use std::io;
use std::path::{Path, PathBuf};

use crate::preferences;

pub(crate) use attention::{Attention, Command as AttentionCommand, USAGE as ATTENTION_USAGE, run as attention};

/// What the menu app is told of its settings file with each frame of usage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Choices {
    pub(crate) indicator: Indicator,
    pub(crate) attention: Attention,
}

impl Choices {
    /// Each choice that cannot be read is its default: the quota badge, and no watch on Rex.
    pub(crate) fn load(file: Option<&Path>) -> Self {
        let Some(file) = file else { return Self::default() };
        Self {
            indicator: Indicator::load(file).unwrap_or_default(),
            attention: Attention::load(file).unwrap_or_default(),
        }
    }
}

impl From<Indicator> for Choices {
    fn from(indicator: Indicator) -> Self {
        Self { indicator, attention: Attention::default() }
    }
}

/// Everything in the menu's settings file is read in one piece of this size or less.
const FILE_LIMIT: u64 = 4096;

/// The settings file's text with `key` set to `value` and every other line as it was.
fn with_value(text: &str, key: &str, value: &str) -> String {
    let assigns = |line: &str| line.split_once('=').is_some_and(|(name, _)| name.trim() == key);
    let mut kept: String = text.lines().filter(|line| !assigns(line)).flat_map(|line| [line, "\n"]).collect();
    kept.push_str(&format!("{key} = {value}\n"));
    kept
}

/// Sets one value in the menu's settings file under its lock. `choice` reads what is saved now.
/// A file that cannot be read as settings is replaced, since nothing in it could be kept.
fn save_value(config: &Path, key: &str, choice: impl FnOnce(&Path) -> io::Result<String>) -> io::Result<()> {
    let path = Indicator::file(config);
    preferences::locked(&path, |dir| {
        let value = choice(&path)?;
        let text = match preferences::read(&path, FILE_LIMIT) {
            Ok(text) => text.unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => String::new(),
            Err(error) => return Err(error),
        };
        preferences::replace(dir, &path, &with_value(&text, key, &value))
    })
}

/// The global menu bar display choice, independent of pane drafts and the app's quota badge mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Indicator {
    #[default]
    Quota,
    Cpu,
    Memory,
}

impl Indicator {
    pub(crate) const fn value(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::Cpu => "cpu",
            Self::Memory => "memory",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Quota => "Agent quota",
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "quota" => Some(Self::Quota),
            "cpu" => Some(Self::Cpu),
            "memory" => Some(Self::Memory),
            _ => None,
        }
    }

    pub(crate) fn file(config: &Path) -> PathBuf {
        config.join("menubar.conf")
    }

    pub(crate) fn load(file: &Path) -> io::Result<Self> {
        let Some(text) = preferences::read(file, FILE_LIMIT)? else { return Ok(Self::default()) };
        // A file that holds only other menu choices leaves the indicator at its default.
        match preferences::layer_value(&text, "indicator") {
            None => Ok(Self::default()),
            Some(value) => Self::parse(value)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid menu bar indicator")),
        }
    }

    pub(crate) fn save(self, config: &Path) -> Result<&'static str, String> {
        Self::update(config, |_| Ok(self))
    }

    pub(crate) fn advance(config: &Path, shown: Option<Self>) -> Result<&'static str, String> {
        Self::update(config, |path| {
            let current = match Self::load(path) {
                Ok(current) => Some(current),
                Err(e) if e.kind() == io::ErrorKind::InvalidData => None,
                Err(e) => return Err(e),
            };
            if current != shown {
                return Err(io::Error::other("the indicator changed elsewhere; check its new value and try again"));
            }
            Ok(match current {
                Some(Self::Quota) => Self::Cpu,
                Some(Self::Cpu) => Self::Memory,
                Some(Self::Memory) | None => Self::Quota,
            })
        })
    }

    fn update(config: &Path, choice: impl FnOnce(&Path) -> io::Result<Self>) -> Result<&'static str, String> {
        save_value(config, "indicator", |path| Ok(choice(path)?.value().to_owned()))
            .map_err(|error| format!("Could not save the menu bar indicator: {error}"))?;
        Ok("Menu bar indicator saved.")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Install,
    Uninstall,
}

/// None for builds without the installer or a destination we must not manage.
pub(crate) fn installed() -> Option<bool> {
    if !cfg!(all(target_os = "macos", milo_menu_payload)) {
        return None;
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
    if !home.is_absolute() { return None; }
    let app = home.join("Applications/Milo.app");
    match std::fs::symlink_metadata(&app) {
        Ok(info)
            if info.is_dir()
                && app
                    .join("Contents/MacOS/Milo")
                    .symlink_metadata()
                    .is_ok_and(|helper| helper.is_file()) =>
        {
            Some(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        _ => None,
    }
}

#[cfg(not(all(target_os = "macos", milo_menu_payload)))]
pub(crate) fn run(_: Action) -> Result<String, String> {
    Err(if cfg!(target_os = "macos") {
        "This Milo build has no menu app. From a checkout, run sh macos/build-menu.sh and use target/local-menu/milo."
    } else {
        "Milo requires macOS 13 or later."
    }.into())
}

#[cfg(all(target_os = "macos", milo_menu_payload))]
pub(crate) fn run(action: Action) -> Result<String, String> {
    use std::{fs, io::Write, path::PathBuf, process::{Command, Stdio}};
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    if !home.is_absolute() { return Err("HOME must be absolute".into()); }
    let apps = home.join("Applications");
    if action == Action::Install {
        fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    } else if !apps.try_exists().map_err(|e| e.to_string())? {
        return Ok("Milo is not installed.".into());
    }
    // Serialize our own upgrades and removals without leaving a lock file behind.
    let lock = fs::File::open(&apps).map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|e| format!("cannot lock Applications for the menu installer: {e}"))?;
    let mut child = Command::new("/bin/sh").args(["-c", include_str!("menubar.sh"), "milo-menubar"])
        .arg(match action { Action::Install => "install", Action::Uninstall => "uninstall" }).arg(apps)
        .arg(env!("MILO_MENU_HELPER_SHA"))
        .arg(std::env::current_exe().map_err(|e| e.to_string())?)
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
