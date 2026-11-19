//! Plans edits to Rex's init file and checks layouts before any settings change.

use std::borrow::Cow;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::files::{self, Change, Image, staged};
use super::install::{RunRex, Store, read, settings_path};
use super::layout::{Layout, listener_lua, lua_path, owns_actions, owns_listener, rex_lua, update_actions};
const CREATED: &[u8] = b"-- Managed by milo setup. Milo created this file.";
pub(super) const MANAGED: &[u8] = b"-- Managed by milo setup.";
pub(super) const OFFERED: &str = "setup-offered";

pub(super) struct RexPlan {
    pub reload: bool,
    pub listener_changed: bool,
}

impl Store {
    pub(super) fn rex_file(&self) -> PathBuf {
        self.config.join("rex.lua")
    }

    pub fn listener_file(&self) -> PathBuf {
        self.config.join("listener.lua")
    }

    /// An installed layout or the first offer's marker suppresses future offers.
    pub fn offer_once(&self) -> bool {
        if self.state.join(OFFERED).exists() {
            return false;
        }
        if self.has_layout() != Ok(false) {
            return false;
        }
        fs::create_dir_all(&self.state).is_ok()
            && crate::fs_ops::open_dir(&self.state).is_ok()
            && OpenOptions::new().write(true).create_new(true).mode(0o600).open(self.state.join(OFFERED)).is_ok()
    }

    /// An unconfigured screen starts with a suggestion; a previously disabled one starts off.
    pub fn layout(&self) -> Result<(Layout, bool), &'static str> {
        check_config_dir(&self.config)?;
        let active = self.has_layout()?;
        let pending = self.rex_reload_pending();
        let disabled = self.state.join("layout-disabled").is_file();
        if (active || pending) && !(pending && disabled) {
            let saved = read(&self.rex_file())?
                .as_ref()
                .and_then(|file| std::str::from_utf8(&file.bytes).ok().and_then(Layout::parse));
            if let Some(saved) = saved {
                return Ok((saved, active));
            }
            if active {
                return Err("Could not read the saved Rex layout.");
            }
        }
        Ok((Layout { panes: [!disabled; 3], ..Layout::default() }, active))
    }

    pub(super) fn has_layout(&self) -> Result<bool, &'static str> {
        let path = settings_path(&self.init)?;
        read_init_prefix(&path, &loader(&self.rex_file()))
            .map_err(|_| "Could not read Rex settings. No files were changed.")
    }

    /// Check generated files before the shared installer touches user settings.
    pub(super) fn plan_rex(
        &self,
        layout: &Layout,
        changes: &mut Vec<Change>,
        run: &RunRex<'_>,
    ) -> Result<RexPlan, &'static str> {
        check_config_dir(&self.config)?;
        let enabled = layout.enabled();
        let rex_path = self.rex_file();
        let init = plan_init(settings_path(&self.init)?, &loader(&rex_path), enabled)?;
        let note = self.state.join("layout-disabled");
        let disabled = read(&note)?;
        if disabled.as_ref().is_some_and(|file| !file.bytes.is_empty()) {
            return Err("Milo's saved layout note was changed. It was left alone.");
        }
        // Record removal first so an interrupted removal does not look like first-time setup.
        changes.extend(Change::new(note, disabled, (!enabled).then(Vec::new)));
        let first_rex_change = changes.len();
        if enabled {
            self.plan_actions(layout, &rex_path, changes, run)?;
        }
        let listener_changed = layout.new_tabs && self.plan_listener(&rex_path, changes, run)?;
        changes.extend(init);
        if !enabled {
            self.remove_shipped(&rex_path, changes)?;
        }
        Ok(RexPlan { reload: changes.len() != first_rex_change, listener_changed })
    }

    fn plan_actions(
        &self,
        layout: &Layout,
        path: &Path,
        changes: &mut Vec<Change>,
        run: &RunRex<'_>,
    ) -> Result<(), &'static str> {
        let before = read(path)?;
        let executable = executable()?;
        let after = if let Some(file) = &before {
            let text = std::str::from_utf8(&file.bytes).ok();
            text.and_then(|text| update_actions(text, layout, &executable))
                .ok_or("Could not update milo's Rex file (rex.lua). Your file was left alone.")?
        } else {
            Some(rex_lua(layout, &executable))
        };
        if let Some(lua) = after {
            changes.extend(Change::new(path.into(), before, Some(self.checked(lua, run)?)));
        }
        Ok(())
    }

    fn plan_listener(
        &self,
        rex_path: &Path,
        changes: &mut Vec<Change>,
        run: &RunRex<'_>,
    ) -> Result<bool, &'static str> {
        let path = self.listener_file();
        let before = read(&path)?;
        let lua = listener_lua(rex_path);
        if let Some(file) = &before {
            if file.bytes == lua.as_bytes() {
                return Ok(false);
            }
            if !text_is(file, |text| owns_listener(text, rex_path)) {
                return Err("Could not update milo's listener file (listener.lua). Your file was left alone.");
            }
        }
        changes.extend(Change::new(path, before, Some(self.checked(lua, run)?)));
        Ok(true)
    }

    fn remove_shipped(&self, rex_path: &Path, changes: &mut Vec<Change>) -> Result<(), &'static str> {
        let actions = read(rex_path)?.filter(|file| text_is(file, owns_actions));
        if actions.is_some() {
            changes.extend(Change::new(rex_path.into(), actions, None));
        }
        let path = self.listener_file();
        let listener = read(&path)?.filter(|file| text_is(file, |text| owns_listener(text, rex_path)));
        if listener.is_some() {
            changes.extend(Change::new(path, listener, None));
        }
        Ok(())
    }

    /// Rex checks a temporary copy so rejecting it leaves the existing layout untouched.
    fn checked(&self, lua: String, run: &RunRex<'_>) -> Result<Vec<u8>, &'static str> {
        let stage = staged(&self.config, lua.as_bytes()).map_err(|_| "Could not prepare the Rex files.")?;
        if !run(&[OsStr::new("config"), OsStr::new("check"), stage.path().as_os_str()]) {
            return Err("Rex could not check the layout. No files were changed.");
        }
        Ok(lua.into_bytes())
    }
}

