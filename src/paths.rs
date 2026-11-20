//! Finds Milo's own files and the files used by the active Claude Code profile.
//!
//! What the user chose lives with their other settings; what milo only
//! remembers between runs lives apart from it, so settings can be copied
//! to another machine without carrying that along.

use std::os::unix::ffi::OsStrExt as _;
use std::path::{self, Path, PathBuf};

/// The user's home directory, if the environment names one.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from)
}

/// A directory named by `variable` if it holds an absolute path, as the XDG
/// convention asks, and otherwise `fallback` under the home directory.
fn base(variable: &str, fallback: &str) -> Option<PathBuf> {
    let named = std::env::var_os(variable).map(PathBuf::from).filter(|dir| dir.is_absolute());
    named.or_else(|| Some(home()?.join(fallback)))
}

/// Where what the user chose in `milo setup` is kept.
pub fn config_dir() -> Option<PathBuf> {
    Some(base("XDG_CONFIG_HOME", ".config")?.join("milo"))
}

/// Where milo keeps what it only remembers between runs.
pub fn state_dir() -> Option<PathBuf> {
    Some(base("XDG_STATE_HOME", ".local/state")?.join("milo"))
}

pub(crate) enum ClaudePaths {
    Default(PathBuf),
    Override(PathBuf),
}

pub(crate) fn claude() -> Option<ClaudePaths> {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => Some(ClaudePaths::Override(path::absolute(dir).ok()?)),
        None => Some(ClaudePaths::Default(home()?)),
    }
}

impl ClaudePaths {
    pub(crate) fn settings(&self) -> PathBuf {
        match self {
            Self::Default(home) => home.join(".claude/settings.json"),
            Self::Override(dir) => dir.join("settings.json"),
        }
    }

    pub(crate) fn projects(&self) -> PathBuf {
        match self {
            Self::Default(home) => home.join(".claude/projects"),
            Self::Override(dir) => dir.join("projects"),
        }
    }

    pub(crate) fn config(&self) -> PathBuf {
        match self {
            Self::Default(home) => home.join(".claude.json"),
            Self::Override(dir) => dir.join(".claude.json"),
        }
    }

    /// Default installations retain their existing ownership note and readings.
    pub(crate) fn state(&self) -> Option<PathBuf> {
        let state = state_dir()?;
        Some(self.state_override(&state).unwrap_or(state))
    }

    pub(crate) fn state_override(&self, state: &Path) -> Option<PathBuf> {
        let Self::Override(dir) = self else { return None };
        let key = dir
            .as_os_str()
            .as_bytes()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
        Some(state.join("claude-profiles").join(format!("{key:016x}")))
    }
}
