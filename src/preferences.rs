//! Stores personal preferences outside repositories and folder choices in local state.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{fs_ops, paths};

pub use crate::git::worktree;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconMode {
    Nerd,
    Plain,
    Off,
}

impl IconMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Nerd => "Nerd Font",
            Self::Plain => "Plain",
            Self::Off => "Off",
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::Nerd => "nerd",
            Self::Plain => "plain",
            Self::Off => "off",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "nerd" => Some(Self::Nerd),
            "plain" => Some(Self::Plain),
            "off" | "none" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn environment() -> Option<Self> {
        Self::parse(&std::env::var("MILO_ICONS").ok()?)
    }

    pub fn effective(self) -> Self {
        Self::environment().unwrap_or(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMode {
    Follow,
    Pin,
}

impl ProjectMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Follow => "Follow shell",
            Self::Pin => "Pin project",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Default,
    Global,
    Worktree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub icons: IconMode,
    pub static_marks: bool,
    pub text_git_marks: bool,
    pub follow_activity: bool,
    pub changes_only: bool,
    pub project_mode: ProjectMode,
    pub history_follow_new: bool,
    pub usage_large_figures: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            icons: if cfg!(test) { IconMode::Plain } else { IconMode::Nerd },
            static_marks: false,
            text_git_marks: false,
            follow_activity: true,
            changes_only: false,
            project_mode: ProjectMode::Follow,
            history_follow_new: false,
            usage_large_figures: false,
        }
    }
}

impl Preferences {
    pub fn load(worktree: Option<&Path>) -> io::Result<Self> {
        Self::resolve(worktree).map(|(prefs, _)| prefs)
    }

    pub fn resolve(worktree: Option<&Path>) -> io::Result<(Self, Source)> {
        let Some(dir) = paths::config_dir() else { return Ok((Self::default(), Source::Default)) };
        Self::resolve_in(&dir, worktree)
    }

    fn resolve_in(dir: &Path, worktree: Option<&Path>) -> io::Result<(Self, Source)> {
        let mut prefs = Self::default();
        let mut source = Source::Default;
        for (root, layer) in [(None, Source::Global), (worktree, Source::Worktree)] {
            if layer == Source::Worktree && root.is_none() {
                break;
            }
            let file = config_file(dir, root);
            if let Some(text) = read(&file, 1 << 16)? {
                prefs.apply(&text).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", file.display())))?;
                source = layer;
            }
        }
        Ok((prefs, source))
    }

    fn apply(&mut self, text: &str) -> io::Result<()> {
        fn invalid(key: &str) -> io::Error {
            io::Error::new(io::ErrorKind::InvalidData, format!("invalid preference {key}"))
        }
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let (key, value) = line.split_once('=').ok_or_else(|| invalid("(expected key = value)"))?;
            let (key, value) = (key.trim(), value.trim());
            let boolean = || value.parse::<bool>().map_err(|_| invalid(key));
            match key {
                "icons" => self.icons = IconMode::parse(value).ok_or_else(|| invalid(key))?,
                "static_marks" => self.static_marks = boolean()?,
                "text_git_marks" => self.text_git_marks = boolean()?,
                "follow_activity" => self.follow_activity = boolean()?,
                "changes_only" => self.changes_only = boolean()?,
                "project_mode" => {
                    self.project_mode = match value {
                        "follow" => ProjectMode::Follow,
                        "pin" => ProjectMode::Pin,
                        _ => return Err(invalid(key)),
                    }
                }
                "history_follow_new" => self.history_follow_new = boolean()?,
                "usage_large_figures" => self.usage_large_figures = boolean()?,
                _ => {}
            }
        }
        Ok(())
    }

    fn encode(&self) -> String {
        let project = if self.project_mode == ProjectMode::Follow { "follow" } else { "pin" };
        format!(
            concat!(
                "icons = {}\nstatic_marks = {}\ntext_git_marks = {}\nfollow_activity = {}\n",
                "changes_only = {}\nproject_mode = {}\nhistory_follow_new = {}\nusage_large_figures = {}\n"
            ),
            self.icons.value(),
            self.static_marks,
            self.text_git_marks,
            self.follow_activity,
            self.changes_only,
            project,
            self.history_follow_new,
            self.usage_large_figures,
        )
    }

    pub fn save(&self, worktree: Option<&Path>) -> io::Result<()> {
        let dir = paths::config_dir().ok_or(io::ErrorKind::NotFound)?;
        let file = config_file(&dir, worktree);
        locked(&file, |dir| replace(dir, &file, &self.encode()))
    }

    pub fn clear(worktree: &Path) -> io::Result<()> {
        let dir = paths::config_dir().ok_or(io::ErrorKind::NotFound)?;
        let file = config_file(&dir, Some(worktree));
        if !file.parent().is_some_and(Path::exists) {
            return Ok(());
        }
        locked(&file, |_| match fs::remove_file(&file) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        })
    }

    /// The keyboard toggle overrides just this setting, leaving inherited choices inherited.
    pub fn set_changes_only(worktree: &Path, value: bool) -> io::Result<()> {
        let dir = paths::config_dir().ok_or(io::ErrorKind::NotFound)?;
        Self::set_changes_only_in(&dir, worktree, value)
    }

    fn set_changes_only_in(dir: &Path, worktree: &Path, value: bool) -> io::Result<()> {
        let file = config_file(dir, Some(worktree));
        locked(&file, |dir| {
            let old = read(&file, 1 << 16)?.unwrap_or_default();
            // Do not turn a malformed hand-edited file into a different set of preferences.
            Self::default().apply(&old)?;
            let mut text = String::with_capacity(old.len() + 24);
            for line in old.lines() {
                if !line.split_once('=').is_some_and(|(key, _)| key.trim() == "changes_only") {
                    let _ = writeln!(text, "{line}");
                }
            }
            let _ = writeln!(text, "changes_only = {value}");
            replace(dir, &file, &text)
        })
    }
}