pub(super) fn check_config_dir(path: &Path) -> Result<(), &'static str> {
    match fs::symlink_metadata(path) {
        Ok(info) if info.is_dir() => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        _ => Err("Milo's settings folder must be a regular folder. It was left alone."),
    }
}

fn text_is(file: &Image, test: impl FnOnce(&str) -> bool) -> bool {
    std::str::from_utf8(&file.bytes).is_ok_and(test)
}

/// A long first line need not be copied into memory just to check for our loader.
fn read_init_prefix(path: &Path, load: &str) -> io::Result<bool> {
    let Some((file, _)) = files::open(path)? else { return Ok(false) };
    let mut file = BufReader::new(file);
    if without_bom(file.fill_buf()?).starts_with(b"#!") {
        file.skip_until(b'\n')?;
    }
    let limit = 3 + CREATED.len() + load.len() + 4;
    let mut prefix = Vec::with_capacity(limit + 1);
    file.take((limit + 1) as u64).read_to_end(&mut prefix)?;
    Ok(managed(&prefix, load).is_some())
}

/// Keep Homebrew's version-independent launcher when it points to this exact binary.
pub(super) fn executable() -> Result<PathBuf, &'static str> {
    let path = env::current_exe().and_then(fs::canonicalize).map_err(|_| "Could not find the full path to milo.")?;
    Ok(brew_launcher(&path).unwrap_or(path))
}

fn brew_launcher(path: &Path) -> Option<PathBuf> {
    let bin = path.parent()?;
    let formula = bin.parent()?.parent()?;
    let cellar = formula.parent()?;
    if path.file_name()? != "milo"
        || bin.file_name()? != "bin"
        || formula.file_name()? != "milo"
        || cellar.file_name()? != "Cellar"
    {
        return None;
    }
    let stable = cellar.parent()?.join("opt/milo/bin/milo");
    (fs::canonicalize(&stable).ok()? == path).then_some(stable)
}

