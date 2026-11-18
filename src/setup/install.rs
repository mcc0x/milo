//! Coordinates settings changes so a failed Rex reload can put all files back.

use std::borrow::Cow;
use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::files::{self, Image};
use super::layout::Layout;
use super::lock;
use crate::paths;

const RELOAD: [&str; 2] = ["config", "reload"];

/// Lets setup use the real Rex command while tests use an isolated stand-in.
pub(super) type RunRex<'a> = dyn Fn(&[&OsStr]) -> bool + 'a;

/// The files may be saved even if the separate listener could not start or stop.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Saved {
    Ready,
    ListenerDidNotStart,
    ListenerDidNotStop,
}

/// Keeps the user settings and milo's files together for one setup transaction.
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

    /// Plan both sets of changes before writing so one failed plan cannot leave a partial save.
    pub fn save(&self, layout: Option<&Layout>, claude: Option<bool>, run: &RunRex<'_>) -> Result<Saved, &'static str> {
        let mut changes = Vec::new();
        let new_tabs = layout.is_some_and(|layout| layout.new_tabs);
        let rex = layout.map(|layout| self.plan_rex(layout, &mut changes, run)).transpose()?;
        let listener_running = new_tabs && lock::running(&self.state).unwrap_or(false);
        let reload = rex.as_ref().is_some_and(|plan| plan.reload) || (new_tabs && !listener_running);
        let restart = listener_running && rex.is_some_and(|plan| plan.listener_changed);
        if let Some(enabled) = claude {
            self.claude_files()?.plan(enabled, &mut changes)?;
        }
        let mut stopped = false;
        let installed = files::install(&changes, || {
            // A running listener has already loaded its script; a reload alone won't update it.
            stopped = restart && lock::stop(&self.state).is_ok();
            !reload || call(run, RELOAD)
        });
        if installed.is_err() && stopped {
            // The previous files are back, so give Rex a chance to start the old listener again.
            call(run, RELOAD);
        }
        installed?;
        Ok(if (restart && !stopped) || (layout.is_some() && !new_tabs && lock::stop(&self.state).is_err()) {
            Saved::ListenerDidNotStop
        } else if new_tabs && !lock::wait_running(&self.state).unwrap_or(false) {
            Saved::ListenerDidNotStart
        } else {
            Saved::Ready
        })
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
    use super::super::layout::{GENERATED, TabPane, WIDTHS, lua_path, rex_lua};
    use super::super::rex_files::{MANAGED, OFFERED, executable, managed};
    use super::*;
    use crate::fixture::Fixture;
    use crate::rex;
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::rc::Rc;

    fn store(fixture: &Fixture) -> Store {
        Store {
            init: fixture.root.join("init.lua"),
            config: fixture.root.join("config"),
            state: fixture.root.join("state"),
            claude: fixture.root.join("settings.json"),
        }
    }

    fn off() -> Layout {
        Layout { panes: [false; 3], ..Layout::default() }
    }

    #[test]
    fn generated_actions_use_only_the_chosen_panes_for_the_layout() {
        let executable = executable().unwrap();
        for width in WIDTHS.map(|width| width.proportion) {
            for mask in 0..8 {
                for new_tabs in [false, true] {
                    let panes = std::array::from_fn(|i| mask & (1 << i) != 0);
                    let layout = Layout { width: width.into(), panes, new_tabs, ..Layout::default() };
                    let lua = rex_lua(&layout, &executable);
                    assert!(lua.contains("local usage_rows = 5\n"));
                    let (_, choices) = lua.split_once("local chosen = {\n").unwrap();
                    let (choices, _) = choices.split_once("}\n").unwrap();
                    for (name, chosen) in TabPane::ALL.map(TabPane::name).into_iter().zip(panes) {
                        assert_eq!(choices.contains(&format!("{name} = true,")), chosen);
                        assert!(lua.contains(&format!("name = \"milo_{name}\"")));
                        assert!(lua.contains(&format!("\"milo:{name}\"")));
                    }
                    assert!(lua.contains("name = \"milo_layout\""));
                    assert!(lua.contains("name = \"milo_close\""));
                    assert_eq!(lua.contains("rex.exec.start"), new_tabs);
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
            let (block, _) = managed(&installed).unwrap();
            assert!(installed[block].starts_with(MANAGED));
            if original.is_some() {
                assert_eq!(fs::metadata(&store.init).unwrap().permissions().mode() & 0o777, 0o640);
            }
            store.save(Some(&Layout::default()), None, &|_| panic!("an unchanged layout needs no Rex calls")).unwrap();
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
        let checked = Rc::new(RefCell::new(None));
        let path = Rc::clone(&checked);
        let run = move |args: &[&OsStr]| {
            assert_eq!(&args[..2], &["config", "check"].map(OsStr::new));
            assert!(fs::read(args[2]).unwrap().starts_with(GENERATED.as_bytes()));
            *path.borrow_mut() = Some(PathBuf::from(args[2]));
            false
        };
        assert!(store.save(Some(&Layout::default()), Some(true), &run).unwrap_err().contains("could not check"));
        assert_eq!(fs::read(&store.init).unwrap(), b"-- Keep this");
        assert_eq!(fs::read(&store.claude).unwrap(), b"{\"keep\":1}");
        assert!(!store.config.exists() && !store.state.exists());
        assert!(!checked.borrow().as_ref().unwrap().exists());
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
    fn setup_is_only_offered_once_and_never_when_already_installed() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        store.save(Some(&Layout::default()), None, &|_| true).unwrap();
        assert!(!store.offer_once());
        assert!(!store.state.join(OFFERED).exists());
        store.save(Some(&off()), None, &|_| true).unwrap();
        assert!(store.offer_once());
        assert!(!store.offer_once());
    }

    #[test]
    fn an_unsaved_layout_can_be_tried_in_its_own_block_without_installing() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let target = rex::Target { session: "session:test".into(), block: "block:test".into() };
        let command = Rc::new(RefCell::new(Vec::new()));
        let record = Rc::clone(&command);
        store
            .arrange(&Layout::default(), &target, &move |args| {
                assert!(fs::read_to_string(args[1]).unwrap().contains("name = \"milo_layout\""));
                *record.borrow_mut() = args.iter().map(|arg| arg.to_os_string()).collect();
                true
            })
            .unwrap();
        let args = command.borrow();
        assert_eq!(args[0], "do");
        assert_eq!(
            args[2..],
            ["--action", "milo_layout", "-s", "session:test", "-b", "block:test"].map(OsString::from)
        );
        assert!(!Path::new(&args[1]).exists());
        assert!(!store.init.exists() && !store.config.exists());
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
        assert!(managed(&fs::read(&init).unwrap()).is_some());
        assert!(fs::read_to_string(&claude).unwrap().contains("milo claude-status"));
        assert!(store.layout().unwrap().1);
        assert!(store.claude() == Claude::On);
        assert!(!store.offer_once());

        store.save(Some(&off()), Some(false), &|_| true).unwrap();
        assert_eq!(fs::read_link(&store.init).unwrap(), Path::new("dotfiles/rex.lua"));
        assert_eq!(fs::read_link(&store.claude).unwrap(), Path::new("dotfiles/claude.json"));
        assert_eq!(fs::read(init).unwrap(), b"-- Keep Rex settings\n");
        assert_eq!(fs::read(claude).unwrap(), b"{\"keep\":1}\n");
    }

    #[test]
    fn generated_lua_uses_the_canonical_running_executable_for_panes_and_listener() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let layout = Layout { new_tabs: true, ..Layout::default() };
        assert_eq!(store.save(Some(&layout), None, &|_| true).unwrap(), Saved::ListenerDidNotStart);
        let lua = fs::read_to_string(store.rex_file()).unwrap();
        let executable = fs::canonicalize(env::current_exe().unwrap()).unwrap();
        assert!(executable.is_absolute());
        assert!(lua.contains(&format!("local milo = {}\n", lua_path(&executable))));
        assert!(lua.contains("command = { milo, name }"));
        assert!(lua.contains("rex.exec.start({ milo, \"listen\" }"));
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
            .replace("local column_width = 0.22", "  local column_width=0.27 -- My width")
            .replace("0.70", "0.67 -- My outline height")
            .replace("usage_rows = 5", "usage_rows = 7 -- My usage height")
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
        store.save(Some(&layout), None, &|_| panic!("an unchanged layout needs no Rex calls")).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), hand_edited);
        layout.toggle(TabPane::History);
        store.save(Some(&layout), None, &run).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), hand_edited.replace("history = false", "history = true"));
        assert!(store.layout().unwrap().0 == layout);
    }

    #[test]
    fn default_setup_neither_checks_nor_installs_the_unused_listener() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let calls = Rc::new(RefCell::new(0));
        let recorded = Rc::clone(&calls);
        store
            .save(Some(&Layout::default()), None, &move |_| {
                *recorded.borrow_mut() += 1;
                true
            })
            .unwrap();
        assert_eq!(*calls.borrow(), 2);
        assert!(!store.listener_file().exists());
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
        assert_eq!(
            fs::read_to_string(&store.claude).unwrap(),
            "{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"milo claude-status\"\n  }\n}\n"
        );
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
