//! Coordinates settings changes and rolls back installs that Rex cannot reload.

use std::borrow::Cow;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use super::files::{self, Image};
use super::layout::Layout;
use super::lock;
use super::rex_files::LISTENER_STATE_MISMATCH;
use crate::paths;

const RELOAD: [&str; 2] = ["config", "reload"];
pub(super) const RELOAD_PENDING: &str = "rex-reload-pending";

/// Lets setup use the real Rex command while tests use an isolated stand-in.
pub(super) type RunRex<'a> = dyn Fn(&[&OsStr]) -> bool + 'a;

/// The files may be saved even if the separate listener could not start or stop.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Saved {
    Ready,
    ListenerDidNotStart,
    ListenerDidNotStop,
}

#[derive(Debug)]
pub(super) struct SaveOutcome {
    pub listener: Saved,
    pub reload_failed: bool,
}

/// Keeps the user settings and milo's files together for one setup transaction.
#[derive(Clone)]
pub(super) struct Store {
    pub init: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub claude: PathBuf,
}

impl Store {
    /// Honour Rex's settings override without redirecting milo's own files there.
    pub fn real() -> Option<Self> {
        let home = paths::home()?;
        let config = paths::config_dir()?;
        let init = env::var_os("REX_CONFIG")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| config.parent().unwrap_or(Path::new(".")).join("rex/init.lua"));
        Some(Self { init, config, state: paths::state_dir()?, claude: home.join(".claude/settings.json") })
    }

    pub fn rex_reload_pending(&self) -> bool {
        fs::symlink_metadata(self.state.join(RELOAD_PENDING)).is_ok_and(|info| info.is_file() && info.len() == 0)
    }

    /// Plan both sets of changes before writing so one failed plan cannot leave a partial save.
    pub fn save(
        &self,
        layout: Option<&Layout>,
        claude: Option<bool>,
        run: &RunRex<'_>,
    ) -> Result<SaveOutcome, &'static str> {
        if layout.is_none() && claude.is_none() {
            return Ok(SaveOutcome { listener: Saved::Ready, reload_failed: false });
        }
        // Removing the loader is safe even when Rex is missing or its config is broken.
        let reload_optional = layout.is_some_and(|layout| !layout.enabled());
        self.save_transaction(layout, claude, run, reload_optional, |_| Ok(()))
    }

    /// Plan Milo-only preferences under the same lock and roll them back with Rex settings.
    pub(super) fn save_with(
        &self,
        layout: Option<&Layout>,
        claude: Option<bool>,
        run: &RunRex<'_>,
        extra: impl FnOnce(&mut Vec<files::Change>) -> Result<(), &'static str>,
    ) -> Result<Saved, &'static str> {
        self.save_transaction(layout, claude, run, false, extra).map(|saved| saved.listener)
    }

    fn save_transaction(
        &self,
        layout: Option<&Layout>,
        claude: Option<bool>,
        run: &RunRex<'_>,
        reload_optional: bool,
        extra: impl FnOnce(&mut Vec<files::Change>) -> Result<(), &'static str>,
    ) -> Result<SaveOutcome, &'static str> {
        if layout.is_some() && !self.listener_state_matches()? {
            return Err(LISTENER_STATE_MISMATCH);
        }
        let _lock = lock::setup(&self.state)?;
        let mut changes = Vec::new();
        extra(&mut changes)?;
        let watch_layout = layout.is_some_and(Layout::enabled);
        let rex_changed = layout.map(|layout| self.plan_rex(layout, &mut changes, run)).transpose()?.unwrap_or(false);
        let pending_path = self.state.join(RELOAD_PENDING);
        let pending = if layout.is_some() { read(&pending_path)? } else { None };
        if pending.as_ref().is_some_and(|file| !file.bytes.is_empty()) {
            return Err("Milo's Rex reload note was changed. It was left alone.");
        }
        if rex_changed && pending.is_none() {
            // The file changes are atomic one at a time; Rex must reload even if
            // the previous save ended after publishing every wanted file.
            changes.splice(..0, files::Change::new(pending_path.clone(), None, Some(Vec::new())));
        }
        let listener_running = watch_layout && lock::running(&self.state).unwrap_or(false);
        let reload = rex_changed || pending.is_some() || (watch_layout && !listener_running);
        let restart = listener_running && (rex_changed || pending.is_some());
        if let Some(enabled) = claude {
            self.claude_files()?.plan(enabled, &mut changes)?;
        }
        let mut stopped = false;
        let mut reload_failed = false;
        let installed = files::install(&changes, || {
            // A running listener has already loaded its script; a reload alone won't update it.
            stopped = restart && lock::stop(&self.state).is_ok();
            reload_failed = reload && !call(run, RELOAD);
            !reload_failed || reload_optional
        });
        if installed.is_err() && stopped {
            // The previous files are back, so give Rex a chance to start the old listener again.
            call(run, RELOAD);
        }
        installed?;
        if (rex_changed || pending.is_some()) && !reload_failed && fs::remove_file(pending_path).is_err() {
            return Err("Saved, but could not clear the Rex reload reminder.");
        }
        let stop_failed =
            (restart && !stopped) || (layout.is_some() && !watch_layout && lock::stop(&self.state).is_err());
        let listener = if stop_failed {
            Saved::ListenerDidNotStop
        } else if watch_layout && !lock::wait_running(&self.state).unwrap_or(false) {
            Saved::ListenerDidNotStart
        } else {
            Saved::Ready
        };
        Ok(SaveOutcome { listener, reload_failed })
    }
}

