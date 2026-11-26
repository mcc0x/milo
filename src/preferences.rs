//! Stores personal preferences outside repositories and folder choices in local state.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{fs_ops, paths};

const PREFERENCES_LIMIT: u64 = 1 << 16;
const FOLDS_LIMIT: u64 = 4 << 20;

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

/// A tool whose figures the usage pane can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageTool {
    Codex,
    Claude,
    OpenCode,
}

impl UsageTool {
    pub const ALL: [Self; 3] = [Self::Codex, Self::Claude, Self::OpenCode];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            Self::OpenCode => "OpenCode",
        }
    }

    /// The tool's name in the preferences file and in the browser editor's fields.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Codex => "usage_codex",
            Self::Claude => "usage_claude",
            Self::OpenCode => "usage_opencode",
        }
    }
}

/// Whether the usage pane gives a tool a place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Show {
    /// Only while the tool has a figure saved on this machine.
    #[default]
    Auto,
    /// Even when all Milo can say about it is that nothing is known.
    Always,
    /// Never, and without looking for its data.
    Hide,
}

impl Show {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Always, Self::Hide];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Always => "Always show",
            Self::Hide => "Hide",
        }
    }

    pub const fn value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Hide => "hide",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|show| show.value() == value)
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Auto => Self::Always,
            Self::Always => Self::Hide,
            Self::Hide => Self::Auto,
        }
    }
}

/// One choice for each tool, in `UsageTool::ALL` order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageTools([Show; 3]);

impl std::ops::Index<UsageTool> for UsageTools {
    type Output = Show;

    fn index(&self, tool: UsageTool) -> &Show {
        &self.0[tool as usize]
    }
}

impl std::ops::IndexMut<UsageTool> for UsageTools {
    fn index_mut(&mut self, tool: UsageTool) -> &mut Show {
        &mut self.0[tool as usize]
    }
}

/// An account whose figures only its provider's service can report. Unlike a
/// tool, nothing on this machine says it is in use, so each starts hidden and
/// Milo asks the provider only after the user has chosen to show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageAccount {
    DeepSeek,
    OpenRouter,
    Devin,
    Copilot,
}

impl UsageAccount {
    pub const ALL: [Self; 4] = [Self::DeepSeek, Self::OpenRouter, Self::Devin, Self::Copilot];

    pub const fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "DeepSeek",
            Self::OpenRouter => "OpenRouter",
            Self::Devin => "Devin",
            Self::Copilot => "Copilot",
        }
    }

    /// The account's name in the preferences file and in the browser editor's fields.
    pub const fn key(self) -> &'static str {
        match self {
            Self::DeepSeek => "usage_deepseek",
            Self::OpenRouter => "usage_openrouter",
            Self::Devin => "usage_devin",
            Self::Copilot => "usage_copilot",
        }
    }
}

/// One choice for each account, in `UsageAccount::ALL` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsageAccounts([Show; 4]);

impl Default for UsageAccounts {
    fn default() -> Self {
        Self([Show::Hide; 4])
    }
}

impl UsageAccounts {
    /// Whether any account may be asked about at all.
    pub fn any(self) -> bool {
        self.0.iter().any(|&show| show != Show::Hide)
    }
}

impl std::ops::Index<UsageAccount> for UsageAccounts {
    type Output = Show;

    fn index(&self, account: UsageAccount) -> &Show {
        &self.0[account as usize]
    }
}

impl std::ops::IndexMut<UsageAccount> for UsageAccounts {
    fn index_mut(&mut self, account: UsageAccount) -> &mut Show {
        &mut self.0[account as usize]
    }
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
    pub usage_tools: UsageTools,
    pub usage_accounts: UsageAccounts,
    /// Whether the Rex listener may title tabs that nobody has named.
    pub tab_titles: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            icons: if !cfg!(test) && crate::rex::inside() { IconMode::Nerd } else { IconMode::Plain },
            static_marks: false,
            text_git_marks: false,
            follow_activity: true,
            changes_only: false,
            project_mode: ProjectMode::Follow,
            history_follow_new: false,
            usage_large_figures: false,
            usage_tools: UsageTools::default(),
            usage_accounts: UsageAccounts::default(),
            tab_titles: false,
        }
    }
}

