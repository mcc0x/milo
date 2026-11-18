//! Plans edits to Rex's init file and checks layouts before any settings change.

use std::borrow::Cow;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::files::{Change, staged};
use super::install::{RunRex, Store, read, settings_path};
use super::layout::{GENERATED, Layout, layout_matches, listener_lua, lua_path, rex_lua};
use crate::rex;
const CREATED: &[u8] = b"-- Managed by milo setup. Milo created this file.";
pub(super) const MANAGED: &[u8] = b"-- Managed by milo setup.";
pub(super) const OFFERED: &str = "setup-offered";

impl Store {
    pub(super) fn rex_file(&self) -> PathBuf {
        self.config.join("rex.lua")
    }

    pub fn listener_file(&self) -> PathBuf {
        self.config.join("listener.lua")
    }

    /// An installed layout or the first offer's marker suppresses future offers.
    pub fn offer_once(&self) -> bool {
        let Ok(init) = settings_path(&self.init).and_then(|path| read(&path)) else { return false };
        if init.as_ref().is_some_and(|file| managed(&file.bytes).is_some()) {
            return false;
        }
        fs::create_dir_all(&self.state).is_ok()
            && OpenOptions::new().write(true).create_new(true).mode(0o600).open(self.state.join(OFFERED)).is_ok()
    }

    /// An unconfigured screen starts with a suggestion; a previously disabled one starts off.
    pub fn layout(&self) -> Result<(Layout, bool), &'static str> {
        let init = read(&settings_path(&self.init)?)?;
        if init.as_ref().is_none_or(|file| managed(&file.bytes).is_none()) {
            let panes = if self.state.join("layout-disabled").is_file() { [false; 3] } else { [true; 3] };
            return Ok((Layout { panes, ..Layout::default() }, false));
        }
        read(&self.rex_file())?
            .as_ref()
            .and_then(|file| std::str::from_utf8(&file.bytes).ok().and_then(Layout::parse))
            .map(|layout| (layout, true))
            .ok_or("Could not read the installed Rex layout.")
    }

    /// Check generated files before the shared installer touches user settings.
    pub(super) fn plan_rex(
        &self,
        layout: &Layout,
        changes: &mut Vec<Change>,
        run: &RunRex,
    ) -> Result<bool, &'static str> {
        let init_path = settings_path(&self.init)?;
        let init = read(&init_path)?;
        let rex_path = self.rex_file();
        let listener_path = self.listener_file();
        let rex_before = read(&rex_path)?;
        let listener_before = read(&listener_path)?;
        let already_installed = init.as_ref().is_some_and(|file| managed(&file.bytes).is_some());
        let init_after = edit_init(init.as_ref().map(|file| file.bytes.as_slice()), &rex_path, layout.enabled())?
            .or_else(|| matches!(init_path, Cow::Owned(_)).then(Vec::new));
        if layout.enabled() {
            let executable = executable()?;
            let installed = rex_before.as_ref().and_then(|file| std::str::from_utf8(&file.bytes).ok());
            if !already_installed || installed.is_none_or(|text| !layout_matches(text, layout, &executable)) {
                changes.extend(Change::new(
                    rex_path,
                    rex_before,
                    Some(self.checked(rex_lua(layout, &executable), run)?),
                ));
            }
            let listener = listener_lua(&self.rex_file());
            if !already_installed || listener_before.as_ref().is_none_or(|file| file.bytes != listener.as_bytes()) {
                changes.extend(Change::new(listener_path, listener_before, Some(self.checked(listener, run)?)));
            }
            changes.extend(Change::new(init_path.into_owned(), init, init_after));
        } else {
            changes.extend(Change::new(init_path.into_owned(), init, init_after));
            for (path, file) in [(rex_path, rex_before), (listener_path, listener_before)] {
                if file.as_ref().is_some_and(|file| line(&file.bytes, 0).0 == GENERATED.trim_end().as_bytes()) {
                    changes.extend(Change::new(path, file, None));
                }
            }
        }
        let reload = !changes.is_empty();
        let note = self.state.join("layout-disabled");
        changes.extend(Change::new(note.clone(), read(&note)?, (!layout.enabled()).then(Vec::new)));
        Ok(reload)
    }

    /// Rex checks a temporary copy so rejecting it leaves the existing layout untouched.
    fn checked(&self, lua: String, run: &RunRex) -> Result<Vec<u8>, &'static str> {
        let stage = staged(&self.config, lua.as_bytes()).map_err(|_| "Could not prepare the Rex files.")?;
        if !run(&[OsStr::new("config"), OsStr::new("check"), stage.path().as_os_str()]) {
            return Err("Rex could not check the layout. No files were changed.");
        }
        Ok(lua.into_bytes())
    }

    /// Try these exact choices in the current tab without installing or reloading.
    pub fn arrange(&self, layout: &Layout, target: &rex::Target, run: &RunRex) -> Result<(), &'static str> {
        if !layout.panes.contains(&true) {
            return Err("Choose at least one pane first.");
        }
        let scope = [OsStr::new("-s"), target.session.as_os_str(), OsStr::new("-b"), target.block.as_os_str()];
        let action = OsStr::new("milo_layout");
        // Try these exact choices without installing or reloading anything.
        let lua = rex_lua(layout, &executable()?);
        let temp = staged(&self.config, lua.as_bytes()).map_err(|_| "Could not prepare the Rex layout.")?;
        let done = run(&[
            OsStr::new("do"),
            temp.path().as_os_str(),
            OsStr::new("--action"),
            action,
            scope[0],
            scope[1],
            scope[2],
            scope[3],
        ]);
        if done { Ok(()) } else { Err("Rex could not arrange this tab.") }
    }
}