/// Give both settings plans the same error when the previous bytes cannot be read.
pub(super) fn read(path: &Path) -> Result<Option<Image>, &'static str> {
    Image::read(path).map_err(|_| "Could not read a settings file. No files were changed.")
}

/// The ownership checks and writes must operate on the same existing settings target.
pub(super) fn settings_path(path: &Path) -> Result<Cow<'_, Path>, &'static str> {
    files::resolve(path).map_err(|_| "Could not read a settings file. No files were changed.")
}

fn call<const N: usize>(run: &RunRex<'_>, args: [&str; N]) -> bool {
    run(&args.map(OsStr::new))
}

#[cfg(test)]
mod tests {
    use super::super::claude::{Claude, OWN_CLAUDE};
    use super::super::files::Change;
    use super::super::layout::{GENERATED, TabPane, WIDTHS, rex_lua};
    use super::super::rex_files::{LAYOUT_DISABLED, MANAGED, executable, loader, managed};
    use super::*;
    use crate::fixture::Fixture;
    use std::cell::RefCell;
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::Command;

    fn store(fixture: &Fixture) -> Store {
        store_at(&fixture.root)
    }

    fn store_at(root: &Path) -> Store {
        Store {
            init: root.join("init.lua"),
            config: root.join("config"),
            state: root.join("state"),
            claude: root.join("settings.json"),
        }
    }

    fn off() -> Layout {
        Layout { panes: [false; 3], ..Layout::default() }
    }

    #[test]
    fn generated_actions_use_only_the_chosen_panes_for_the_layout() {
        let executable = executable().unwrap();
        let fixture = Fixture::folder();
        let store = store(&fixture);
        for width in WIDTHS.map(|width| width.proportion) {
            for mask in 0..8 {
                for new_tabs in [false, true] {
                    let panes = std::array::from_fn(|i| mask & (1 << i) != 0);
                    let layout = Layout { width: width.into(), panes, new_tabs, ..Layout::default() };
                    let lua = rex_lua(&layout, &executable, store.listener_env());
                    let (_, choices) = lua.split_once("local chosen = {\n").unwrap();
                    let (choices, _) = choices.split_once("}\n").unwrap();
                    for (name, chosen) in TabPane::ALL.map(TabPane::name).into_iter().zip(panes) {
                        assert_eq!(choices.contains(&format!("{name} = true,")), chosen);
                    }
                    assert!(Layout::parse(&lua).is_some_and(|read| read == layout));
                }
            }
        }
    }