/// Keep the layers observed by an editor so stale tabs cannot overwrite settings.
pub(crate) struct PreferenceSnapshot {
    global: Option<String>,
    local: Option<String>,
    pub prefs: Preferences,
    pub source: Source,
}

impl PreferenceSnapshot {
    pub(crate) fn load(dir: &Path, root: Option<&Path>) -> io::Result<Self> {
        let (global, local) = Self::read_layers(dir, root)?;
        let mut prefs = Preferences::default();
        let mut source = Source::Default;
        for (layer, origin) in [(&global, Source::Global), (&local, Source::Worktree)] {
            if let Some(text) = layer {
                prefs.apply(text)?;
                source = origin;
            }
        }
        Ok(Self { global, local, prefs, source })
    }

    fn read_layers(dir: &Path, root: Option<&Path>) -> io::Result<(Option<String>, Option<String>)> {
        let global = read(&config_file(dir, None), PREFERENCES_LIMIT)?;
        let local = root.map(|root| read(&config_file(dir, Some(root)), PREFERENCES_LIMIT)).transpose()?.flatten();
        Ok((global, local))
    }

    fn check(&self, dir: &Path, root: Option<&Path>) -> io::Result<()> {
        let (global, local) = Self::read_layers(dir, root)?;
        if self.global == global && self.local == local { Ok(()) } else { Err(io::ErrorKind::Interrupted.into()) }
    }

    pub(crate) fn save(&self, prefs: Preferences, dir: &Path, root: Option<&Path>, local: bool) -> io::Result<()> {
        let destination = if local { Some(root.ok_or(io::ErrorKind::InvalidInput)?) } else { None };
        let path = config_file(dir, destination);
        // For a worktree, lock its inherited global layer before locking the
        // destination. Global and keyboard writes use the same directory locks.
        let _global = if local { Some(lock_directory(dir)?) } else { None };
        prefs.save_in(self.prefs, &path, || self.check(dir, root))
    }

    pub(crate) fn inherit(&self, dir: &Path, root: &Path) -> io::Result<()> {
        let _global = lock_directory(dir)?;
        remove_layer(&config_file(dir, Some(root)), || self.check(dir, Some(root)))
    }
}

impl Preferences {
    /// Panes can use the last valid layer, but setup and Doctor must resolve strictly.
    pub fn for_pane(worktree: Option<&Path>) -> (Self, Option<&'static str>) {
        if cfg!(test) {
            return (Self::default(), None);
        }
        paths::config_dir().map_or_else(|| (Self::default(), None), |dir| Self::for_pane_in(&dir, worktree))
    }

    pub(crate) fn for_pane_in(dir: &Path, worktree: Option<&Path>) -> (Self, Option<&'static str>) {
        let Ok((global, _)) = Self::resolve_in(dir, None) else {
            return (Self::default(), Some("milo doctor: global prefs ignored"));
        };
        let Some(root) = worktree else { return (global, None) };
        match global.read_layer(&config_file(dir, Some(root))) {
            Ok(prefs) => (prefs.unwrap_or(global), None),
            Err(_) => (global, Some("milo doctor: worktree prefs ignored")),
        }
    }

    #[cfg(test)]
    pub fn resolve(worktree: Option<&Path>) -> io::Result<(Self, Source)> {
        let Some(dir) = paths::config_dir() else { return Ok((Self::default(), Source::Default)) };
        Self::resolve_in(&dir, worktree)
    }

    pub(crate) fn resolve_in(dir: &Path, worktree: Option<&Path>) -> io::Result<(Self, Source)> {
        let mut prefs = Self::default();
        let mut source = Source::Default;
        for (root, layer) in [(None, Source::Global), (worktree, Source::Worktree)] {
            if layer == Source::Worktree && root.is_none() {
                break;
            }
            let file = config_file(dir, root);
            let error_at = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", file.display()));
            if let Some(resolved) = prefs.read_layer(&file).map_err(error_at)? {
                prefs = resolved;
                source = layer;
            }
        }
        Ok((prefs, source))
    }

