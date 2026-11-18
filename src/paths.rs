//! Says where milo keeps its own files.
//!
//! What the user chose lives with their other settings; what milo only
//! remembers between runs lives apart from it, so settings can be copied
//! to another machine without carrying that along.

use std::path::PathBuf;

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