    #[test]
    fn installing_twice_and_removing_restores_the_exact_init_file() {
        for original in [
            None,
            Some(&b""[..]),
            Some(&b"-- Keep this\r\nrex.action{}\r\n"[..]),
            Some(&b"\xef\xbb\xbf#!/bin/rex\n-- No newline"[..]),
        ] {
            let fixture = Fixture::folder();
            let store = store(&fixture);
            if let Some(original) = original {
                fs::write(&store.init, original).unwrap();
                fs::set_permissions(&store.init, fs::Permissions::from_mode(0o640)).unwrap();
            }
            store.save(Some(&Layout::default()), None, &|_| true).unwrap();
            let installed = fs::read(&store.init).unwrap();
            let (block, _) = managed(&installed, &loader(&store.rex_file())).unwrap();
            assert!(installed[block].starts_with(MANAGED));
            if original.is_some() {
                assert_eq!(fs::metadata(&store.init).unwrap().permissions().mode() & 0o777, 0o640);
            }
            store.save(Some(&Layout::default()), None, &|args| args == RELOAD.map(OsStr::new)).unwrap();
            assert_eq!(fs::read(&store.init).unwrap(), installed);
            store.save(Some(&off()), None, &|args| args == RELOAD.map(OsStr::new)).unwrap();
            assert_eq!(fs::read(&store.init).ok().as_deref(), original);
            assert!(!store.rex_file().exists() && !store.listener_file().exists());
            assert_eq!(store.layout().unwrap().0.panes, [false; 3]);
        }
    }