    /// Parse into a copy so a broken layer cannot leak any partial edits.
    fn read_layer(mut self, file: &Path) -> io::Result<Option<Self>> {
        let Some(text) = read(file, PREFERENCES_LIMIT)? else { return Ok(None) };
        self.apply(&text)?;
        Ok(Some(self))
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
                "tab_titles" => self.tab_titles = boolean()?,
                _ => {
                    if let Some(tool) = UsageTool::ALL.into_iter().find(|tool| tool.key() == key) {
                        self.usage_tools[tool] = Show::parse(value).ok_or_else(|| invalid(key))?;
                    } else if let Some(account) = UsageAccount::ALL.into_iter().find(|account| account.key() == key) {
                        self.usage_accounts[account] = Show::parse(value).ok_or_else(|| invalid(key))?;
                    }
                }
            }
        }
        Ok(())
    }

    fn save_in(&self, before: Self, path: &Path, check: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        let boolean = |old, new| (old != new).then(|| bool_value(new));
        let tool = |tool: UsageTool| {
            let show = self.usage_tools[tool];
            (tool.key(), (before.usage_tools[tool] != show).then(|| show.value()))
        };
        let account = |account: UsageAccount| {
            let show = self.usage_accounts[account];
            (account.key(), (before.usage_accounts[account] != show).then(|| show.value()))
        };
        update_guarded(
            path,
            &[
                ("icons", (before.icons != self.icons).then(|| self.icons.value())),
                ("static_marks", boolean(before.static_marks, self.static_marks)),
                ("text_git_marks", boolean(before.text_git_marks, self.text_git_marks)),
                ("follow_activity", boolean(before.follow_activity, self.follow_activity)),
                ("changes_only", boolean(before.changes_only, self.changes_only)),
                (
                    "project_mode",
                    (before.project_mode != self.project_mode).then_some(match self.project_mode {
                        ProjectMode::Follow => "follow",
                        ProjectMode::Pin => "pin",
                    }),
                ),
                ("history_follow_new", boolean(before.history_follow_new, self.history_follow_new)),
                ("usage_large_figures", boolean(before.usage_large_figures, self.usage_large_figures)),
                tool(UsageTool::Codex),
                tool(UsageTool::Claude),
                tool(UsageTool::OpenCode),
                account(UsageAccount::DeepSeek),
                account(UsageAccount::OpenRouter),
                account(UsageAccount::Devin),
                account(UsageAccount::Copilot),
                ("tab_titles", boolean(before.tab_titles, self.tab_titles)),
            ],
            check,
        )
    }

    /// The choice for `key` as a preferences file writes it, or `None` for a key no file holds.
    pub(crate) fn value_of(&self, key: &str) -> Option<&'static str> {
        Some(match key {
            "icons" => self.icons.value(),
            "static_marks" => bool_value(self.static_marks),
            "text_git_marks" => bool_value(self.text_git_marks),
            "follow_activity" => bool_value(self.follow_activity),
            "changes_only" => bool_value(self.changes_only),
            "project_mode" => match self.project_mode {
                ProjectMode::Follow => "follow",
                ProjectMode::Pin => "pin",
            },
            "history_follow_new" => bool_value(self.history_follow_new),
            "usage_large_figures" => bool_value(self.usage_large_figures),
            "tab_titles" => bool_value(self.tab_titles),
            _ => {
                if let Some(tool) = UsageTool::ALL.into_iter().find(|tool| tool.key() == key) {
                    self.usage_tools[tool].value()
                } else {
                    let account = UsageAccount::ALL.into_iter().find(|account| account.key() == key)?;
                    self.usage_accounts[account].value()
                }
            }
        })
    }

    pub fn set_icons(icons: IconMode) -> io::Result<()> {
        let dir = paths::config_dir().ok_or(io::ErrorKind::NotFound)?;
        update(&config_file(&dir, None), &[("icons", Some(icons.value()))])
    }

    /// The keyboard toggle overrides just this setting, leaving inherited choices inherited.
    pub fn set_changes_only(worktree: &Path, value: bool) -> io::Result<()> {
        let dir = paths::config_dir().ok_or(io::ErrorKind::NotFound)?;
        Self::set_changes_only_in(&dir, worktree, value)
    }

    fn set_changes_only_in(dir: &Path, worktree: &Path, value: bool) -> io::Result<()> {
        let _global = lock_directory(dir)?;
        update(&config_file(dir, Some(worktree)), &[("changes_only", Some(bool_value(value)))])
    }
}