/// The managed block must be found in LF or CRLF files without changing other bytes.
fn line(bytes: &[u8], start: usize) -> (&[u8], usize) {
    let remaining = &bytes[start..];
    remaining.iter().position(|&b| b == b'\n').map_or((remaining, bytes.len()), |end| {
        let text = &remaining[..end];
        (text.strip_suffix(b"\r").unwrap_or(text), start + end + 1)
    })
}

fn without_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
}

fn init_start(bytes: &[u8]) -> usize {
    let body = without_bom(bytes);
    let start = bytes.len() - body.len();
    if body.starts_with(b"#!") { line(bytes, start).1 } else { start }
}

pub(super) fn loader(path: &Path) -> String {
    format!("pcall(dofile, {})", lua_path(path))
}

/// Only the first lines can belong to setup; a quoted example farther down cannot.
pub(super) fn managed(bytes: &[u8], load: &str) -> Option<(std::ops::Range<usize>, bool)> {
    let start = init_start(bytes);
    let (comment, next) = line(bytes, start);
    let (call, end) = line(bytes, next);
    (matches!(comment, MANAGED | CREATED) && call == load.as_bytes()).then_some((start..end, comment == CREATED))
}

fn plan_init(path: Cow<'_, Path>, load: &str, enable: bool) -> Result<Option<Change>, &'static str> {
    let original = read(&path)?;
    let bytes = original.as_ref().map_or(&[][..], |file| file.bytes.as_slice());
    let start = init_start(bytes);
    let block = managed(bytes, load);
    let other = &bytes[block.as_ref().map_or(start, |(range, _)| range.end)..];
    // An exact loader elsewhere is ambiguous: it may be active, commented out, or quoted.
    // Do not add a second copy or remove the files it may still load.
    if (block.is_none() && matches!(line(bytes, start).0, MANAGED | CREATED))
        || other.split(|&byte| byte == b'\n').any(|line| {
            let mut line = line.trim_ascii();
            while let Some(rest) = line.strip_prefix(b"--") {
                line = rest.trim_ascii_start();
            }
            line.strip_prefix(load.as_bytes()).is_some_and(|rest| {
                let rest = rest.trim_ascii_start();
                rest.is_empty() || rest.starts_with(b"--")
            })
        })
    {
        return Err("Milo's lines in Rex settings were moved, copied or changed. Your file was left alone.");
    }
    let after = match (block, enable) {
        (Some((range, created)), false) => {
            let delete = created && matches!(path, Cow::Borrowed(_)) && range == (0..bytes.len());
            (!delete).then(|| [&bytes[..range.start], &bytes[range.end..]].concat())
        }
        (None, true) => Some(insert_loader(bytes, start, load, original.is_none())?),
        _ => return Ok(None),
    };
    Ok(Change::new(path.into_owned(), original, after))
}

fn insert_loader(bytes: &[u8], start: usize, load: &str, created: bool) -> Result<Vec<u8>, &'static str> {
    if start == bytes.len() && without_bom(bytes).starts_with(b"#!") && !bytes.ends_with(b"\n") {
        return Err("Rex's settings file has an unfinished first line.");
    }
    let newline: &[u8] = match bytes.iter().position(|&b| b == b'\n') {
        Some(end) if bytes[..end].ends_with(b"\r") => b"\r\n",
        _ => b"\n",
    };
    let comment = if created { CREATED } else { MANAGED };
    Ok([&bytes[..start], comment, newline, load.as_bytes(), newline, &bytes[start..]].concat())
}

#[cfg(test)]
mod launcher_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn only_use_the_stable_brew_link_for_the_running_installation() {
        let scratch = crate::fixture::Fixture::folder();
        let prefix = scratch.root.canonicalize().unwrap();
        let cellar = prefix.join("Cellar/milo");
        let old = cellar.join("1/bin/milo");
        let current = cellar.join("2/bin/milo");
        for path in [&old, &current] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, []).unwrap();
        }
        fs::create_dir(prefix.join("opt")).unwrap();
        symlink("../Cellar/milo/2", prefix.join("opt/milo")).unwrap();
        assert_eq!(brew_launcher(&current), Some(prefix.join("opt/milo/bin/milo")));
        assert_eq!(brew_launcher(&old), None);
    }
}