    #[test]
    fn a_failed_check_leaves_every_file_as_it_was() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        fs::write(&store.init, b"-- Keep this").unwrap();
        fs::write(&store.claude, b"{\"keep\":1}").unwrap();
        let run = |args: &[&OsStr]| {
            assert_eq!(&args[..2], &["config", "check"].map(OsStr::new));
            assert!(fs::read(args[2]).unwrap().starts_with(GENERATED.as_bytes()));
            // Even a SIGKILL during Rex validation cannot strand this file.
            assert_eq!(fs::File::open(args[2]).unwrap().metadata().unwrap().nlink(), 0);
            false
        };
        assert!(store.save(Some(&Layout::default()), Some(true), &run).unwrap_err().contains("could not check"));
        assert_eq!(fs::read(&store.init).unwrap(), b"-- Keep this");
        assert_eq!(fs::read(&store.claude).unwrap(), b"{\"keep\":1}");
        assert!(!store.config.exists());
        assert!(!store.rex_reload_pending() && !store.state.join(OWN_CLAUDE).exists());
    }

    #[test]
    fn a_failed_reload_restores_every_file_including_claude_settings() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let initial = b"-- Keep this";
        fs::write(&store.init, initial).unwrap();
        fs::write(&store.claude, b"{\"keep\":1}").unwrap();
        let run = |args: &[&OsStr]| args.get(1) == Some(&OsStr::new("check"));
        assert!(store.save(Some(&Layout::default()), Some(true), &run).unwrap_err().contains("put back"));
        assert_eq!(fs::read(&store.init).unwrap(), initial);
        assert_eq!(fs::read(&store.claude).unwrap(), b"{\"keep\":1}");
        assert!(!store.rex_file().exists() && !store.listener_file().exists());
        assert!(!store.state.join(OWN_CLAUDE).exists());
    }

    #[test]
    fn a_failed_reload_keeps_removal_and_leaves_a_reminder() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let initial = b"-- Keep my settings";
        let claude = b"{\"keep\":1}";
        fs::write(&store.init, initial).unwrap();
        fs::write(&store.claude, claude).unwrap();
        store.save(Some(&Layout::default()), Some(true), &|_| true).unwrap();

        let reject = |args: &[&OsStr]| {
            assert_eq!(args, RELOAD.map(OsStr::new));
            false
        };
        // Repair uses strict reloads, even when recovering a disabled layout.
        assert!(store.save_with(Some(&off()), Some(false), &reject, |_| Ok(())).unwrap_err().contains("put back"));
        assert!(store.layout().unwrap().1);
        assert!(store.claude() == Claude::On);
        assert!(!store.rex_reload_pending());

        let removed = store.save(Some(&off()), Some(false), &reject).unwrap();
        assert_eq!(removed.listener, Saved::Ready);
        assert!(removed.reload_failed);
        assert_eq!(fs::read(&store.init).unwrap(), initial);
        assert_eq!(fs::read(&store.claude).unwrap(), claude);
        assert!(!store.rex_file().exists() && !store.listener_file().exists());
        assert!(!store.state.join(OWN_CLAUDE).exists());
        assert!(store.rex_reload_pending());

        let (disabled, active) = store.layout().unwrap();
        assert!(!disabled.enabled() && !active);
        store.save_with(Some(&disabled), None, &|args| args == RELOAD.map(OsStr::new), |_| Ok(())).unwrap();
        assert!(!store.rex_reload_pending());
    }

    #[test]
    fn setup_removes_only_the_claude_status_line_it_installed() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let not_rex = |_: &[&OsStr]| panic!("Claude Code settings do not need Rex");
        let original = b"{\n    \"keep\": [1, 2]\n}\n";
        fs::write(&store.claude, original).unwrap();
        assert!(store.claude() == Claude::Off);
        store.save(None, Some(true), &not_rex).unwrap();
        assert!(store.claude() == Claude::On);
        store.save(None, Some(false), &not_rex).unwrap();
        assert_eq!(fs::read(&store.claude).unwrap(), original);
        store.save(None, Some(true), &not_rex).unwrap();
        fs::remove_file(store.state.join(OWN_CLAUDE)).unwrap();
        assert!(store.claude() == Claude::NotOwned);
        let before = fs::read(&store.claude).unwrap();
        assert!(store.save(None, Some(false), &not_rex).is_err());
        assert_eq!(fs::read(&store.claude).unwrap(), before);
    }

    #[test]
    fn linked_settings_are_edited_through_the_link_without_replacing_it() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let init = fixture.root.join("dotfiles/rex.lua");
        let claude = fixture.root.join("dotfiles/claude.json");
        fs::create_dir(init.parent().unwrap()).unwrap();
        fs::write(&init, b"-- Keep Rex settings\n").unwrap();
        fs::write(&claude, b"{\"keep\":1}\n").unwrap();
        symlink("dotfiles/rex.lua", &store.init).unwrap();
        symlink("dotfiles/claude.json", &store.claude).unwrap();

        assert!(!store.layout().unwrap().1);
        assert!(store.claude() == Claude::Off);
        store.save(Some(&Layout::default()), Some(true), &|_| true).unwrap();
        assert!(managed(&fs::read(&init).unwrap(), &loader(&store.rex_file())).is_some());
        assert!(store.claude_files().unwrap().launcher_matches());
        assert!(store.layout().unwrap().1);
        assert!(store.claude() == Claude::On);

        store.save(Some(&off()), Some(false), &|_| true).unwrap();
        assert_eq!(fs::read_link(&store.init).unwrap(), Path::new("dotfiles/rex.lua"));
        assert_eq!(fs::read_link(&store.claude).unwrap(), Path::new("dotfiles/claude.json"));
        assert_eq!(fs::read(init).unwrap(), b"-- Keep Rex settings\n");
        assert_eq!(fs::read(claude).unwrap(), b"{\"keep\":1}\n");
    }

    #[test]
    fn ownership_is_written_before_the_claude_status_line() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let mut changes = Vec::new();
        store.claude_files().unwrap().plan(true, &mut changes).unwrap();
        assert_eq!(changes.iter().map(Change::path).collect::<Vec<_>>(), [store.state.join(OWN_CLAUDE), store.claude]);
    }

    #[test]
    fn saving_a_layout_with_comments_and_crlf_keeps_hand_edits() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let run = |_: &[&OsStr]| true;
        store.save(Some(&Layout::default()), None, &run).unwrap();
        let file = store.rex_file();
        let hand_edited = fs::read_to_string(&file)
            .unwrap()
            .replace("-- Fraction of the tab for the left column.", "--[=[ Old width:\nlocal column_width = 0.52\n]=]")
            .replace("local column_width = 0.20", "  local column_width=0.27 -- My width")
            .replace("0.70", "0.67 -- My outline height")
            .replace("usage_rows = 2", "usage_rows = 7 -- My usage height")
            .replace("false\n", "false -- Only on demand\n")
            .replace("  history = true,", "  --[[\n  history = true,\n  ]]\n  history = false, -- Only when needed")
            .replace('\n', "\r\n");
        fs::write(&file, &hand_edited).unwrap();
        let (mut layout, installed) = store.layout().unwrap();
        assert!(installed);
        assert_eq!(layout.width, "0.27");
        assert_eq!(layout.outline_share, "0.67");
        assert_eq!(layout.usage_rows, "7");
        assert_eq!(layout.panes, [true, false, true]);
        store.save(Some(&layout), None, &|args| args == RELOAD.map(OsStr::new)).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), hand_edited);
        layout.toggle(TabPane::History);
        store.save(Some(&layout), None, &run).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), hand_edited.replace("history = false", "history = true"));
        assert!(store.layout().unwrap().0 == layout);
    }

    #[test]
    fn a_quoted_example_in_rex_settings_is_not_replaced_or_removed() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let original = b"local notes = [=[\n-- Managed by milo setup.\npcall(dofile, \"other.lua\")\n]=]\n";
        fs::write(&store.init, original).unwrap();
        store.save(Some(&Layout::default()), None, &|_| true).unwrap();
        assert!(fs::read(&store.init).unwrap().ends_with(original));
        store.save(Some(&off()), None, &|_| true).unwrap();
        assert_eq!(fs::read(&store.init).unwrap(), original);
    }

    #[test]
    fn moved_copied_or_commented_loaders_are_never_changed_or_installed_twice() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let load = loader(&store.rex_file());
        let block = format!("{}\n{load}\n", std::str::from_utf8(MANAGED).unwrap());
        for original in [
            format!("-- My Rex settings\n{block}"),
            format!("{block}-- My Rex settings\n{block}"),
            format!("-- {load}\n"),
            format!("{block}-- {load}\n"),
        ] {
            fs::write(&store.init, &original).unwrap();
            for layout in [Layout::default(), off()] {
                let result = store.save(Some(&layout), None, &|_| panic!("ambiguous settings need no Rex calls"));
                assert!(result.unwrap_err().contains("moved, copied or changed"));
                assert_eq!(fs::read_to_string(&store.init).unwrap(), original);
                assert!(!store.rex_file().exists());
            }
        }
    }

    #[test]
    fn saving_does_not_replace_read_only_settings_even_in_writable_folders() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        for (path, original, layout, limits) in [
            (&store.init, "-- Keep my settings", Some(Layout::default()), None),
            (&store.claude, r#"{"keep":true}"#, None, Some(true)),
        ] {
            fs::write(path, original).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
            let result = store.save(layout.as_ref(), limits, &|args| args[1] == "check");
            assert!(result.unwrap_err().contains("read-only"));
            assert_eq!(fs::read_to_string(path).unwrap(), original);
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o444);
            assert!(!store.rex_file().exists());
            assert!(!store.rex_reload_pending() && !store.state.join(OWN_CLAUDE).exists());
        }
    }

    #[test]
    fn saving_does_not_write_inside_a_linked_milo_folder() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let elsewhere = fixture.root.join("dotfiles");
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("notes"), "Keep these").unwrap();
        symlink(&elsewhere, &store.config).unwrap();
        let result = store.save(Some(&Layout::default()), None, &|_| panic!("cannot save through the link"));
        assert!(result.unwrap_err().contains("regular folder"));
        assert_eq!(fs::read_link(&store.config).unwrap(), elsewhere);
        assert_eq!(fs::read_to_string(elsewhere.join("notes")).unwrap(), "Keep these");
        assert!(!store.init.exists() && !elsewhere.join("rex.lua").exists());
    }

    #[test]
    fn another_setup_cannot_save_while_the_first_is_waiting_for_rex() {
        const TEST: &str = "setup::install::tests::another_setup_cannot_save_while_the_first_is_waiting_for_rex";
        const ROOT: &str = "MILO_TEST_SETUP_CONTENTION";
        if let Some(root) = env::var_os(ROOT) {
            let store = store_at(Path::new(&root));
            let second = store.save(Some(&off()), None, &|_| panic!("a blocked save must not call Rex"));
            assert!(second.unwrap_err().contains("Another setup is saving"));
            return;
        }
        let fixture = Fixture::folder();
        let store = store(&fixture);
        store
            .save(Some(&Layout::default()), None, &|args| {
                if args == RELOAD.map(OsStr::new) {
                    let child = Command::new(env::current_exe().unwrap())
                        .args(["--exact", TEST])
                        .env(ROOT, &fixture.root)
                        .output()
                        .unwrap();
                    let output = String::from_utf8_lossy(&child.stdout);
                    assert!(child.status.success() && output.contains("1 passed"), "{output}");
                }
                true
            })
            .unwrap();
        assert!(store.layout().unwrap().1);
        assert!(!store.rex_reload_pending());
    }

    #[test]
    fn saving_again_reloads_rex_if_the_last_save_stopped_at_reload() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        fs::write(&store.init, "-- Keep my settings").unwrap();
        let note = store.state.join(RELOAD_PENDING);
        let before_reload = RefCell::new(None);
        store
            .save(Some(&Layout::default()), None, &|args| {
                if args == RELOAD.map(OsStr::new) {
                    *before_reload.borrow_mut() = fs::read(&note).ok();
                }
                true
            })
            .unwrap();
        assert!(!note.exists());
        // Restore the state at the interrupted reload, without letting a normal rollback run.
        fs::write(&note, before_reload.into_inner().expect("a save must leave a reload reminder")).unwrap();
        let called = RefCell::new(false);
        store
            .save(Some(&Layout::default()), None, &|args| {
                assert_eq!(args, RELOAD.map(OsStr::new));
                *called.borrow_mut() = true;
                true
            })
            .unwrap();
        assert!(called.into_inner());
        assert!(!note.exists());
        assert!(fs::read_to_string(&store.init).unwrap().ends_with("-- Keep my settings"));
    }

    #[test]
    fn an_interrupted_install_keeps_the_layout_choices_already_saved() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        fs::create_dir(&store.state).unwrap();
        fs::write(store.state.join(RELOAD_PENDING), []).unwrap();
        fs::create_dir(&store.config).unwrap();
        let layout = Layout { panes: [true, false, true], width: "0.30".into(), ..Layout::default() };
        fs::write(store.rex_file(), rex_lua(&layout, &executable().unwrap(), store.listener_env())).unwrap();
        let (reopened, active) = store.layout().unwrap();
        assert!(!active && reopened == layout);
        store.save(Some(&reopened), None, &|_| true).unwrap();
        assert!(store.layout().unwrap().1);
        assert!(!store.rex_reload_pending());
    }

    #[test]
    fn an_interrupted_removal_keeps_the_disabled_layout() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        store.save(Some(&Layout::default()), None, &|_| true).unwrap();
        fs::write(store.state.join(RELOAD_PENDING), []).unwrap();
        fs::write(store.state.join(LAYOUT_DISABLED), []).unwrap();
        let (reopened, active) = store.layout().unwrap();
        assert!(active && !reopened.enabled());
        store.save(Some(&reopened), None, &|args| args == RELOAD.map(OsStr::new)).unwrap();
        assert!(!store.init.exists() && !store.rex_file().exists());
        assert!(!store.rex_reload_pending());
    }

    #[test]
    fn saving_does_not_replace_a_changed_ownership_note() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        fs::create_dir(&store.state).unwrap();
        let note = store.state.join(OWN_CLAUDE);
        fs::write(&note, "Keep this").unwrap();
        let result = store.save(None, Some(true), &|_| panic!("Claude Code settings do not need Rex"));
        assert!(result.unwrap_err().contains("note was changed"));
        assert_eq!(fs::read_to_string(&note).unwrap(), "Keep this");
        assert!(!store.claude.exists());
    }

    #[test]
    fn unrelated_rex_files_are_not_overwritten_on_install() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        fs::create_dir(&store.config).unwrap();
        let original = b"rex.action{ name = 'my_action' }\n";
        fs::write(store.rex_file(), original).unwrap();
        let result = store.save(Some(&Layout::default()), None, &|_| true);
        assert!(result.unwrap_err().contains("Your file was left alone."));
        assert_eq!(fs::read(store.rex_file()).unwrap(), original);
        assert!(!store.init.exists());
    }

    #[test]
    fn adding_a_pane_does_not_put_it_inside_a_comment() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        store.save(Some(&Layout::default()), None, &|_| true).unwrap();
        let file = store.rex_file();
        let note = "--[=[\n  history = true, -- Only an example\n  ]=]";
        let edited = fs::read_to_string(&file).unwrap().replace("  history = true,", "");
        fs::write(&file, edited.replace("local chosen = {", &format!("local chosen = {{ {note}"))).unwrap();

        let (mut layout, _) = store.layout().unwrap();
        assert!(!layout.chosen(TabPane::History));
        layout.toggle(TabPane::History);
        store.save(Some(&layout), None, &|_| true).unwrap();
        assert!(store.layout().unwrap().0.chosen(TabPane::History));
        assert!(fs::read_to_string(file).unwrap().contains(note));
    }

    #[test]
    fn setup_does_not_edit_ambiguous_or_quoted_layout_declarations() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let layout = Layout::default();
        store.save(Some(&layout), None, &|_| true).unwrap();
        let file = store.rex_file();
        let shipped = fs::read_to_string(&file).unwrap();
        let mut updated = layout;
        updated.next_width();
        updated.toggle(TabPane::Usage);
        for original in [
            shipped.replace("local column_width = 0.20", "local column_width = 0.20\nlocal column_width = 0.22"),
            shipped.replace("  usage = true,", "  usage = false,\n  usage = true,"),
            shipped.replace("local column_width = 0.20", "local column_width = 0.20 --[[]] local column_width = 0.30"),
            format!("{GENERATED}local example = [=[\n{}]=]\n", &shipped[GENERATED.len()..]),
        ] {
            fs::write(&file, &original).unwrap();
            let result = store.save(Some(&updated), None, &|_| panic!("could not prove what setup should edit"));
            assert!(result.unwrap_err().contains("Your file was left alone."));
            assert_eq!(fs::read_to_string(&file).unwrap(), original);
        }
    }

    #[test]
    fn enabling_new_tabs_does_not_overwrite_a_custom_listener() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        store.save(Some(&Layout::default()), None, &|_| true).unwrap();
        let saved = fs::read(store.rex_file()).unwrap();
        let listener = b"-- My own listener.\n";
        fs::write(store.listener_file(), listener).unwrap();
        let layout = Layout { new_tabs: true, ..Layout::default() };
        assert!(store.save(Some(&layout), None, &|_| true).unwrap_err().contains("listener file"));
        assert_eq!(fs::read(store.listener_file()).unwrap(), listener);
        assert_eq!(fs::read(store.rex_file()).unwrap(), saved);
    }

    #[test]
    fn an_unconditional_launcher_followed_by_user_code_is_left_alone() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let layout = Layout { new_tabs: true, ..Layout::default() };
        fs::write(&store.init, format!("{}\n{}\n", std::str::from_utf8(MANAGED).unwrap(), loader(&store.rex_file())))
            .unwrap();
        fs::create_dir(&store.config).unwrap();
        let original = rex_lua(&layout, &executable().unwrap(), store.listener_env()) + "-- Keep my notes here.\n";
        fs::write(store.rex_file(), &original).unwrap();
        let result = store.save(Some(&Layout::default()), None, &|_| panic!("can't safely turn off the old launcher"));
        assert!(result.unwrap_err().contains("Your file was left alone."));
        assert_eq!(fs::read_to_string(store.rex_file()).unwrap(), original);
    }

    #[test]
    fn removal_leaves_user_changes_to_generated_rex_files() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        store.save(Some(&Layout::default()), None, &|_| true).unwrap();
        let file = store.rex_file();
        let original = fs::read_to_string(&file).unwrap() + "\n-- Keep my notes here.\n";
        fs::write(&file, &original).unwrap();
        store.save(Some(&off()), None, &|_| true).unwrap();
        assert_eq!(fs::read_to_string(file).unwrap(), original);
        assert!(!store.init.exists());
    }

    #[test]
    fn a_new_claude_settings_file_uses_claudes_json_format() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        store.save(None, Some(true), &|_| panic!("Claude settings do not need Rex")).unwrap();
        let text = fs::read_to_string(&store.claude).unwrap();
        assert!(text.starts_with("{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"'/"));
        assert!(text.ends_with("' claude-status\"\n  }\n}\n"));
        assert!(store.claude_files().unwrap().launcher_matches());
    }

    #[test]
    fn removing_claude_status_leaves_a_reformatted_settings_file() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let not_rex = |_: &[&OsStr]| panic!("Claude Code settings do not need Rex");
        store.save(None, Some(true), &not_rex).unwrap();
        let text = fs::read_to_string(&store.claude).unwrap().replace('\n', "\r\n");
        fs::write(&store.claude, text).unwrap();
        store.save(None, Some(false), &not_rex).unwrap();
        assert_eq!(fs::read_to_string(&store.claude).unwrap(), "{\r\n  \r\n}\r\n");
    }
}