fn bool_value(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn update(path: &Path, edits: &[(&str, Option<&str>)]) -> io::Result<()> {
    update_guarded(path, edits, || Ok(()))
}

fn update_guarded(
    path: &Path,
    edits: &[(&str, Option<&str>)],
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let extra = edits.iter().filter_map(|(key, value)| value.map(|value| key.len() + value.len() + 4)).sum::<usize>();
    if extra == 0 {
        return Ok(());
    }
    locked(path, |dir| {
        check()?;
        let old = read(path, PREFERENCES_LIMIT)?.unwrap_or_default();
        replace(dir, path, &edited(&old, edits)?)
    })
}

/// A layer's text with each edited key's lines replaced by one new line.
/// Every other line, comment and unknown key stays as it was.
pub(crate) fn edited(old: &str, edits: &[(&str, Option<&str>)]) -> io::Result<String> {
    // Validate the destination without filling in its missing (inherited) settings.
    Preferences::default().apply(old)?;
    let mut text = String::with_capacity(old.len() + 64);
    for line in old.split_inclusive('\n') {
        let edited = line.split_once('=').is_some_and(|(key, _)| {
            let key = key.trim();
            edits.iter().any(|&(changed, value)| value.is_some() && key == changed)
        });
        if !edited {
            text.push_str(line);
        }
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for (key, value) in edits {
        if let Some(value) = value {
            let _ = writeln!(text, "{key} = {value}");
        }
    }
    if text.len() as u64 > PREFERENCES_LIMIT {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "preferences file would exceed its size limit"));
    }
    Ok(text)
}

/// What a layer's own text says for `key`, if it sets it. The last line wins, as it does when read.
pub(crate) fn layer_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .rev()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| name.trim() == key)
        .map(|(_, value)| value.trim())
}

/// Whether a preferences file could hold `key = value`. An unknown key is never valid here.
pub(crate) fn valid_choice(key: &str, value: &str) -> bool {
    let mut prefs = Preferences::default();
    prefs.value_of(key).is_some() && !value.contains(['\n', '\r']) && prefs.apply(&format!("{key} = {value}")).is_ok()
}