/// Rex uses the same binary as setup, even if milo was invoked via a link.
pub(super) fn executable() -> Result<PathBuf, &'static str> {
    env::current_exe().and_then(fs::canonicalize).map_err(|_| "Could not find the full path to milo.")
}

/// The managed block must be found in LF or CRLF files without changing other bytes.
fn line(bytes: &[u8], start: usize) -> (&[u8], usize) {
    let remaining = &bytes[start..];
    remaining.iter().position(|&b| b == b'\n').map_or((remaining, bytes.len()), |end| {
        let text = &remaining[..end];
        (text.strip_suffix(b"\r").unwrap_or(text), start + end + 1)
    })
}

/// Returns the exact two lines setup owns, including their line endings.
pub(super) fn managed(bytes: &[u8]) -> Option<(std::ops::Range<usize>, bool)> {
    let mut start = if bytes.starts_with(b"\xef\xbb\xbf") { 3 } else { 0 };
    while start < bytes.len() {
        let (comment, next) = line(bytes, start);
        if matches!(comment, MANAGED | CREATED) {
            let (load, end) = line(bytes, next);
            if load.starts_with(b"pcall(dofile, \"") && load.ends_with(b"\")") {
                return Some((start..end, comment == CREATED));
            }
        }
        start = next;
    }
    None
}

/// Keep the original line endings and the file's opening BOM or shebang.
fn edit_init(original: Option<&[u8]>, rex_file: &Path, enable: bool) -> Result<Option<Vec<u8>>, &'static str> {
    let bytes = original.unwrap_or_default();
    let block = managed(bytes);
    if !enable {
        let Some((range, created)) = block else { return Ok(original.map(<[u8]>::to_vec)) };
        let restored = [&bytes[..range.start], &bytes[range.end..]].concat();
        return Ok((!created || !restored.is_empty()).then_some(restored));
    }
    let (range, created) = if let Some(block) = block {
        block
    } else {
        let mut start = if bytes.starts_with(b"\xef\xbb\xbf") { 3 } else { 0 };
        if bytes[start..].starts_with(b"#!") {
            start = line(bytes, start).1;
            if !bytes[..start].ends_with(b"\n") {
                return Err("Rex's init.lua has an unfinished first line.");
            }
        }
        (start..start, original.is_none())
    };
    let newline: &[u8] = match bytes.iter().position(|&b| b == b'\n') {
        Some(end) if bytes[..end].ends_with(b"\r") => b"\r\n",
        _ => b"\n",
    };
    let load = format!("pcall(dofile, {})", lua_path(rex_file));
    let comment = if created { CREATED } else { MANAGED };
    Ok(Some([&bytes[..range.start], comment, newline, load.as_bytes(), newline, &bytes[range.end..]].concat()))
}
