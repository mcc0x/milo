//! Plans edits to Rex's init file and checks layouts before any settings change.

use crate::sys::AsRawFd;
use std::borrow::Cow;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::files::{Change, Image, staged};
use super::install::{RunRex, Store, read, settings_path};
use super::layout::{
    Layout, ListenerEnv, listener_lua, listener_state_matches, lua_path, owns_actions, owns_listener, rex_lua,
    update_actions,
};
const CREATED: &[u8] = b"-- Managed by milo setup. Milo created this file.";
pub(super) const MANAGED: &[u8] = b"-- Managed by milo setup.";
pub(super) const LAYOUT_DISABLED: &str = "layout-disabled";
pub(super) const LISTENER_STATE_MISMATCH: &str =
    "Milo's Rex listener was installed with a different XDG_STATE_HOME. Rerun with the original XDG_STATE_HOME.";
pub(super) const LOADER_CONFLICT: &str =
    "Milo's lines in Rex settings were moved, copied or changed. Your file was left alone.";

impl Store {
    pub(super) fn rex_file(&self) -> PathBuf {
        self.config.join("rex.lua")
    }

    pub fn listener_file(&self) -> PathBuf {
        self.config.join("listener.lua")
    }

    /// An unconfigured screen starts with a suggestion; a previously disabled one starts off.
    pub fn layout(&self) -> Result<(Layout, bool), &'static str> {
        check_config_dir(&self.config)?;
        let active = self.has_layout()?;
        let pending = self.rex_reload_pending();
        let disabled = self.state.join(LAYOUT_DISABLED).is_file();
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
        let file = read(&settings_path(&self.init)?)?;
        let bytes = file.as_ref().map_or(&[][..], |file| file.bytes.as_slice());
        Ok(checked_loader(bytes, &loader(&self.rex_file()))?.1.is_some())
    }

    pub(super) fn listener_env(&self) -> ListenerEnv<'_> {
        ListenerEnv {
            config_home: self.config.parent().unwrap_or(Path::new(".")),
            state_home: self.state.parent().unwrap_or(Path::new(".")),
        }
    }

    pub(super) fn listener_state_matches(&self) -> Result<bool, &'static str> {
        check_config_dir(&self.config)?;
        let actions = read(&self.rex_file())?;
        Ok(actions
            .as_ref()
            .and_then(|file| std::str::from_utf8(&file.bytes).ok())
            .is_none_or(|text| listener_state_matches(text, self.listener_env().state_home)))
    }

    /// Check generated files before the shared installer touches user settings.
    pub(super) fn plan_rex(
        &self,
        layout: &Layout,
        changes: &mut Vec<Change>,
        run: &RunRex<'_>,
    ) -> Result<bool, &'static str> {
        check_config_dir(&self.config)?;
        let enabled = layout.enabled();
        let rex_path = self.rex_file();
        let init = plan_init(settings_path(&self.init)?, &loader(&rex_path), enabled)?;
        let note = self.state.join(LAYOUT_DISABLED);
        let disabled = read(&note)?;
        if disabled.as_ref().is_some_and(|file| !file.bytes.is_empty()) {
            return Err("Milo's saved layout note was changed. It was left alone.");
        }
        // Record removal first so an interrupted removal does not look like first-time setup.
        changes.extend(Change::new(note, disabled, (!enabled).then(Vec::new)));
        let first_rex_change = changes.len();
        if enabled {
            self.plan_actions(layout, &rex_path, changes, run)?;
            self.plan_listener(&rex_path, changes, run)?;
        }
        changes.extend(init);
        if !enabled {
            self.remove_shipped(&rex_path, changes)?;
        }
        Ok(changes.len() != first_rex_change)
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
        let env = self.listener_env();
        let after = if let Some(file) = &before {
            let text = std::str::from_utf8(&file.bytes).ok();
            text.and_then(|text| update_actions(text, layout, &executable, env))
                .ok_or("Could not update milo's Rex file (rex.lua). Your file was left alone.")?
        } else {
            Some(rex_lua(layout, &executable, env))
        };
        if let Some(lua) = after {
            changes.extend(Change::in_private_dir(path.into(), before, Some(self.checked(lua, run)?)));
        }
        Ok(())
    }

    fn plan_listener(&self, rex_path: &Path, changes: &mut Vec<Change>, run: &RunRex<'_>) -> Result<(), &'static str> {
        let path = self.listener_file();
        let before = read(&path)?;
        let lua = listener_lua(rex_path);
        if let Some(file) = &before {
            if file.bytes == lua.as_bytes() {
                return Ok(());
            }
            if !text_is(file, |text| owns_listener(text, rex_path)) {
                return Err("Could not update milo's listener file (listener.lua). Your file was left alone.");
            }
        }
        changes.extend(Change::in_private_dir(path, before, Some(self.checked(lua, run)?)));
        Ok(())
    }

    fn remove_shipped(&self, rex_path: &Path, changes: &mut Vec<Change>) -> Result<(), &'static str> {
        let actions = read(rex_path)?.filter(|file| text_is(file, owns_actions));
        if actions.is_some() {
            changes.extend(Change::in_private_dir(rex_path.into(), actions, None));
        }
        let path = self.listener_file();
        let listener = read(&path)?.filter(|file| text_is(file, |text| owns_listener(text, rex_path)));
        if listener.is_some() {
            changes.extend(Change::in_private_dir(path, listener, None));
        }
        Ok(())
    }

    /// Rex checks a temporary copy so rejecting it leaves the existing layout untouched.
    fn checked(&self, lua: String, run: &RunRex<'_>) -> Result<Vec<u8>, &'static str> {
        let stage = staged(lua.as_bytes()).map_err(|_| "Could not prepare the Rex files.")?;
        // Let the runner attach the held, unlinked descriptor without resolving the scratch filename.
        let fd = format!("/dev/fd/{}", stage.as_raw_fd());
        if !run(&[OsStr::new("config"), OsStr::new("check"), OsStr::new(&fd)]) {
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

type LoaderBlock = (std::ops::Range<usize>, bool);

pub(super) fn checked_loader(bytes: &[u8], load: &str) -> Result<(usize, Option<LoaderBlock>), &'static str> {
    let start = init_start(bytes);
    let (comment, next) = line(bytes, start);
    // Only the first two lines after a possible shebang belong to setup.
    let marker = matches!(comment, MANAGED | CREATED);
    let block = if marker {
        let (call, end) = line(bytes, next);
        (call == load.as_bytes()).then_some((start..end, comment == CREATED))
    } else {
        None
    };
    let other = &bytes[block.as_ref().map_or(start, |(range, _)| range.end)..];
    // An exact loader elsewhere is ambiguous: it may be active, commented out, or quoted.
    // Do not add a second copy or remove the files it may still load.
    if (block.is_none() && marker)
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
        return Err(LOADER_CONFLICT);
    }
    Ok((start, block))
}

fn plan_init(path: Cow<'_, Path>, load: &str, enable: bool) -> Result<Option<Change>, &'static str> {
    let original = read(&path)?;
    let bytes = original.as_ref().map_or(&[][..], |file| file.bytes.as_slice());
    let (start, block) = checked_loader(bytes, load)?;
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
mod tests {
    use super::super::files;
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    #[test]
    fn only_the_new_milo_config_directory_is_private() {
        let fixture = crate::fixture::Fixture::folder();
        let shared = fixture.root.join("shared");
        let reference = fixture.root.join("reference");
        fs::create_dir(&reference).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let store = Store {
            init: shared.join("rex/init.lua"),
            config: shared.join("milo"),
            state: fixture.root.join("state"),
            claude: fixture.root.join("settings.json"),
            claude_state: None,
        };
        let mut changes = Vec::new();
        store.plan_rex(&Layout::default(), &mut changes, &|_| true).unwrap();
        files::install(&changes, || true).unwrap();
        assert_eq!(mode(&store.config) & 0o077, 0);
        assert_eq!(mode(&shared), mode(&reference));
        assert_eq!(mode(&shared.join("rex")), mode(&reference));
    }

    #[test]
    fn installing_into_existing_milo_config_preserves_the_directory_mode() {
        let fixture = crate::fixture::Fixture::folder();
        let store = Store {
            init: fixture.root.join("init.lua"),
            config: fixture.root.join("milo"),
            state: fixture.root.join("state"),
            claude: fixture.root.join("settings.json"),
            claude_state: None,
        };
        fs::create_dir(&store.config).unwrap();
        fs::set_permissions(&store.config, fs::Permissions::from_mode(0o750)).unwrap();

        let mut changes = Vec::new();
        store.plan_rex(&Layout::default(), &mut changes, &|_| true).unwrap();
        files::install(&changes, || true).unwrap();
        assert_eq!(fs::metadata(&store.config).unwrap().permissions().mode() & 0o777, 0o750);
    }

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