fn remove_layer(path: &Path, check: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let lock = match fs_ops::open_dir(dir) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return check(),
        Err(e) => return Err(e),
    };
    lock.lock()?;
    check()?;
    if writable_mode(path)?.is_none() {
        return Ok(());
    }
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn writable_mode(path: &Path) -> io::Result<Option<u32>> {
    // Renaming or unlinking an existing file only requires a writable directory.
    match fs::symlink_metadata(path) {
        Ok(info) if !info.is_file() => Err(io::ErrorKind::InvalidInput.into()),
        Ok(info) if info.permissions().readonly() => {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is read-only", path.display())))
        }
        #[cfg(unix)]
        Ok(info) => Ok(Some(info.permissions().mode() & 0o7777)),
        #[cfg(windows)]
        Ok(_) => Ok(Some(0)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn config_file(dir: &Path, worktree: Option<&Path>) -> PathBuf {
    match worktree {
        Some(root) => dir.join("worktrees").join(format!("{:016x}.conf", worktree_key(root))),
        None => dir.join("preferences.conf"),
    }
}

/// The file one layer of preferences is kept in: the global defaults, or a worktree's own.
pub(crate) fn layer_file(dir: &Path, worktree: Option<&Path>) -> PathBuf {
    config_file(dir, worktree)
}

/// The files a pane's preferences are read from: the global defaults, then the worktree's own.
pub(crate) fn layers(dir: &Path, worktree: &Path) -> [PathBuf; 2] {
    [config_file(dir, None), config_file(dir, Some(worktree))]
}

/// A stable identifier for the checkout path, not the common git directory.
fn worktree_key(root: &Path) -> u64 {
    root.as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

pub(crate) fn read(path: &Path, limit: u64) -> io::Result<Option<String>> {
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

pub(crate) fn locked<T>(path: &Path, save: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let _lock = lock_directory(dir)?;
    save(dir)
}

fn lock_directory(dir: &Path) -> io::Result<fs::File> {
    fs::create_dir_all(dir)?;
    // Lock the directory because the files themselves are atomically replaced.
    let lock = fs_ops::open_dir(dir)?;
    lock.lock()?;
    Ok(lock)
}

pub(crate) fn replace(dir: &Path, path: &Path, text: &str) -> io::Result<()> {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    #[cfg(unix)]
    let mode = writable_mode(path)?;
    #[cfg(windows)]
    writable_mode(path)?;
    let (temp, mut file) = fs_ops::Temp::create(dir.join(format!(
        ".milo-prefs-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed),
    )))?;
    file.write_all(text.as_bytes())?;
    // Windows keeps no mode bits here: the file lives in the per-user profile directory, whose
    // inherited access list is already private.
    #[cfg(unix)]
    if let Some(mode) = mode {
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    drop(file);
    temp.replace(path)
}

pub struct FoldStore(PathBuf);

impl FoldStore {
    pub fn for_worktree(root: &Path) -> Option<Self> {
        Some(Self::in_state_dir(&paths::state_dir()?, root))
    }

    pub(crate) fn in_state_dir(dir: &Path, root: &Path) -> Self {
        Self(dir.join("worktrees").join(format!("{:016x}.folds", worktree_key(root))))
    }

    pub fn load(&self) -> io::Result<BTreeMap<String, bool>> {
        Ok(read(&self.0, FOLDS_LIMIT)?.map_or_else(BTreeMap::new, |text| decode_folds(&text)))
    }

    /// Merge with choices made by other panes instead of replacing their snapshot.
    pub fn set(&self, path: &str, open: bool) -> io::Result<()> {
        locked(&self.0, |dir| {
            let mut folds = self.load()?;
            match folds.get_mut(path) {
                Some(choice) if *choice == open => return Ok(()),
                Some(choice) => *choice = open,
                None => {
                    folds.insert(path.to_owned(), open);
                }
            }
            replace(dir, &self.0, &encode_folds(&folds)?)
        })
    }
}

// Folder names can contain newlines, tabs or spaces; each byte is written as hex.
fn encode_folds(folds: &BTreeMap<String, bool>) -> io::Result<String> {
    let size = folds.keys().map(|name| 2 * name.len() + 2).sum::<usize>();
    if size as u64 > FOLDS_LIMIT {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "folder choices would exceed their size limit"));
    }
    let mut text = String::with_capacity(size);
    for (name, &open) in folds {
        text.push(if open { '+' } else { '-' });
        for byte in name.bytes() {
            let _ = write!(text, "{byte:02x}");
        }
        text.push('\n');
    }
    Ok(text)
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
    fn usage_tool_choices_save_only_what_changed_and_files_without_them_stay_automatic() {
        let f = Fixture::folder();
        let config = f.root.join("milo");
        let root = f.root.join("checkout");
        let (global_file, local_file) = (config_file(&config, None), config_file(&config, Some(&root)));
        // A file from before the choices existed, with a tool only a later release knows.
        let older = "icons = off\nusage_large_figures = true\nusage_gemini = always\n";
        fs::create_dir_all(&config).unwrap();
        fs::write(&global_file, older).unwrap();
        let (before, source) = Preferences::resolve_in(&config, Some(&root)).unwrap();
        assert_eq!(source, Source::Global);
        assert!(
            before.usage_large_figures && UsageTool::ALL.into_iter().all(|tool| before.usage_tools[tool] == Show::Auto)
        );

        let mut global = before;
        global.usage_tools[UsageTool::OpenCode] = Show::Hide;
        global.save_in(before, &global_file, || Ok(())).unwrap();
        assert_eq!(fs::read_to_string(&global_file).unwrap(), format!("{older}usage_opencode = hide\n"));

        let mut local = global;
        local.usage_tools[UsageTool::Codex] = Show::Always;
        local.usage_tools[UsageTool::OpenCode] = Show::Auto;
        local.save_in(global, &local_file, || Ok(())).unwrap();
        assert_eq!(fs::read_to_string(&local_file).unwrap(), "usage_codex = always\nusage_opencode = auto\n");
        assert_eq!(Preferences::resolve_in(&config, None).unwrap(), (global, Source::Global));
        assert_eq!(Preferences::resolve_in(&config, Some(&root)).unwrap(), (local, Source::Worktree));
        assert_eq!(Preferences::for_pane_in(&config, Some(&root)), (local, None));

        // A choice Milo does not know is an error to correct, not a tool to show or hide by guesswork.
        fs::write(&local_file, "usage_claude = sometimes\n").unwrap();
        let error = Preferences::resolve_in(&config, Some(&root)).unwrap_err();
        assert!(error.to_string().contains("usage_claude"), "{error}");
        assert_eq!(
            Preferences::for_pane_in(&config, Some(&root)),
            (global, Some("milo doctor: worktree prefs ignored"))
        );
    }

    #[test]
    fn accounts_stay_hidden_until_chosen_and_a_choice_is_saved_like_a_tools() {
        let f = Fixture::folder();
        let config = f.root.join("milo");
        let file = config_file(&config, None);
        fs::create_dir_all(&config).unwrap();
        fs::write(&file, "usage_codex = always\n").unwrap();
        let (before, _) = Preferences::resolve_in(&config, None).unwrap();
        assert!(!before.usage_accounts.any());
        assert!(UsageAccount::ALL.into_iter().all(|account| before.usage_accounts[account] == Show::Hide));

        let mut chosen = before;
        chosen.usage_accounts[UsageAccount::DeepSeek] = Show::Auto;
        chosen.save_in(before, &file, || Ok(())).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "usage_codex = always\nusage_deepseek = auto\n");
        let (after, _) = Preferences::resolve_in(&config, None).unwrap();
        assert_eq!(after, chosen);
        assert!(after.usage_accounts.any() && after.usage_accounts[UsageAccount::Devin] == Show::Hide);

        fs::write(&file, "usage_openrouter = yes\n").unwrap();
        assert!(Preferences::resolve_in(&config, None).unwrap_err().to_string().contains("usage_openrouter"));
    }

    #[test]
    fn a_partial_checkout_override_keeps_global_choices() {
        let f = Fixture::folder();
        let config = f.root.join("milo");
        let left = f.root.join("left");
        let right = f.root.join("right");
        let global = Preferences { icons: IconMode::Off, text_git_marks: true, ..Preferences::default() };
        update(&config_file(&config, None), &[("icons", Some("off")), ("text_git_marks", Some("true"))]).unwrap();
        Preferences::set_changes_only_in(&config, &left, true).unwrap();
        assert_eq!(Preferences::resolve_in(&config, Some(&right)).unwrap(), (global, Source::Global));
        assert_eq!(
            Preferences::resolve_in(&config, Some(&left)).unwrap(),
            (Preferences { changes_only: true, ..global }, Source::Worktree),
        );
    }

    #[test]
    fn panes_warn_when_broken_global_preferences_force_defaults() {
        let f = Fixture::folder();
        fs::write(config_file(&f.root, None), "icons = off\nchanges_only = invalid\n").unwrap();
        assert_eq!(
            Preferences::for_pane_in(&f.root, None),
            (Preferences::default(), Some("milo doctor: global prefs ignored")),
        );
    }

    #[test]
    fn panes_keep_the_global_layer_without_partially_applying_a_broken_override() {
        let f = Fixture::folder();
        let config = f.root.join("milo");
        let root = f.root.join("checkout");
        let global_path = config_file(&config, None);
        let file = config_file(&config, Some(&root));
        update(&global_path, &[("icons", Some("off")), ("changes_only", Some("true"))]).unwrap();
        Preferences::set_changes_only_in(&config, &root, false).unwrap();
        let global = Preferences { icons: IconMode::Off, changes_only: true, ..Preferences::default() };
        assert_eq!(
            Preferences::for_pane_in(&config, Some(&root)),
            (Preferences { changes_only: false, ..global }, None),
        );

        let invalid = "changes_only = false\nhistory_follow_new = not-a-bool\n";
        fs::write(&file, invalid).unwrap();
        let (prefs, warning) = Preferences::for_pane_in(&config, Some(&root));
        assert_eq!(prefs, global);
        assert_eq!(warning, Some("milo doctor: worktree prefs ignored"));

        let err = Preferences::resolve_in(&config, Some(&root)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains(file.to_str().unwrap()));
        assert!(err.to_string().contains("history_follow_new"));
        assert!(Preferences::set_changes_only_in(&config, &root, true).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), invalid);
    }

    #[test]
    fn a_save_beyond_the_read_limit_preserves_the_valid_preferences() {
        let f = Fixture::folder();
        let root = f.root.join("checkout");
        let file = config_file(&f.root, Some(&root));
        let line = "icons = off\n";
        let comment = "#".repeat(usize::try_from(PREFERENCES_LIMIT).unwrap() - line.len() - 1);
        let original = format!("{line}{comment}\n");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, &original).unwrap();
        let expected = (Preferences { icons: IconMode::Off, ..Preferences::default() }, Source::Worktree);
        assert_eq!(Preferences::resolve_in(&f.root, Some(&root)).unwrap(), expected);

        let err = Preferences::set_changes_only_in(&f.root, &root, true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read_to_string(&file).unwrap(), original);
        assert_eq!(Preferences::resolve_in(&f.root, Some(&root)).unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_preference_layers_can_be_inherited_but_not_replaced_or_removed() {
        let f = Fixture::folder();
        let root = f.root.join("checkout");
        let [global, local] = layers(&f.root, &root);
        let global_text = "icons = off\n";
        fs::write(&global, global_text).unwrap();
        let snapshot = PreferenceSnapshot::load(&f.root, Some(&root)).unwrap();
        let draft = Preferences { changes_only: true, ..snapshot.prefs };
        fs::set_permissions(&global, fs::Permissions::from_mode(0o444)).unwrap();

        let err = snapshot.save(draft, &f.root, Some(&root), false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&global).unwrap(), global_text);
        assert!(fs::metadata(&global).unwrap().permissions().readonly());
        snapshot.save(draft, &f.root, Some(&root), true).unwrap();

        let snapshot = PreferenceSnapshot::load(&f.root, Some(&root)).unwrap();
        let local_text = "changes_only = true\n";
        assert_eq!(snapshot.prefs, draft);
        fs::set_permissions(&local, fs::Permissions::from_mode(0o444)).unwrap();
        let err = Preferences::set_changes_only_in(&f.root, &root, false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let err = snapshot.inherit(&f.root, &root).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&local).unwrap(), local_text);
        assert!(fs::metadata(&local).unwrap().permissions().readonly());
    }

    #[cfg(unix)]
    #[test]
    fn updating_preferences_preserves_existing_permissions() {
        let f = Fixture::folder();
        let path = config_file(&f.root, None);
        fs::write(&path, "icons = off\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        update(&path, &[("changes_only", Some("true"))]).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
    }

    #[test]
    fn removing_an_override_reports_an_unusable_parent_directory() {
        let f = Fixture::folder();
        let parent = f.root.join("not-a-directory");
        fs::write(&parent, "Keep this").unwrap();
        let path = config_file(&parent, Some(&f.root));

        let error = remove_layer(&path, || Ok(())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
        assert_eq!(fs::read_to_string(parent).unwrap(), "Keep this");
    }

    #[test]
    fn folder_state_merges_choices_and_round_trips_names() {
        let f = Fixture::folder();
        let path = f.root.join("state/folds");
        let a = FoldStore(path.clone());
        let b = FoldStore(path);
        a.set("docs/", false).unwrap();
        b.set("a\n猫/", true).unwrap();
        assert_eq!(a.load().unwrap(), BTreeMap::from([("a\n猫/".into(), true), ("docs/".into(), false)]));
    }

    #[test]
    fn a_full_folder_store_keeps_existing_choices() {
        let f = Fixture::folder();
        let store = FoldStore(f.root.join("folds"));
        let original = format!("+{}2f\n", "61".repeat(usize::try_from(FOLDS_LIMIT / 2).unwrap() - 2));
        fs::write(&store.0, &original).unwrap();

        let err = store.set("fresh/", false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read_to_string(&store.0).unwrap(), original);
        assert_eq!(store.load().unwrap().len(), 1);
    }
}