fn config_file(dir: &Path, worktree: Option<&Path>) -> PathBuf {
    match worktree {
        Some(root) => dir.join("worktrees").join(format!("{:016x}.conf", worktree_key(root))),
        None => dir.join("preferences.conf"),
    }
}

/// A stable identifier for the checkout path, not the common git directory.
fn worktree_key(root: &Path) -> u64 {
    root.as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

fn read(path: &Path, limit: u64) -> io::Result<Option<String>> {
    let file = match fs_ops::open_read(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut text = String::new();
    let size = file.take(limit + 1).read_to_string(&mut text)?;
    if size as u64 > limit {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Some(text))
}

fn locked<T>(path: &Path, save: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    // Lock the directory because the files themselves are atomically replaced.
    let lock = fs_ops::open_dir(dir)?;
    lock.lock()?;
    save(dir)
}

fn replace(dir: &Path, path: &Path, text: &str) -> io::Result<()> {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    let (temp, mut file) = fs_ops::Temp::create(dir.join(format!(
        ".milo-prefs-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed),
    )))?;
    file.write_all(text.as_bytes())?;
    drop(file);
    temp.replace(path)
}

pub struct FoldStore(PathBuf);

impl FoldStore {
    pub fn for_worktree(root: &Path) -> Option<Self> {
        Some(Self(paths::state_dir()?.join("worktrees").join(format!("{:016x}.folds", worktree_key(root)))))
    }

    pub fn load(&self) -> io::Result<BTreeMap<String, bool>> {
        Ok(read(&self.0, 4 << 20)?.map_or_else(BTreeMap::new, |text| decode_folds(&text)))
    }

    /// Merge with choices made by other panes instead of replacing their snapshot.
    pub fn set(&self, path: &str, open: bool) -> io::Result<()> {
        self.update(|folds| folds.insert(path.to_owned(), open) != Some(open))
    }

    pub fn prune(&self, exists: impl Fn(&str) -> bool) -> io::Result<()> {
        self.update(|folds| {
            let before = folds.len();
            folds.retain(|path, _| exists(path));
            before != folds.len()
        })
    }

    fn update(&self, update: impl FnOnce(&mut BTreeMap<String, bool>) -> bool) -> io::Result<()> {
        locked(&self.0, |dir| {
            let mut folds = self.load()?;
            if update(&mut folds) {
                replace(dir, &self.0, &encode_folds(&folds))?;
            }
            Ok(())
        })
    }
}

// Folder names can contain newlines, tabs or spaces; each byte is written as hex.
fn encode_folds(folds: &BTreeMap<String, bool>) -> String {
    let mut text = String::new();
    for (name, &open) in folds {
        text.push(if open { '+' } else { '-' });
        for byte in name.bytes() {
            let _ = write!(text, "{byte:02x}");
        }
        text.push('\n');
    }
    text
}

fn decode_folds(text: &str) -> BTreeMap<String, bool> {
    text.lines()
        .filter_map(|line| {
            let (open, name) =
                line.strip_prefix('+').map(|s| (true, s)).or_else(|| line.strip_prefix('-').map(|s| (false, s)))?;
            if !name.is_ascii() || !name.len().is_multiple_of(2) {
                return None;
            }
            let bytes = (0..name.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&name[at..at + 2], 16))
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            let name = String::from_utf8(bytes).ok()?;
            (name.len() > 1 && name.ends_with('/')).then_some((name, open))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn a_partial_checkout_override_keeps_global_choices() {
        let f = Fixture::folder();
        let config = f.root.join("milo");
        let left = f.root.join("left");
        let right = f.root.join("right");
        let global = Preferences { icons: IconMode::Off, text_git_marks: true, ..Preferences::default() };
        let global_file = config_file(&config, None);
        locked(&global_file, |dir| replace(dir, &global_file, &global.encode())).unwrap();
        Preferences::set_changes_only_in(&config, &left, true).unwrap();
        assert_eq!(Preferences::resolve_in(&config, Some(&right)).unwrap(), (global, Source::Global));
        assert_eq!(
            Preferences::resolve_in(&config, Some(&left)).unwrap(),
            (Preferences { changes_only: true, ..global }, Source::Worktree),
        );
    }

    #[test]
    fn folder_state_round_trips_names_and_prunes_only_absent_folders() {
        let f = Fixture::folder();
        let path = f.root.join("state/folds");
        let a = FoldStore(path.clone());
        let b = FoldStore(path);
        a.set("docs/", false).unwrap();
        b.set("a\n猫/", true).unwrap();
        assert_eq!(a.load().unwrap(), BTreeMap::from([("a\n猫/".into(), true), ("docs/".into(), false)]));
        a.prune(|folder| folder == "a\n猫/").unwrap();
        assert_eq!(b.load().unwrap(), BTreeMap::from([("a\n猫/".into(), true)]));
    }
}
