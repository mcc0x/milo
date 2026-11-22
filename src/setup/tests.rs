use super::*;
use crate::fixture::Fixture;
use crate::preferences::Show;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

fn store(fixture: &Fixture) -> Store {
    Store {
        init: fixture.root.join("init.lua"),
        config: fixture.root.join("config"),
        state: fixture.root.join("state"),
        claude: fixture.root.join("settings.json"),
        claude_state: None,
    }
}

fn frame(pane: &mut dyn Pane, width: usize, height: usize) -> Vec<String> {
    let mut screen = Screen::sized(width, height);
    pane.draw(&mut screen);
    screen.text()
}

fn click_at(rows: &[String], y: usize, label: &str) -> Key {
    let at = rows[y].find(label).unwrap_or_else(|| panic!("missing {label:?} in {:?}", rows[y]));
    Key::Click(crate::width::str_width(&rows[y][..at]), y)
}

fn click_label(rows: &[String], label: &str) -> Key {
    let y = rows.iter().position(|row| row.contains(label)).unwrap_or_else(|| panic!("missing {label:?}: {rows:?}"));
    click_at(rows, y, label)
}

fn choose<R: Fn(&[&OsStr]) -> bool>(pane: &mut Setup<R>, row: Row) -> After {
    pane.selected = pane.rows().iter().position(|&it| it == row).expect("visible setup choice");
    pane.key(Key::Enter)
}

// Exercise real config paths and the Rex CLI transport without changing the
// environment of other tests, or touching the user's config or sessions.
fn isolated_settings(name: &str) -> Option<PathBuf> {
    isolated_settings_with_claude(name, None)
}

fn isolated_settings_with_claude(name: &str, profile: Option<&str>) -> Option<PathBuf> {
    const ROOT: &str = "MILO_TEST_SETTINGS_ROOT";
    if let Some(root) = std::env::var_os(ROOT) {
        return Some(root.into());
    }
    let f = Fixture::folder();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture"])
        .env(ROOT, &f.root)
        .env("HOME", &f.root)
        .env("XDG_STATE_HOME", f.root.join("state"))
        .env_remove("XDG_DATA_HOME")
        .env_remove("OPENCODE_DB")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("REX_CONFIG")
        .env_remove("REX_SESSION")
        .env_remove("REX_BLOCK")
        .env("XDG_CONFIG_HOME", &f.root)
        .env("PATH", &f.root)
        .current_dir(&f.root);
    if let Some(dir) = profile {
        command.env("CLAUDE_CONFIG_DIR", dir);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    None
}

#[test]
fn claude_setup_and_doctor_use_only_the_selected_profile() {
    let Some(root) = isolated_settings_with_claude(
        "setup::tests::claude_setup_and_doctor_use_only_the_selected_profile",
        Some("alternate"),
    ) else {
        return;
    };
    let active = Store::real().unwrap();
    let default = Store { claude: root.join(".claude/settings.json"), claude_state: None, ..active.clone() };
    let profile = paths::claude().unwrap();
    assert_eq!(active.claude, root.join("alternate/settings.json"));
    assert_eq!(active.claude_state_dir(), profile.state().unwrap());
    assert_ne!(active.claude_note(), default.claude_note());

    let report = doctor::report_for(&active, None).unwrap();
    assert!(report.contains("Claude status line: not installed"), "{report}");
    assert!(needs_welcome().unwrap());
    assert!(!active.state.exists() && !active.claude.exists());

    let no_rex = |_: &[&OsStr]| panic!("Claude settings do not need Rex");
    let mut pane = Setup::new(Some(active.clone()), false, None, None, no_rex);
    assert_eq!(choose(&mut pane, Row::ClaudeLimits), After::Stay);
    assert_eq!(choose(&mut pane, Row::SaveClaude), After::Stay);
    assert!(active.claude() == Claude::On);
    assert!(active.claude_note().is_file());
    assert!(!needs_welcome().unwrap());
    assert!(!default.claude.exists() && !default.claude_note().exists());

    // The same launcher in another profile isn't owned until Milo installs it there.
    fs::create_dir_all(default.claude.parent().unwrap()).unwrap();
    fs::copy(&active.claude, &default.claude).unwrap();
    let report = doctor::report_for(&default, None).unwrap();
    assert!(report.contains("Claude status line: no matching ownership note"), "{report}");
    fs::remove_file(&default.claude).unwrap();
    default.save(None, Some(true), &no_rex).unwrap();
    let default_before = fs::read(&default.claude).unwrap();
    let default_note = fs::read(default.claude_note()).unwrap();
    let report = doctor::report_for(&active, None).unwrap();
    assert!(report.contains("Claude status line: managed by Milo"), "{report}");

    let removed = doctor::remove().unwrap();
    assert!(removed.contains("Removed managed integrations"), "{removed}");
    assert!(!active.claude.exists() && !active.claude_note().exists());
    assert_eq!(fs::read(&default.claude).unwrap(), default_before);
    assert_eq!(fs::read(default.claude_note()).unwrap(), default_note);
}

#[test]
fn dirty_exit_keeps_the_editor_by_default_and_can_discard_by_click() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("outside Rex"));
    assert_eq!(pane.key(Key::Esc), After::Leave, "a clean exit does not prompt");
    assert_eq!(pane.key(Key::Char('2')), After::Stay);
    let clean = frame(&mut pane, 80, 18);
    assert!(!clean[0].contains("Unsaved changes"));
    assert_eq!(pane.key(click_label(&clean, "Activity marks")), After::Stay);
    let edited = frame(&mut pane, 80, 18);
    assert!(edited[0].contains("Unsaved changes"));
    let quit = click_at(&edited, 17, "q quit");
    assert_eq!(pane.key(quit), After::Stay);
    assert_eq!(pane.panel, Panel::Exit);
    assert_eq!(pane.key(quit), After::Ignored, "the previous frame can't activate the prompt");

    let prompt = frame(&mut pane, 18, 8);
    assert!(prompt[0].contains("Unsaved changes"));
    assert!(prompt.iter().any(|row| row.starts_with('›') && row.contains("Keep editing")));
    assert!(prompt.iter().any(|row| row.trim() == "Save"));
    assert!(prompt.iter().any(|row| row.trim() == "Discard"));
    assert_eq!(pane.key(Key::Enter), After::Stay);
    assert_eq!(pane.panel, Panel::Appearance);
    assert!(pane.rows()[pane.selected] == Row::Activity);
    assert!(pane.prefs.static_marks);

    assert_eq!(pane.key(Key::Esc), After::Stay);
    let prompt = frame(&mut pane, 18, 8);
    let discard = click_label(&prompt, "Discard");
    let Key::Click(x, y) = discard else { unreachable!() };
    assert!(pane.hover_span(x, y).is_some());
    pane.modal_click.opened(x, y);
    assert_eq!(pane.key(discard), After::Ignored);
    pane.modal_click.clear();
    assert_eq!(pane.key(discard), After::Leave);
    assert!(!f.root.join("settings.json").exists());
}

#[test]
fn dirty_settings_from_home_asks_before_returning() {
    let Some(_) = isolated_settings("setup::tests::dirty_settings_from_home_asks_before_returning") else { return };
    let mut home = crate::home::Home::new();
    let rows = frame(&mut home, 80, 18);
    assert_eq!(home.key(click_label(&rows, "Terminal settings")), After::Stay);
    home.key(Key::Char('2'));
    let rows = frame(&mut home, 80, 18);
    assert_eq!(home.key(click_label(&rows, "Activity marks")), After::Stay);
    assert_eq!(home.key(Key::Char('q')), After::Stay);
    assert!(frame(&mut home, 80, 18)[0].contains("Unsaved changes"));
    assert_eq!(home.key(Key::Enter), After::Stay);
    assert!(frame(&mut home, 80, 18)[0].contains("Appearance"));
    home.key(Key::Esc);
    let prompt = frame(&mut home, 80, 18);
    assert_eq!(home.key(click_label(&prompt, "Discard")), After::Stay);
    let menu = frame(&mut home, 80, 18);
    assert!(menu.iter().any(|row| row.contains("Terminal settings")));
    assert!(menu.iter().any(|row| row.contains("Uninstall")));
    assert_eq!(Preferences::resolve(None).unwrap().1, Source::Default);
}

#[test]
fn toggling_back_and_saving_claude_directly_clear_the_dirty_indicator() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("outside Rex"));
    choose(&mut pane, Row::FollowActivity);
    assert!(frame(&mut pane, 18, 8)[0].contains("Unsaved changes"));
    choose(&mut pane, Row::FollowActivity);
    assert!(!frame(&mut pane, 18, 8)[0].contains("Unsaved changes"));
    choose(&mut pane, Row::ClaudeLimits);
    assert!(frame(&mut pane, 18, 8)[0].contains("Unsaved changes"));
    choose(&mut pane, Row::SaveClaude);
    assert!(!frame(&mut pane, 18, 8)[0].contains("Unsaved changes"));
    assert_eq!(pane.key(Key::Char('q')), After::Leave);
}

#[test]
fn exit_save_scopes_preferences_then_saves_claude() {
    let Some(root) = isolated_settings("setup::tests::exit_save_scopes_preferences_then_saves_claude") else {
        return;
    };
    let f = Fixture::folder();
    let store = Store { config: root.join("milo"), ..store(&f) };
    let mut pane = Setup::new(Some(store), false, None, Some(root.join("worktree")), |_| panic!("outside Rex"));
    pane.key(Key::Char('2'));
    choose(&mut pane, Row::Activity);
    pane.key(Key::Char('3'));
    choose(&mut pane, Row::ChangesOnly);
    choose(&mut pane, Row::ClaudeLimits);
    pane.key(Key::Char('q'));
    assert_eq!(choose(&mut pane, Row::SaveAndExit), After::Stay);
    let rows = frame(&mut pane, 80, 18);
    assert!(rows.iter().any(|row| row.contains("Save preferences for…")));
    assert_eq!(pane.key(Key::Enter), After::Stay, "the default scope choice goes back");
    assert_eq!(pane.panel, Panel::Exit);
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert_eq!(pane.panel, Panel::Workflow);
    assert_eq!(Preferences::resolve(pane.root.as_deref()).unwrap().1, Source::Default);
    assert!(!f.root.join("settings.json").exists());

    pane.key(Key::Esc);
    choose(&mut pane, Row::SaveAndExit);
    let rows = frame(&mut pane, 80, 18);
    assert_eq!(pane.key(click_label(&rows, "For this worktree")), After::Leave);
    let (saved, source) = Preferences::resolve(pane.root.as_deref()).unwrap();
    assert_eq!(source, Source::Worktree);
    assert!(saved.static_marks && saved.changes_only);
    assert_eq!(Preferences::resolve(None).unwrap().1, Source::Default);
    assert!(matches!(pane.store().unwrap().claude(), Claude::On));
}

#[test]
fn global_preference_save_rejects_stale_edits_and_does_not_copy_worktree_settings() {
    let Some(root) =
        isolated_settings("setup::tests::global_preference_save_rejects_stale_edits_and_does_not_copy_worktree_settings")
    else {
        return;
    };
    let worktree = root.join("worktree");
    Preferences::set_changes_only(&worktree, true).unwrap();
    let global = root.join("milo/preferences.conf");
    fs::write(&global, "icons = nerd\nchanges_only = false\n").unwrap();

    let store = Store::real().unwrap();
    let mut pane = Setup::new(Some(store.clone()), false, None, Some(worktree.clone()), |_| panic!("outside Rex"));
    choose(&mut pane, Row::HistoryFollow);
    choose(&mut pane, Row::FollowProject);
    pane.key(Key::Char('2'));
    choose(&mut pane, Row::Icons);
    let newer = "icons = off\nchanges_only = false\nfollow_activity = false\n";
    fs::write(&global, newer).unwrap();
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveGlobal), After::Stay);
    assert_eq!(pane.message.as_ref().unwrap().0, PREFS_CHANGED);
    assert!(pane.prefs_dirty());
    assert_eq!(fs::read_to_string(&global).unwrap(), newer);

    let mut pane = Setup::new(Some(store), false, None, Some(worktree.clone()), |_| panic!("outside Rex"));
    choose(&mut pane, Row::HistoryFollow);
    choose(&mut pane, Row::FollowProject);
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveGlobal), After::Stay);

    let expected = Preferences {
        icons: IconMode::Off,
        follow_activity: false,
        history_follow_new: true,
        project_mode: ProjectMode::Pin,
        ..Preferences::default()
    };
    assert_eq!(Preferences::resolve(None).unwrap(), (expected, Source::Global));
    assert_eq!(
        Preferences::resolve(Some(&worktree)).unwrap(),
        (Preferences { changes_only: true, ..expected }, Source::Worktree),
    );
    assert!(!pane.prefs_dirty());
}

#[test]
fn usage_tool_choices_cycle_in_appearance_and_save_to_the_chosen_scope() {
    let Some(root) =
        isolated_settings("setup::tests::usage_tool_choices_cycle_in_appearance_and_save_to_the_chosen_scope")
    else {
        return;
    };
    let worktree = root.join("worktree");
    let mut pane = Setup::new(Store::real(), false, None, Some(worktree.clone()), |_| panic!("outside Rex"));
    pane.key(Key::Char('2'));
    let rows = frame(&mut pane, 80, 24);
    for label in ["Codex usage", "Claude Code usage", "OpenCode usage"] {
        assert!(rows.iter().any(|row| row.contains(label) && row.contains("Auto")), "{label}: {rows:?}");
    }

    let opencode = Row::UsageTool(UsageTool::OpenCode);
    choose(&mut pane, opencode);
    let rows = frame(&mut pane, 80, 24);
    assert!(rows.iter().any(|row| row.contains("OpenCode usage") && row.contains("Always show")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("Always show keeps its place")), "{rows:?}");
    choose(&mut pane, opencode);
    assert!(frame(&mut pane, 18, 12).iter().any(|row| row.starts_with('›') && row.ends_with("Hide")));
    assert!(pane.prefs_dirty());
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveGlobal), After::Stay);
    let (saved, source) = Preferences::resolve(Some(&worktree)).unwrap();
    assert_eq!((saved.usage_tools[UsageTool::OpenCode], source), (Show::Hide, Source::Global));

    // A worktree can show a tool the global choice hides, and keeps inheriting the rest.
    assert_eq!(pane.panel, Panel::Appearance);
    assert!(pane.selected_row() == Some(opencode));
    choose(&mut pane, opencode);
    choose(&mut pane, Row::UsageTool(UsageTool::Codex));
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveWorktree), After::Stay);
    let choices = |prefs: Preferences| UsageTool::ALL.map(|tool| prefs.usage_tools[tool]);
    let (local, source) = Preferences::resolve(Some(&worktree)).unwrap();
    assert_eq!((choices(local), source), ([Show::Always, Show::Auto, Show::Auto], Source::Worktree));
    assert_eq!(choices(Preferences::resolve(None).unwrap().0), [Show::Auto, Show::Auto, Show::Hide]);
    assert!(!pane.prefs_dirty());
}

#[test]
fn terminal_save_feedback_names_the_scope_and_reports_the_effect() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, Some(f.root.join("checkout")), |_| panic!("outside Rex"));

    choose(&mut pane, Row::ChangesOnly);
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveWorktree), After::Stay);
    assert_eq!(pane.message.as_ref().unwrap().0, "Saved pane preferences for “checkout”. Reopen panes to apply.");
    let rows = frame(&mut pane, 24, 16);
    assert!(rows.iter().any(|row| row.contains("“checkout”")));
    assert!(rows.iter().map(|row| row.trim()).collect::<Vec<_>>().join(" ").contains(REOPEN_PANES));

    choose(&mut pane, Row::ChangesOnly);
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveGlobal), After::Stay);
    assert_eq!(pane.message.as_ref().unwrap().0, "Saved global pane preferences. “checkout” keeps its preferences.");

    choose(&mut pane, Row::TabTitles);
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveGlobal), After::Stay);
    assert_eq!(
        pane.message.as_ref().unwrap().0,
        "Saved global pane preferences. Milo uses the title setting when naming Rex tabs.",
    );

    choose(&mut pane, Row::UseGlobalPrefs);
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert_eq!(pane.message.as_ref().unwrap().0, "“checkout” now uses global pane preferences. Reopen panes to apply.");

    for effect in ["enabled", "disabled"] {
        choose(&mut pane, Row::ClaudeLimits);
        assert_eq!(choose(&mut pane, Row::SaveClaude), After::Stay);
        assert_eq!(
            pane.message.as_ref().unwrap().0,
            format!("Saved Claude Code account settings. Milo's status line is {effect}."),
        );
    }
}

#[test]
fn worktree_preference_save_keeps_other_fields_inherited_or_overridden() {
    let Some(root) =
        isolated_settings("setup::tests::worktree_preference_save_keeps_other_fields_inherited_or_overridden")
    else {
        return;
    };
    let worktree = root.join("worktree");
    let config = root.join("milo");
    fs::create_dir(&config).unwrap();
    let global = config.join("preferences.conf");
    let global_before = "icons = off\nchanges_only = false\n";
    fs::write(&global, global_before).unwrap();

    let mut pane = Setup::new(Store::real(), false, None, Some(worktree.clone()), |_| panic!("outside Rex"));
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveWorktree), After::Stay);
    assert!(!config.join("worktrees").exists(), "saving no edits creates no override");

    Preferences::set_changes_only(&worktree, false).unwrap();
    let mut pane = Setup::new(Store::real(), false, None, Some(worktree.clone()), |_| panic!("outside Rex"));
    choose(&mut pane, Row::HistoryFollow);
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveWorktree), After::Stay);
    assert_eq!(fs::read_to_string(&global).unwrap(), global_before);
    assert!(!pane.prefs_dirty());

    fs::write(&global, "icons = nerd\nchanges_only = true\nfollow_activity = false\n").unwrap();
    let expected = Preferences { icons: IconMode::Nerd, follow_activity: false, ..Preferences::default() };
    assert_eq!(
        Preferences::resolve(Some(&worktree)).unwrap(),
        (Preferences { history_follow_new: true, ..expected }, Source::Worktree),
    );
}

#[test]
fn stale_terminal_preference_inheritance_keeps_the_newer_worktree_layer() {
    let f = Fixture::folder();
    let store = store(&f);
    let root = Some(f.root.join("checkout"));
    let no_rex = |_: &[&OsStr]| panic!("preferences do not call Rex");
    let mut newer = Setup::new(Some(store.clone()), false, None, root.clone(), no_rex);
    choose(&mut newer, Row::HistoryFollow);
    newer.key(Key::Char('s'));
    assert_eq!(choose(&mut newer, Row::SaveWorktree), After::Stay);
    assert_eq!(newer.prefs_scope(), Source::Worktree);

    let mut stale = Setup::new(Some(store.clone()), false, None, root.clone(), no_rex);
    choose(&mut newer, Row::ChangesOnly);
    newer.key(Key::Char('s'));
    assert_eq!(choose(&mut newer, Row::SaveWorktree), After::Stay);

    assert_eq!(choose(&mut stale, Row::UseGlobalPrefs), After::Stay);
    assert_eq!(choose(&mut stale, Row::Continue), After::Stay);
    assert_eq!(stale.message.as_ref().unwrap().0, PREFS_CHANGED);
    let (kept, scope) = Preferences::resolve_in(&store.config, root.as_deref()).unwrap();
    assert_eq!(scope, Source::Worktree);
    assert!(kept.history_follow_new && kept.changes_only);
}

#[test]
fn exit_layout_save_reports_failure_and_requires_global_scope_for_auto_arrange() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, Some(f.root.clone()), |_| false);
    choose(&mut pane, Row::Tune);
    choose(&mut pane, Row::Side);
    pane.key(Key::Char('q'));
    assert_eq!(choose(&mut pane, Row::SaveAndExit), After::Stay);
    assert_eq!(pane.panel, Panel::Save(SaveKind::Layout));
    assert_eq!(choose(&mut pane, Row::SaveWorktree), After::Stay);
    assert!(pane.layout_dirty());
    let failed = frame(&mut pane, 80, 18);
    assert!(failed.iter().any(|row| row.contains("could not check")));
    assert!(!f.root.join("init.lua").exists());
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert_eq!(pane.panel, Panel::Tune);
    assert!(pane.rows()[pane.selected] == Row::Side);

    choose(&mut pane, Row::Side);
    choose(&mut pane, Row::AutoArrange);
    assert_eq!(pane.key(Key::Char('s')), After::Stay);
    assert!(!pane.rows().contains(&Row::SaveWorktree), "a manual save cannot create an unrelated layout override");
    assert_eq!(pane.key(Key::Esc), After::Stay);
    pane.key(Key::Char('q'));
    choose(&mut pane, Row::SaveAndExit);
    let scope = frame(&mut pane, 80, 18);
    assert!(scope.iter().any(|row| row.contains("Auto-arrange needs a global save")));
    assert!(!pane.rows().contains(&Row::SaveWorktree));
    assert!(pane.rows().contains(&Row::SaveGlobal));
}

#[test]
fn repeating_the_worktree_save_click_cannot_save_auto_arrange_globally() {
    const THIS_TEST: &str = "setup::tests::repeating_the_worktree_save_click_cannot_save_auto_arrange_globally";
    const LISTENER_STATE: &str = "MILO_TEST_SCOPE_LISTENER_STATE";
    if let Some(state) = std::env::var_os(LISTENER_STATE) {
        let state = Path::new(&state);
        lock::listen(state, &state.with_file_name("config").join("listener.lua")).unwrap();
        return;
    }

    let f = Fixture::folder();
    // Rex reload has to establish a process-owned listener lock. A fake
    // successful exit code alone leaves the editor on the warning screen.
    f.write("rex", "#!/bin/sh\nexec /bin/sleep 30\n");
    fs::set_permissions(f.root.join("rex"), fs::Permissions::from_mode(0o700)).unwrap();
    let store = store(&f);
    let state = store.state.clone();
    let listener = std::cell::RefCell::new(None);
    let run = |args: &[&OsStr]| {
        if args == ["config", "reload"] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", THIS_TEST])
                .env(LISTENER_STATE, &state)
                .env("PATH", &f.root)
                .spawn()
                .unwrap();
            if !(0..5).any(|_| lock::wait_running(&state).unwrap()) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the fixture listener did not start");
            }
            *listener.borrow_mut() = Some(child);
        }
        true
    };
    let mut pane = Setup::new(Some(store), true, None, Some(f.root.clone()), run);
    choose(&mut pane, Row::Tree);
    choose(&mut pane, Row::Tune);
    choose(&mut pane, Row::AutoArrange);
    pane.key(Key::Char('q'));
    assert_eq!(choose(&mut pane, Row::SaveAndExit), After::Stay);
    assert_eq!(pane.key(Key::End), After::Stay);

    let rows = frame(&mut pane, 32, 4);
    let worktree = click_label(&rows, "For this worktree");
    let saved = pane.key(worktree);
    let mut child = listener.borrow_mut().take().expect("the save reloaded Rex");
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(saved, After::Stay);
    assert_eq!(pane.panel, Panel::Save(SaveKind::Layout));
    assert!(pane.message.as_ref().unwrap().0.contains("Auto-arrange still needs a global save."));
    let rows = frame(&mut pane, 32, 4);
    assert_eq!(click_label(&rows, "As global default"), worktree);
    assert_eq!(pane.key(worktree), After::Ignored);
    assert!(pane.auto_arrange_dirty());
    assert!(!pane.saved.as_ref().unwrap().new_tabs);
}

#[test]
fn leaving_only_undoes_the_trial_after_an_exit_choice_and_stays_on_undo_failure() {
    let Some(root) =
        isolated_settings("setup::tests::leaving_only_undoes_the_trial_after_an_exit_choice_and_stays_on_undo_failure")
    else {
        return;
    };
    let rex = root.join("rex");
    fs::write(
        &rex,
        concat!(
            "#!/bin/sh\n/bin/cat >/dev/null\n",
            "for arg do case \"$arg\" in\n",
            "milo_operation=trial) echo 'ok window:test tiled outline=block:test'; exit;;\n",
            "milo_operation=undo) echo undo >> trial.log; if [ -f allow-undo ]; then echo ok; else echo blocked; fi; exit;;\n",
            "esac; done\nexit 1\n",
        ),
    )
    .unwrap();
    fs::set_permissions(rex, fs::Permissions::from_mode(0o700)).unwrap();
    let f = Fixture::folder();
    let target = rex::Target { session: "session:test".into(), block: "block:shell".into() };
    let mut pane = Setup::new(Some(store(&f)), true, Some(target), None, |_| panic!("discard never saves"));
    choose(&mut pane, Row::Tree);
    pane.key(Key::Char('a'));
    assert!(pane.trial.is_some());
    pane.key(Key::Char('q'));
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert!(!root.join("trial.log").exists());

    pane.key(Key::Char('q'));
    assert_eq!(choose(&mut pane, Row::Discard), After::Stay);
    assert_eq!(pane.panel, Panel::Layout);
    assert!(pane.layout_dirty() && pane.trial.is_some());
    assert!(pane.message.as_ref().unwrap().0.contains("Could not undo"));

    fs::write(root.join("allow-undo"), "").unwrap();
    pane.key(Key::Esc);
    assert_eq!(choose(&mut pane, Row::Discard), After::Leave);
    assert!(pane.trial.is_none());
    assert_eq!(fs::read_to_string(root.join("trial.log")).unwrap(), "undo\nundo\n");
    assert!(!f.root.join("init.lua").exists());
}

#[test]
fn presets_and_tuning_change_the_diagram_but_not_the_settings() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| panic!("drawing does not call Rex"));
    pane.tab_size = Some((124, 58));
    let before = frame(&mut pane, 110, 27);
    for label in [
        "Just the tree",
        "Project sidebar",
        "Use this tab’s layout",
        "Outline",
        "History",
        "Usage",
        "Your work",
        "124×58",
    ] {
        assert!(before.iter().any(|line| line.contains(label)), "missing {label}: {before:?}");
    }
    assert_eq!(choose(&mut pane, Row::Tree), After::Stay);
    let tree = frame(&mut pane, 110, 27);
    assert!(tree.iter().any(|line| line.contains("Draft preview")));
    assert!(!tree.iter().any(|line| line.contains("History")));
    assert_eq!(choose(&mut pane, Row::Tune), After::Stay);
    assert_eq!(choose(&mut pane, Row::Side), After::Stay);
    assert_eq!(choose(&mut pane, Row::Width), After::Stay);
    let tuned = frame(&mut pane, 110, 27);
    assert!(tuned.iter().any(|line| line.contains("30% right")));
    assert!(tuned.iter().any(|line| line.contains("Medium")));
    assert!(!f.root.join("init.lua").exists());
}

#[test]
fn short_pane_keeps_the_chosen_row_and_a_way_back() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| panic!("drawing does not call Rex"));
    choose(&mut pane, Row::Tune);
    pane.selected = TUNE.iter().position(|&row| row == Row::AutoArrange).unwrap();
    let rows = frame(&mut pane, 28, 5);
    assert!(rows.iter().any(|row| row.starts_with('›') && row.contains("New tabs")));
    assert!(!rows.iter().any(|row| row.contains('┌')));
    assert!(rows[4].contains("Esc back"));
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert_eq!(pane.panel, Panel::Layout);
}

#[test]
fn one_row_settings_keeps_the_selected_control_visible_and_clickable() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("draft changes do not call Rex"));
    pane.key(Key::Char('2'));
    let rows = frame(&mut pane, 42, 1);
    assert!(rows[0].starts_with('›') && rows[0].contains("Icons"));
    assert_eq!(pane.key(Key::Up), After::Ignored);
    assert!(pane.hover_span(2, 0).is_some(), "a no-op move keeps the current hit target");
    assert_eq!(pane.key(Key::WheelDown), After::Stay);
    let rows = frame(&mut pane, 42, 1);
    assert!(rows[0].starts_with('›') && rows[0].contains("Activity"));
    assert_eq!(pane.key(click_at(&rows, 0, "Activity marks")), After::Stay);
    assert!(pane.prefs.static_marks);

    for width in 1..=3 {
        let rows = frame(&mut pane, width, 1);
        assert!(rows[0].contains('M'), "the narrow Motion label stays identifiable: {rows:?}");
    }
}

#[test]
fn narrow_settings_shows_short_choices_and_wraps_only_the_selected_detail() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("drawing does not call Rex"));
    pane.key(Key::Char('2'));
    let rows = frame(&mut pane, 18, 8);
    assert!(rows[2].contains("Icons") && rows[2].contains("Plain"));
    assert!(rows[3].contains("Motion") && rows[3].contains("Pulse"));
    assert!(rows[4].contains("Git marks") && rows[4].contains("Style"));
    assert!(rows[5].contains("Usage") && rows[5].contains("Auto"));
    assert_eq!(rows[6].trim(), "Icons: Plain");
    assert_eq!(rows[7].trim(), "save ? quit");

    for _ in 0..3 {
        assert_eq!(pane.key(Key::Down), After::Stay);
    }
    let rows = frame(&mut pane, 18, 8);
    let selected = rows.iter().position(|line| line.starts_with('›')).unwrap();
    assert!(rows[selected].contains("Usage") && rows[selected].contains("Auto"));
    assert_eq!(rows[5].trim(), "Usage display:");
    assert_eq!(rows[6].trim(), "Auto compact");
    assert_eq!(pane.key(click_at(&rows, 5, "Usage display:")), After::Ignored, "the summary is not a mouse target");
    assert_eq!(pane.key(click_at(&rows, selected, "Auto")), After::Stay);
    let rows = frame(&mut pane, 18, 8);
    assert!(rows[0].contains("Unsaved changes"));
    assert_eq!(rows[6].trim(), "Large figures");

    pane.key(Key::Char('3'));
    pane.claude = Claude::Foreign;
    pane.selected = pane.rows().iter().position(|&row| row == Row::ClaudeLimits).unwrap();
    let rows = frame(&mut pane, 18, 8);
    assert!(rows.iter().any(|row| row.starts_with('›') && row.contains("Claude") && row.contains("Locked")));
    assert_eq!(rows[5].trim(), "Show Claude Code");
    assert_eq!(rows[6].trim(), "limits: Unchanged");
    for height in [2, 3] {
        let rows = frame(&mut pane, 18, height);
        assert!(rows[..height - 1].iter().any(|row| row.starts_with('›') && row.contains("Claude")));
        assert!(rows[height - 1].contains("quit"));
    }
}

#[test]
fn every_settings_control_fits_a_narrow_row_when_selected() {
    let f = Fixture::folder();
    let mut pane =
        Setup::new(Some(store(&f)), true, None, Some(f.root.clone()), |_| panic!("drawing does not call Rex"));
    pane.scope = Scope::Worktree;
    pane.layout.geometry.shares = Some([540_000, 260_000, 200_000]);
    pane.prefs.icons = IconMode::Nerd;
    pane.prefs.usage_large_figures = true;
    pane.prefs.project_mode = ProjectMode::Pin;
    pane.prefs.text_git_marks = true;
    pane.prefs.static_marks = true;
    pane.prefs.changes_only = true;
    let panels = [
        Panel::Layout,
        Panel::Tune,
        Panel::Appearance,
        Panel::Workflow,
        Panel::Alerts,
        Panel::Health,
        Panel::Save(SaveKind::Layout),
        Panel::Save(SaveKind::Workflow),
        Panel::Confirm(Confirm::UseGlobal),
        Panel::Confirm(Confirm::Repair),
        Panel::Confirm(Confirm::Remove),
        Panel::Confirm(Confirm::RemoveMenuBar),
        Panel::Confirm(Confirm::MirrorAlerts),
        Panel::Presence,
        Panel::Confirm(Confirm::Presence),
        Panel::Confirm(Confirm::PresenceDetails(presence::Details::Both)),
        Panel::Confirm(Confirm::PresenceApp),
        Panel::Exit,
    ];
    for panel in panels {
        pane.select_panel(panel, None);
        for selected in 0..pane.rows().len() {
            pane.selected = selected;
            let rows = frame(&mut pane, 18, 8);
            let row = rows.iter().find(|line| line.starts_with('›')).unwrap_or_else(|| panic!("{panel:?}: {rows:?}"));
            assert!(!row.contains('…'), "{panel:?} row {selected} was clipped: {row:?}");
        }
    }
}

#[test]
fn the_save_shortcut_opens_a_scope_chooser_without_writing() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| true);
    assert_eq!(pane.key(Key::Up), After::Stay);
    assert!(pane.selected_row() == Some(Row::Tree));
    assert_eq!(pane.key(Key::Char('s')), After::Stay);
    let rows = frame(&mut pane, 80, 18);
    assert!(rows.iter().any(|row| row.contains("On save:")));
    assert!(rows.iter().any(|row| row.contains("As global default")));
    assert!(!rows.iter().any(|row| row.contains("For this worktree")));
    assert_eq!(pane.key(Key::Enter), After::Stay); // Back is the initial choice.
    assert_eq!(pane.panel, Panel::Layout);
    assert!(pane.selected_row() == Some(Row::Tree));
    assert!(!f.root.join("init.lua").exists());
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveGlobal), After::Stay);
    assert_eq!(pane.panel, Panel::Layout);
    assert!(pane.selected_row() == Some(Row::Tree));
    assert!(pane.installed);
    assert!(f.root.join("init.lua").exists());
}

#[test]
fn saving_only_auto_arrange_globally_preserves_both_layouts() {
    let f = Fixture::folder();
    let store = store(&f);
    let root = f.root.join("worktree");
    let global = Layout::parse_rail("v1 r .321 .6 14 uho 111 300000 420000 280000").unwrap();
    let worktree = Layout::parse_rail("v1 l .231 .8 6 hou 011 0 600000 400000").unwrap();
    store.save_for(&global, Scope::Global, None, None, &|_| true).unwrap();
    store.save_for(&worktree, Scope::Worktree, Some(&root), None, &|_| true).unwrap();
    let global_profile = store.config.join("layouts/global");
    let before = fs::read(&global_profile).unwrap();

    let mut pane = Setup::new(Some(store), true, None, Some(root.clone()), |_| true);
    assert_eq!(pane.scope, Scope::Worktree);
    assert!(pane.layout == worktree);
    choose(&mut pane, Row::Tune);
    choose(&mut pane, Row::AutoArrange);
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveGlobal), After::Stay);

    let store = pane.store().unwrap();
    assert_eq!(fs::read(global_profile).unwrap(), before);
    for (root, layout, scope) in [(None, global, Scope::Global), (Some(root.as_path()), worktree, Scope::Worktree)] {
        let (saved, _, saved_scope) = store.layout_for(root).unwrap();
        assert_eq!(saved.rail_code(), layout.rail_code());
        assert!(saved.new_tabs);
        assert_eq!(saved_scope, scope);
    }
    assert!(!pane.layout_dirty());
}

#[test]
fn worktree_scope_and_inheritance_only_change_after_explicit_confirmation() {
    let f = Fixture::new();
    let mut pane = Setup::new(Some(store(&f)), true, None, Some(f.root.clone()), |_| true);
    choose(&mut pane, Row::Tree);
    choose(&mut pane, Row::Tune);
    choose(&mut pane, Row::AutoArrange);
    pane.key(Key::Char('s'));
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert_eq!(pane.panel, Panel::Tune);
    assert!(pane.selected_row() == Some(Row::AutoArrange));
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveWorktree), After::Stay);
    assert_eq!(pane.panel, Panel::Tune);
    assert!(pane.selected_row() == Some(Row::AutoArrange));
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert_eq!(pane.scope, Scope::Worktree);
    assert_eq!(pane.saved.as_ref().unwrap().panes, [true, false, false]);
    assert!(!pane.saved.as_ref().unwrap().new_tabs, "auto-arrange is global");
    assert!(pane.layout.new_tabs, "unsaved auto-arrange choice is not lost");
    assert!(frame(&mut pane, 77, 16).iter().any(|row| row.contains("This worktree")));
    assert_eq!(choose(&mut pane, Row::UseGlobal), After::Stay);
    assert_eq!(pane.key(Key::Enter), After::Stay);
    assert_eq!(pane.scope, Scope::Worktree);
    assert!(pane.selected_row() == Some(Row::UseGlobal));
    choose(&mut pane, Row::UseGlobal);
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert_eq!(pane.scope, Scope::Global);
    assert_eq!(pane.panel, Panel::Layout);
    assert!(pane.selected_row() == Some(Row::Tune));
    assert_eq!(pane.layout.panes, [true, true, true]);
    assert!(pane.layout.new_tabs, "inheriting the pane arrangement does not discard the global draft switch");
}

#[test]
fn stale_terminal_layout_inheritance_keeps_the_newer_worktree_profile() {
    let f = Fixture::folder();
    let store = store(&f);
    let root = f.root.join("checkout");
    let run = |_: &[&OsStr]| true;
    let tree = layout_preset(true, false);
    store.save_for(&tree, Scope::Worktree, Some(&root), None, &run).unwrap();
    let mut pane = Setup::new(Some(store.clone()), true, None, Some(root.clone()), run);
    assert!(pane.layout == tree);

    let sidebar = layout_preset(false, false);
    store.save_for(&sidebar, Scope::Worktree, Some(&root), None, &run).unwrap();
    assert_eq!(choose(&mut pane, Row::UseGlobal), After::Stay);
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert!(pane.message.as_ref().unwrap().0.starts_with("The saved Milo layout changed."));
    let (saved, _, scope) = store.layout_for(Some(&root)).unwrap();
    assert!(saved == sidebar);
    assert_eq!(scope, Scope::Worktree);
}

#[test]
fn preference_prompts_return_to_the_clicked_control_in_each_section() {
    let Some(root) = isolated_settings("setup::tests::preference_prompts_return_to_the_clicked_control_in_each_section")
    else {
        return;
    };
    let worktree = root.join("worktree");
    let mut pane = Setup::new(Store::real(), false, None, Some(worktree.clone()), |_| panic!("outside Rex"));
    const SAVE: &str = "Save pane preferences for";
    const INHERIT: &str = "Use global pane preferences";
    for (tab, section, change) in
        [('2', Panel::Appearance, "Activity marks"), ('3', Panel::Workflow, "Follow new commits")]
    {
        pane.key(Key::Char(tab));
        let rows = frame(&mut pane, 80, 24);
        assert_eq!(pane.key(click_label(&rows, change)), After::Stay);
        let rows = frame(&mut pane, 80, 24);
        assert_eq!(pane.key(click_label(&rows, SAVE)), After::Stay);
        assert_eq!(pane.key(Key::Esc), After::Stay);
        assert_eq!(pane.panel, section);
        assert!(pane.selected_row() == Some(Row::SavePrefs));
        let rows = frame(&mut pane, 80, 24);
        assert!(rows.iter().any(|row| row.starts_with('›') && row.contains(SAVE)), "{rows:?}");
        assert_eq!(pane.key(Key::Enter), After::Stay);
        assert_eq!(choose(&mut pane, Row::SaveWorktree), After::Stay);
        assert_eq!(pane.panel, section);
        assert!(pane.selected_row() == Some(Row::SavePrefs));
        assert_eq!(pane.prefs_scope(), Source::Worktree);

        let rows = frame(&mut pane, 80, 24);
        assert_eq!(pane.key(click_label(&rows, INHERIT)), After::Stay);
        assert_eq!(pane.key(Key::Enter), After::Stay);
        assert_eq!(pane.panel, section);
        assert!(pane.selected_row() == Some(Row::UseGlobalPrefs));
        let rows = frame(&mut pane, 80, 24);
        assert!(rows.iter().any(|row| row.starts_with('›') && row.contains(INHERIT)), "{rows:?}");
        assert_eq!(pane.key(Key::Enter), After::Stay);
        assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
        assert_eq!(pane.panel, section);
        assert!(pane.selected_row() == Some(Row::UseGlobalPrefs));
        assert_eq!(pane.prefs_scope(), Source::Default);
    }
}

#[test]
fn workflow_and_claude_settings_remain_available_outside_rex() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("outside Rex"));
    assert_eq!(pane.panel, Panel::Workflow);
    let rows = frame(&mut pane, 75, 23);
    assert!(rows.iter().any(|row| row.contains("Show Claude Code limits")));
    assert!(rows.iter().any(|row| row.contains("Keep the starting project open")));
    assert!(!rows.iter().any(|row| row.contains("Arrange new shell tabs")));
    pane.key(Key::Char('1'));
    let rows = frame(&mut pane, 28, 10);
    let words = rows.iter().map(|row| row.trim()).collect::<Vec<_>>().join(" ");
    assert!(words.contains("Open setup in Rex to arrange tabs."));
    for tool in ["milo outline", "milo history", "milo usage"] {
        assert!(words.contains(tool));
    }
    pane.key(Key::Char('3'));
    assert_eq!(choose(&mut pane, Row::ClaudeLimits), After::Stay);
    assert_eq!(choose(&mut pane, Row::SaveClaude), After::Stay);
    assert!(store(&f).claude_files().unwrap().launcher_matches());
    assert!(!f.root.join("init.lua").exists());
}

#[test]
fn workflow_shows_install_or_separate_open_and_remove_actions() {
    let mut pane = Setup::new(None, false, None, None, |_| panic!("drawing does not call Rex"));
    let rows = frame(&mut pane, 80, 20);
    assert_eq!(pane.visible.len, 11);
    assert!(!rows.iter().any(|row| row.contains("menu bar app")));
    for (installed, action, label, short) in [
        (false, Row::MenuBar, "Install Milo menu bar app", "Install app"),
        (true, Row::MenuBar, "Open or update Milo menu bar app", "Open/update app"),
        (true, Row::RemoveMenuBar, "Remove Milo menu bar app…", "Remove app…"),
    ] {
        pane.menu_installed = Some(installed);
        pane.select_panel(Panel::Workflow, Some(action));
        let rows = frame(&mut pane, 80, 20);
        assert_eq!(pane.visible.len, if installed { 13 } else { 12 });
        assert_eq!(rows.iter().any(|row| row.contains("Install Milo menu bar app")), !installed);
        assert_eq!(rows.iter().any(|row| row.contains("Open or update Milo menu bar app")), installed);
        assert_eq!(rows.iter().any(|row| row.contains("Remove Milo menu bar app…")), installed);
        assert!(rows.iter().any(|row| row.starts_with('›') && row.contains(label)));
        let rows = frame(&mut pane, 18, 1);
        let row = rows.iter().find(|row| row.starts_with('›')).unwrap();
        assert_eq!(row.trim_start_matches('›').trim_end_matches('▐').trim(), short);
    }
}

#[test]
fn workflow_opens_alerts_and_presence_independently() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("drawing does not call Rex"));
    pane.menu_installed = Some(true);
    for (label, panel, row) in [
        ("Mirror Rex alerts…", Panel::Alerts, Row::Alerts),
        ("Discord presence…", Panel::Presence, Row::PresenceOpen),
    ] {
        let rows = frame(&mut pane, 80, 20);
        assert_eq!(pane.key(click_label(&rows, label)), After::Stay);
        assert_eq!(pane.panel, panel);
        let rows = frame(&mut pane, 80, 20);
        assert_eq!(pane.key(click_at(&rows, 19, "Esc back")), After::Stay);
        assert_eq!(pane.panel, Panel::Workflow);
        assert!(pane.selected_row() == Some(row));
        assert_eq!(pane.key(Key::Enter), After::Stay);
        assert_eq!(pane.key(Key::Ctrl(b'i')), After::Stay);
        assert_eq!(pane.panel, Panel::Health);
        assert_eq!(pane.key(Key::Char('3')), After::Stay);
    }
}

#[test]
fn menu_bar_app_actions_keep_drafts_and_only_removal_needs_confirmation() {
    let Some(home) =
        isolated_settings("setup::tests::menu_bar_app_actions_keep_drafts_and_only_removal_needs_confirmation")
    else {
        return;
    };
    let app = home.join("Applications/Milo.app");
    fs::create_dir_all(app.parent().unwrap()).unwrap();
    fs::write(&app, "Not a Milo app").unwrap();
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| panic!("menu bar settings do not call Rex"));
    assert_eq!(choose(&mut pane, Row::Tree), After::Stay);
    assert_eq!(pane.key(Key::Char('3')), After::Stay);
    assert_eq!(choose(&mut pane, Row::TabTitles), After::Stay);
    assert_eq!(choose(&mut pane, Row::ClaudeLimits), After::Stay);
    assert!(pane.layout_dirty() && pane.prefs_dirty() && pane.claude_dirty());
    let layout = pane.layout.clone();
    let prefs = pane.prefs;
    let show_limits = pane.show_limits;

    pane.menu_installed = Some(false);
    assert_eq!(choose(&mut pane, Row::MenuBar), After::Stay);
    assert_eq!(pane.panel, Panel::Workflow);
    assert!(pane.message.as_ref().is_some_and(|(_, style)| *style == DEL.bold()));
    assert!(!pane.rows().contains(&Row::MenuBar), "refresh hides an unmanageable app");

    pane.menu_installed = Some(true);
    pane.select_panel(Panel::Workflow, Some(Row::MenuBar));
    let rows = frame(&mut pane, 18, 1);
    assert_eq!(pane.key(click_label(&rows, "Open/update app")), After::Stay);
    assert_eq!(pane.panel, Panel::Workflow, "opening or updating never prompts for removal");
    assert!(pane.message.as_ref().is_some_and(|(_, style)| *style == DEL.bold()));
    assert!(!pane.rows().contains(&Row::MenuBar), "opening uses the same verified installer");

    pane.menu_installed = Some(true);

    let rows = frame(&mut pane, 80, 20);
    let remove = click_label(&rows, "Remove Milo menu bar app…");
    assert_eq!(pane.key(remove), After::Stay);
    assert_eq!(pane.panel, Panel::Confirm(Confirm::RemoveMenuBar));
    assert!(pane.selected_row() == Some(Row::Cancel));
    assert_eq!(pane.key(remove), After::Ignored);
    let rows = frame(&mut pane, 80, 20);
    assert!(rows.iter().any(|row| row.contains("The Milo CLI and pane settings stay")));
    assert_eq!(pane.key(Key::Enter), After::Stay);
    assert_eq!(pane.panel, Panel::Workflow);
    assert!(pane.selected_row() == Some(Row::RemoveMenuBar));
    assert!(pane.message.is_none());

    assert_eq!(pane.key(Key::Enter), After::Stay);
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert_eq!(pane.panel, Panel::Workflow);
    let (message, style) = pane.message.as_ref().unwrap();
    assert!(message.starts_with("Milo:"), "{message}");
    assert!(*style == DEL.bold());
    assert!(!pane.rows().contains(&Row::MenuBar));
    assert!(!pane.rows().contains(&Row::RemoveMenuBar));
    assert_eq!(fs::read_to_string(&app).unwrap(), "Not a Milo app");
    assert!(pane.layout == layout && pane.prefs == prefs && pane.show_limits == show_limits);
    assert!(pane.layout_dirty() && pane.prefs_dirty() && pane.claude_dirty());
}

#[test]
fn a_foreign_status_line_is_explained_and_never_changed() {
    let f = Fixture::folder();
    let paths = store(&f);
    let original = r#"{"statusLine":{"type":"command","command":"another-program"}}"#;
    fs::write(&paths.claude, original).unwrap();
    let mut pane = Setup::new(Some(paths), false, None, None, |_| panic!("outside Rex"));
    assert_eq!(choose(&mut pane, Row::ClaudeLimits), After::Ignored);
    let rows = frame(&mut pane, 78, 26);
    assert!(rows.iter().any(|row| row.contains("Milo will leave your Claude Code status line alone")));
    assert_eq!(choose(&mut pane, Row::SaveClaude), After::Ignored);
    assert_eq!(fs::read_to_string(f.root.join("settings.json")).unwrap(), original);
}

#[test]
fn appearance_changes_its_live_sample_without_global_writes() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("appearance never calls Rex"));
    pane.key(Key::Char('2'));
    let before = frame(&mut pane, 70, 22);
    assert!(before.iter().any(|line| line.contains("●") && line.contains("main.rs")));
    assert!(before.iter().any(|line| line.contains("Claude 5h · 12% left")));
    assert_eq!(pane.key(click_label(&before, "Activity marks")), After::Stay);
    let rows = frame(&mut pane, 70, 22);
    assert_eq!(pane.key(click_label(&rows, "Usage display")), After::Stay);
    pane.icon_override = Some(IconMode::Off);
    let after = frame(&mut pane, 70, 22);
    assert!(after.iter().any(|line| line.contains("Sample (MILO_ICONS override)")));
    assert!(after.iter().any(|line| line.contains("• main.rs")));
    assert!(after.iter().any(|line| line.contains("12%  Claude 5h")));
    assert!(!f.root.join("config").exists());
}

#[test]
fn settings_show_all_controls_and_selected_help_before_the_sample() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("drawing does not call Rex"));
    for (tab, moves, label, detail, summary, sample) in [
        ('2', 3, "Usage", "Auto uses text in narrow panes", "Auto compact", "Sample"),
        ('3', 2, "Outline", "Set the initial Outline view.", "with: All files", "What panes"),
    ] {
        pane.key(Key::Char(tab));
        for _ in 0..moves {
            assert_eq!(pane.key(Key::Down), After::Stay);
        }
        for (width, height) in [(18, 16), (80, 18), (18, 24), (80, 24)] {
            let rows = frame(&mut pane, width, height);
            assert_eq!(pane.visible.len, pane.rows().len(), "{width}x{height}: {rows:?}");
            assert!(rows.iter().any(|row| row.starts_with('›') && row.contains(label)), "{rows:?}");
            let help_text = if width == 18 { summary } else { detail };
            assert!(rows.iter().any(|row| !row.starts_with('›') && row.contains(help_text)), "{rows:?}");
            assert_eq!(rows.iter().any(|row| row.contains(sample)), height == 24, "{rows:?}");
            assert_eq!(pane.key(click_at(&rows, height - 1, "?")), After::Help);
        }
    }
}

#[test]
fn no_target_cannot_try_and_health_removal_is_not_the_default_action() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| panic!("no Rex target"));
    assert_eq!(pane.key(Key::Char('a')), After::Stay);
    assert!(frame(&mut pane, 60, 18).iter().any(|line| line.contains("Could not find this Rex pane")));
    assert_eq!(pane.key(Key::Char('4')), After::Stay);
    let rows = frame(&mut pane, 80, 18);
    let remove = click_label(&rows, "Remove Milo integration…");
    assert_eq!(pane.key(remove), After::Stay);
    assert_eq!(pane.panel, Panel::Confirm(Confirm::Remove));
    assert_eq!(pane.key(remove), After::Ignored);
    assert_eq!(pane.key(Key::Enter), After::Stay);
    assert_eq!(pane.panel, Panel::Health);
    assert!(!f.root.join("init.lua").exists());
}

#[test]
fn integration_maintenance_cannot_discard_unsaved_layout_or_claude_choices() {
    let Some(_) =
        isolated_settings("setup::tests::integration_maintenance_cannot_discard_unsaved_layout_or_claude_choices")
    else {
        return;
    };
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| panic!("draft changes do not call Rex"));
    choose(&mut pane, Row::Tree);
    pane.key(Key::Char('3'));
    choose(&mut pane, Row::ClaudeLimits);
    pane.key(Key::Char('4'));
    for row in [Row::Repair, Row::Remove] {
        assert_eq!(choose(&mut pane, row), After::Stay);
        assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
        assert_eq!(pane.message.as_ref().unwrap().0, MAINTENANCE_DRAFT);
        assert!(pane.layout_dirty() && pane.claude_dirty());
        assert_eq!(pane.key(Key::Esc), After::Stay);
    }
}

#[test]
fn a_narrow_notice_does_not_hide_the_control_or_exit() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| false);
    pane.key(Key::Char('a'));
    let rows = frame(&mut pane, 10, 5);
    assert!(rows.iter().any(|row| row.starts_with('›')));
    assert_eq!(rows[4].trim(), "s a ? q");
}

#[test]
fn clicking_a_setting_changes_the_draft_and_ignores_padding_disabled_and_stale_hits() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| panic!("draft changes do not call Rex"));
    let rows = frame(&mut pane, 110, 27);
    let tree = click_label(&rows, "Just the tree");
    let Key::Click(x, y) = tree else { unreachable!() };
    assert_eq!(pane.key(Key::Click(x + "Just the tree".len(), y)), After::Ignored);
    assert_eq!(pane.key(click_label(&rows, "Use this tab’s layout")), After::Ignored);
    assert_eq!(pane.key(tree), After::Stay);
    assert_eq!(pane.layout.panes, [true, false, false]);
    let tune = click_label(&rows, "Customize panes");
    assert_eq!(pane.key(tune), After::Ignored, "a click must refer to a freshly drawn frame");
    let rows = frame(&mut pane, 110, 27);
    assert_eq!(pane.key(click_label(&rows, "Customize panes")), After::Stay);
    assert_eq!(pane.panel, Panel::Tune);
    let rows = frame(&mut pane, 110, 27);
    let side = rows.iter().position(|line| line.contains("Rail side")).unwrap();
    assert_eq!(pane.key(click_at(&rows, side, "Left")), After::Stay);
    assert_eq!(pane.layout.geometry.side, Side::Right);
    let rows = frame(&mut pane, 110, 27);
    assert_eq!(pane.key(click_at(&rows, 0, "Appearance")), After::Stay);
    assert_eq!(pane.panel, Panel::Appearance);
    let rows = frame(&mut pane, 110, 27);
    let activity = click_label(&rows, "Activity marks");
    frame(&mut pane, 8, 1);
    assert_eq!(pane.key(activity), After::Ignored, "a clipped redraw clears previous mouse targets");
    assert!(!f.root.join("init.lua").exists());
}

#[test]
fn hovering_setup_rows_only_matches_rendered_enabled_parts() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| panic!("hover never calls Rex"));
    pane.selected = 0;
    assert!(pane.tracks_hover());
    let rows = frame(&mut pane, 110, 27);
    let Key::Click(x, y) = click_label(&rows, "Just the tree") else { unreachable!() };
    let end = x + "Just the tree".len();
    assert_eq!(pane.hover_span(end - 1, y), Some(x..end));
    assert_eq!(pane.hover_span(end, y), None);
    assert_eq!(pane.hover_span(0, y), Some(0..1));
    assert_eq!(pane.hover_span(1, y), None);
    assert_eq!(pane.selected, 0);
    assert_eq!(pane.key(Key::Down), After::Stay);
    assert_eq!(pane.hover_span(x, y), None, "keyboard input makes the previous frame stale");

    assert_eq!(pane.key(Key::Down), After::Stay);
    let rows = frame(&mut pane, 110, 27);
    let Key::Click(x, y) = click_label(&rows, "Use this tab’s layout") else { unreachable!() };
    assert!(rows[y].starts_with('›'));
    assert_eq!(pane.hover_span(x, y), None);
    assert_eq!(pane.hover_span(0, y), None);

    assert_eq!(pane.key(click_label(&rows, "Customize panes")), After::Stay);
    let rows = frame(&mut pane, 110, 27);
    let Key::Click(label, y) = click_label(&rows, "Rail side") else { unreachable!() };
    let Key::Click(value, _) = click_at(&rows, y, "Left") else { unreachable!() };
    assert_eq!(pane.hover_span(label, y), Some(label..label + "Rail side".len()));
    assert_eq!(pane.hover_span(label + "Rail side".len(), y), None);
    assert_eq!(pane.hover_span(value, y), Some(value..value + "Left".len()));
    assert_eq!(pane.layout.geometry.side, Side::Left);
    assert_eq!(pane.key(Key::Click(value, y)), After::Stay);
    assert_eq!(pane.layout.geometry.side, Side::Right);
}

#[test]
fn hovering_setup_tabs_footer_and_save_choices_uses_the_current_frame() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| panic!("hover never calls Rex"));
    let rows = frame(&mut pane, 80, 18);
    let Key::Click(tab, top) = click_at(&rows, 0, "Appearance") else { unreachable!() };
    assert_eq!(pane.hover_span(tab + 2, top), Some(tab..tab + "Appearance".len()));
    assert_eq!(pane.hover_span(tab - 1, top), None);
    let save_label = "s save…";
    let save = click_at(&rows, 17, save_label);
    let Key::Click(x, y) = save else { unreachable!() };
    let end = x + crate::width::str_width(save_label);
    assert_eq!(pane.hover_span(end - 1, y), Some(x..end));
    assert_eq!(pane.hover_span(end, y), None);
    assert_eq!(pane.panel, Panel::Layout);

    assert_eq!(pane.key(save), After::Stay);
    let rows = frame(&mut pane, 80, 18);
    assert_eq!(pane.hover_span(tab, top), None, "the modal hides the tabs");
    let label = "As global default";
    let Key::Click(x, y) = click_label(&rows, label) else { unreachable!() };
    assert_eq!(pane.hover_span(x, y), Some(x..x + label.len()));
    assert_eq!(pane.panel, Panel::Save(SaveKind::Layout));
    assert_eq!(pane.selected, 0, "hovering a save choice does not select it");
    frame(&mut pane, 8, 1);
    assert_eq!(pane.hover_span(x, y), None, "a clipped redraw clears previous targets");
    assert!(!f.root.join("init.lua").exists());
}

#[test]
fn clicked_save_and_inheritance_keep_their_scope_prompts() {
    let f = Fixture::new();
    let mut pane = Setup::new(Some(store(&f)), true, None, Some(f.root.clone()), |_| true);
    let rows = frame(&mut pane, 80, 18);
    assert_eq!(pane.key(click_label(&rows, "s save")), After::Stay);
    assert_eq!(pane.panel, Panel::Save(SaveKind::Layout));
    assert_eq!(pane.key(click_at(&rows, 0, "Health")), After::Ignored);
    let rows = frame(&mut pane, 80, 18);
    assert!(!rows[0].contains("Health"));
    assert_eq!(pane.key(click_at(&rows, 17, "Esc back")), After::Stay);
    assert!(!f.root.join("init.lua").exists());

    let rows = frame(&mut pane, 80, 18);
    assert_eq!(pane.key(click_label(&rows, "s save")), After::Stay);
    let rows = frame(&mut pane, 80, 18);
    assert_eq!(pane.key(click_label(&rows, "For this worktree")), After::Stay);
    assert_eq!(pane.scope, Scope::Worktree);

    let rows = frame(&mut pane, 80, 18);
    let inherit = click_label(&rows, "Use global default");
    assert_eq!(pane.key(inherit), After::Stay);
    assert_eq!(pane.panel, Panel::Confirm(Confirm::UseGlobal));
    assert_eq!(pane.key(inherit), After::Ignored);
    assert_eq!(pane.scope, Scope::Worktree);
    let rows = frame(&mut pane, 80, 18);
    let remove = click_label(&rows, "Remove override");
    let Key::Click(x, y) = remove else { unreachable!() };
    // A newly drawn confirming action may land at the opening click.
    pane.modal_click.opened(x, y);
    assert_eq!(pane.key(remove), After::Ignored);
    assert_eq!(pane.scope, Scope::Worktree);
    pane.modal_click.clear();
    assert_eq!(pane.key(remove), After::Stay);
    assert_eq!(pane.scope, Scope::Global);
}

#[test]
fn diagnostics_refresh_scroll_and_return_by_click() {
    let mut pane = Setup::new(None, false, None, None, |_| panic!("outside Rex"));
    let rows = frame(&mut pane, 75, 8);
    assert_eq!(pane.key(click_at(&rows, 0, "Health")), After::Stay);
    let rows = frame(&mut pane, 75, 8);
    assert_eq!(pane.key(click_label(&rows, "Read diagnostics")), After::Stay);
    assert_eq!(pane.panel, Panel::Diagnostics);
    let rows = frame(&mut pane, 75, 8);
    assert!(rows.iter().any(|row| row.contains(doctor::CHECKING)));
    doctor::wait_ready(pane.external_fd());
    assert!(matches!(pane.check_external(), External::Redraw));
    assert!(pane.health.contains("Could not find your settings folders"));

    pane.health = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\nLine 6\nLine 7\nLine 8\nLine 9".into();
    let rows = frame(&mut pane, 75, 8);
    assert_eq!(pane.key(click_at(&rows, 7, "↓ down")), After::Stay);
    assert_eq!(pane.health_top, 1);
    assert_eq!(pane.key(Key::End), After::Stay);
    assert_eq!(pane.health_top, pane.visible.scroll_limit);
    assert_eq!(pane.key(Key::Down), After::Ignored);
    let rows = frame(&mut pane, 75, 8);
    assert_eq!(pane.key(click_at(&rows, 7, "r refresh")), After::Stay);
    assert_eq!(pane.health_top, 0);
    assert!(pane.diagnostics.checking());
    let rows = frame(&mut pane, 75, 8);
    assert!(rows.iter().any(|row| row.contains(doctor::CHECKING)));
    assert_eq!(pane.key(click_at(&rows, 7, "Esc back")), After::Stay);
    assert_eq!(pane.panel, Panel::Health);
    doctor::wait_ready(pane.external_fd());
    assert!(matches!(pane.check_external(), External::Unchanged));
    assert!(pane.health.ends_with("Line 9"));
}

#[test]
fn home_doctor_and_settings_share_the_diagnostic_worker() {
    let Some(_) = isolated_settings("setup::tests::home_doctor_and_settings_share_the_diagnostic_worker") else {
        return;
    };
    let mut home = crate::home::Home::new();
    let rows = frame(&mut home, 64, 12);
    assert_eq!(home.key(click_label(&rows, "Doctor")), After::Stay);
    assert!(frame(&mut home, 64, 12).iter().any(|row| row.contains(doctor::CHECKING)));
    let abandoned = home.external_fd();
    assert!(abandoned.is_some());
    assert_eq!(home.key(Key::Esc), After::Stay);
    let rows = frame(&mut home, 64, 12);
    assert_eq!(home.key(click_label(&rows, "Terminal settings")), After::Stay);
    assert_eq!(home.external_fd(), abandoned);

    assert_eq!(home.key(Key::Char('4')), After::Stay);
    let rows = frame(&mut home, 64, 12);
    assert_eq!(home.key(click_label(&rows, "Read diagnostics")), After::Stay);
    assert!(frame(&mut home, 64, 12).iter().any(|row| row.contains(doctor::CHECKING)));
    assert_eq!(home.external_fd(), abandoned, "wait for the old worker instead of spawning another");
    doctor::wait_ready(home.external_fd());
    assert!(matches!(home.check_external(), External::Unchanged));
    doctor::wait_ready(home.external_fd());
    assert!(matches!(home.check_external(), External::Redraw));
    assert!(frame(&mut home, 64, 12).iter().any(|row| row.contains("Worktree: Git could not be run.")));

    assert_eq!(home.key(Key::Char('r')), After::Stay);
    let abandoned = home.external_fd();
    assert!(abandoned.is_some());
    assert_eq!(home.key(Key::Esc), After::Stay);
    assert_eq!(home.key(Key::Esc), After::Stay);
    let rows = frame(&mut home, 64, 12);
    assert_eq!(home.key(click_label(&rows, "Doctor")), After::Stay);
    assert_eq!(home.external_fd(), abandoned);
    doctor::wait_ready(home.external_fd());
    assert!(matches!(home.check_external(), External::Unchanged));
    doctor::wait_ready(home.external_fd());
    assert!(matches!(home.check_external(), External::Redraw));
    assert!(frame(&mut home, 64, 12).iter().any(|row| row.contains("Worktree: Git could not be run.")));
}

#[test]
fn narrow_rails_keep_tabs_scrolled_rows_and_footer_actions_mouse_accessible() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), true, None, None, |_| panic!("no Rex target"));
    let rows = frame(&mut pane, 10, 5);
    assert_eq!(pane.key(click_at(&rows, 1, "A")), After::Stay);
    assert_eq!(pane.panel, Panel::Appearance);
    let rows = frame(&mut pane, 10, 5);
    assert_eq!(pane.key(click_at(&rows, 1, "L")), After::Stay);

    // The first row is selected when entering the Layout section; a one-row
    // view must not skip any controls when using the wheel.
    for label in ["Project sidebar", "Use this tab’s layout", "Customize panes"] {
        frame(&mut pane, 28, 3);
        assert_eq!(pane.key(Key::WheelDown), After::Stay);
        let rows = frame(&mut pane, 28, 3);
        assert!(rows[1].contains(label), "{rows:?}");
    }

    let rows = frame(&mut pane, 10, 5);
    assert_eq!(pane.key(click_at(&rows, 4, "a")), After::Stay);
    assert!(pane.message.as_ref().unwrap().0.contains("Could not find this Rex pane"));
    let rows = frame(&mut pane, 10, 5);
    assert_eq!(pane.key(click_at(&rows, 4, "?")), After::Help);
    let rows = frame(&mut pane, 10, 5);
    assert_eq!(pane.key(click_at(&rows, 4, "s")), After::Stay);
    assert_eq!(pane.panel, Panel::Save(SaveKind::Layout));
    let rows = frame(&mut pane, 10, 5);
    assert_eq!(pane.key(click_at(&rows, 4, "‹")), After::Stay);
    let rows = frame(&mut pane, 18, 8);
    assert_eq!(pane.key(click_at(&rows, 7, "try")), After::Stay);
    assert!(pane.message.as_ref().unwrap().0.contains("Could not find this Rex pane"));
    let rows = frame(&mut pane, 18, 8);
    assert_eq!(pane.key(click_at(&rows, 7, "save")), After::Stay);
    assert_eq!(pane.panel, Panel::Save(SaveKind::Layout));
    let rows = frame(&mut pane, 18, 8);
    assert_eq!(pane.key(click_at(&rows, 7, "back")), After::Stay);
    assert_eq!(pane.panel, Panel::Layout);
    let rows = frame(&mut pane, 10, 5);
    assert_eq!(pane.key(click_at(&rows, 4, "q")), After::Leave);
    assert!(!f.root.join("init.lua").exists());
}

#[test]
fn alerts_are_mirrored_only_after_the_pane_has_said_what_is_sent() {
    const TOKEN: &str = "123456:SYNTHETICsyntheticSYNTHETIC00";
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("alerts do not call Rex"));
    pane.alerts = Some(Alerts::at(&f.root.join("alerts"), &f.root.join("alert-state")));
    let saved = |pane: &Setup<_>| pane.alerts.as_ref().unwrap().settings().unwrap();

    assert_eq!(choose(&mut pane, Row::Alerts), After::Stay);
    assert_eq!(pane.panel, Panel::Alerts);
    let rows = frame(&mut pane, 80, 24).join("\n");
    assert!(rows.contains("Mirror Rex alerts  Off") && rows.contains("None saved"), "{rows}");

    // Turning on is asked about first, and without a destination does nothing.
    choose(&mut pane, Row::AlertsOn);
    assert_eq!(pane.panel, Panel::Confirm(Confirm::MirrorAlerts));
    assert!(pane.selected_row() == Some(Row::Cancel));
    assert!(!saved(&pane).enabled);
    let rows = frame(&mut pane, 80, 24).join("\n");
    assert!(rows.contains("Nothing the") && rows.contains("Turn mirroring on"), "{rows}");
    choose(&mut pane, Row::Continue);
    assert_eq!(pane.panel, Panel::Alerts);
    assert!(!saved(&pane).enabled);
    assert!(frame(&mut pane, 80, 24).join("\n").contains("Nothing was turned on"));
    choose(&mut pane, Row::AlertsDestination);
    assert!(frame(&mut pane, 80, 24).join("\n").contains("No other destination is saved"));

    // A destination is saved elsewhere; the pane never takes a secret.
    pane.alerts.as_ref().unwrap().set_telegram(TOKEN, "-1001234567890").unwrap();
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert!(pane.panel == Panel::Workflow && pane.selected_row() == Some(Row::Alerts));
    choose(&mut pane, Row::Alerts);
    let rows = frame(&mut pane, 80, 24).join("\n");
    assert!(rows.contains("Telegram") && !rows.contains("SYNTHETIC"), "{rows}");

    choose(&mut pane, Row::AlertKind(alerts::Kind::Bell));
    choose(&mut pane, Row::AlertKind(alerts::Kind::Done));
    assert_eq!(saved(&pane).kinds, [true, true, false, false, true]);
    choose(&mut pane, Row::AlertsContent);
    assert_eq!(saved(&pane).content, alerts::Content::Text);
    choose(&mut pane, Row::AlertsOn);
    assert!(frame(&mut pane, 80, 24).join("\n").contains("300 characters"));
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert!(pane.panel == Panel::Alerts && !saved(&pane).enabled);
    choose(&mut pane, Row::AlertsOn);
    let review = frame(&mut pane, 80, 24).join("\n");
    assert!(review.contains("Telegram chat -1001234567890") && review.contains("Events: blocked, error, bell"));
    pane.alerts.as_ref().unwrap().set_content(alerts::Content::Metadata).unwrap();
    choose(&mut pane, Row::Continue);
    assert!(pane.panel == Panel::Alerts && !saved(&pane).enabled);
    assert!(frame(&mut pane, 80, 24).join("\n").contains("Review them again"));

    choose(&mut pane, Row::AlertsOn);
    choose(&mut pane, Row::Continue);
    assert!(saved(&pane).enabled);
    assert!(frame(&mut pane, 80, 24).join("\n").contains("Rex alert mirroring is on."));

    // Turning off needs no second step.
    choose(&mut pane, Row::AlertsOn);
    assert!(pane.panel == Panel::Alerts && !saved(&pane).enabled);
    // None of this is a draft to save or discard on the way out.
    assert!(!pane.dirty());
}

#[test]
fn choosing_a_legacy_destination_in_settings_does_not_reroute_active_alerts() {
    let f = Fixture::folder();
    let config = f.root.join("alerts");
    let alerts = Alerts::at(&config, &f.root.join("alert-state"));
    alerts.set_telegram("123456:SYNTHETICsyntheticSYNTHETIC00", "42").unwrap();
    alerts.set_discord("https://discord.com/api/webhooks/12345/SYNTHETICsyntheticSYNTHETIC00").unwrap();
    fs::write(config.join("alerts.conf"), "enabled = true\ndestination = telegram\ntelegram_chat = 42\n").unwrap();

    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("alerts do not call Rex"));
    pane.alerts = Some(alerts);
    choose(&mut pane, Row::Alerts);
    choose(&mut pane, Row::AlertsDestination);
    let settings = pane.alerts.as_ref().unwrap().settings().unwrap();
    assert!(settings.active(alerts::Destination::Telegram) && !settings.active(alerts::Destination::Discord));
    assert!(frame(&mut pane, 80, 24).join("\n").contains("Turn mirroring off"));
}

#[test]
fn discord_presence_says_what_becomes_public_before_turning_on_and_turns_off_at_once() {
    let f = Fixture::folder();
    let file = f.root.join("config/discord.conf");
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("presence does not call Rex"));
    assert_eq!(choose(&mut pane, Row::FollowActivity), After::Stay);
    let draft = pane.prefs;
    // The control's own row: its label, a gap, then its value.
    let row = |pane: &mut Setup<_>, label: &str| {
        let start = format!("{label}  ");
        let found = frame(pane, 80, 30).into_iter().find(|row| row.trim_start_matches(['›', ' ']).starts_with(&start));
        found.unwrap_or_else(|| panic!("no {label} row")).trim_end().to_owned()
    };
    let page = |pane: &mut Setup<_>| frame(pane, 80, 30).join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    // Workflow has one row for presence, which says whether it is on and opens its choices.
    assert!(row(&mut pane, "Discord presence…").ends_with("Off"));
    assert_eq!(choose(&mut pane, Row::PresenceOpen), After::Stay);
    assert_eq!(pane.panel, Panel::Presence);
    assert!(row(&mut pane, "Discord presence").ends_with("Off"));
    assert!(row(&mut pane, "Presence shows").ends_with("Generic"));
    assert!(row(&mut pane, "Presence application").ends_with("Milo"));

    assert_eq!(choose(&mut pane, Row::Presence), After::Stay);
    assert_eq!(pane.panel, Panel::Confirm(Confirm::Presence));
    let asked = page(&mut pane);
    for said in [
        "Turn on Discord presence?",
        "Milo's name and icon with the line “In a Rex session”",
        "Anyone who can see your Discord activity sees it",
        "no session, tab, project or file names, no paths and no commands",
    ] {
        assert!(asked.contains(said), "{said:?} is missing from {asked:?}");
    }
    assert!(pane.selected_row() == Some(Row::Cancel), "nothing can be turned on by pressing Enter twice");
    assert_eq!(pane.key(Key::Char('q')), After::Stay);
    assert_eq!(pane.panel, Panel::Presence);
    assert!(pane.selected_row() == Some(Row::Presence));
    assert!(!file.exists(), "reading about it and backing out saves nothing");

    // No application ID is asked for: Milo's own is used.
    assert_eq!(choose(&mut pane, Row::Presence), After::Stay);
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert_eq!(pane.panel, Panel::Presence);
    assert_eq!(fs::read_to_string(&file).unwrap(), "presence = true\n");
    assert!(pane.message.as_ref().is_some_and(|(text, _)| text.starts_with("Discord presence is on.")));
    assert!(row(&mut pane, "Discord presence").ends_with("On"));

    assert_eq!(choose(&mut pane, Row::Presence), After::Stay);
    assert_eq!(pane.panel, Panel::Presence, "turning it off asks nothing");
    assert_eq!(fs::read_to_string(&file).unwrap(), "presence = false\n");
    assert!(pane.message.as_ref().is_some_and(|(text, _)| text.starts_with("Discord presence is off.")));
    assert!(pane.prefs == draft && pane.prefs_dirty(), "the pane preference draft is neither saved nor lost");

    // A choice Milo cannot read is left alone and cannot be changed from here.
    fs::write(&file, "presence = maybe\n").unwrap();
    for row in [Row::Presence, Row::PresenceDetails, Row::PresenceApp] {
        pane.select_panel(Panel::Presence, Some(row));
        assert_eq!(pane.key(Key::Enter), After::Ignored);
    }
    pane.select_panel(Panel::Presence, Some(Row::Presence));
    assert!(page(&mut pane).contains("Could not read the saved Discord presence choice."));
    assert_eq!(fs::read_to_string(&file).unwrap(), "presence = maybe\n");
}

#[test]
fn discord_presence_names_are_asked_about_only_when_they_would_show_more() {
    let f = Fixture::folder();
    let file = f.root.join("config/discord.conf");
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("presence does not call Rex"));
    let page = |pane: &mut Setup<_>| frame(pane, 80, 30).join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    let details = |pane: &Setup<_>| pane.presence_view.as_ref().unwrap().setting.details;

    // While presence is off a choice is only saved, and turning it on then says what that choice shows.
    assert_eq!(choose(&mut pane, Row::PresenceOpen), After::Stay);
    assert_eq!(choose(&mut pane, Row::PresenceDetails), After::Stay);
    assert_eq!(pane.panel, Panel::Presence);
    assert_eq!(fs::read_to_string(&file).unwrap(), "presence = false\ndetails = session\n");
    assert_eq!(choose(&mut pane, Row::Presence), After::Stay);
    let asked = page(&mut pane);
    assert!(asked.contains("the name of the Rex session in front on this computer"), "{asked}");
    assert!(asked.contains("as in “Session: release-notes”"), "{asked}");
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert_eq!(fs::read_to_string(&file).unwrap(), "presence = true\ndetails = session\n");

    // On, a choice that adds a name is not saved until it is confirmed.
    assert_eq!(choose(&mut pane, Row::PresenceDetails), After::Stay);
    assert_eq!(pane.panel, Panel::Confirm(Confirm::PresenceDetails(presence::Details::Project)));
    let asked = page(&mut pane);
    assert!(asked.contains("Show these names on Discord?"), "{asked}");
    assert!(asked.contains("the folder name of the Git project in the focused pane"), "{asked}");
    assert!(asked.contains("never the path, the remote address or a branch"), "{asked}");
    assert!(pane.selected_row() == Some(Row::Cancel));
    assert_eq!(pane.key(Key::Enter), After::Stay);
    assert_eq!(pane.panel, Panel::Presence);
    assert!(pane.selected_row() == Some(Row::PresenceDetails));
    assert_eq!(details(&pane), presence::Details::Session, "backing out keeps what was shown");

    assert_eq!(choose(&mut pane, Row::PresenceDetails), After::Stay);
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert_eq!(fs::read_to_string(&file).unwrap(), "presence = true\ndetails = project\n");
    assert_eq!(choose(&mut pane, Row::PresenceDetails), After::Stay);
    assert_eq!(pane.panel, Panel::Confirm(Confirm::PresenceDetails(presence::Details::Both)));
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert_eq!(details(&pane), presence::Details::Both);

    // Showing less asks nothing and is saved at once.
    assert_eq!(choose(&mut pane, Row::PresenceDetails), After::Stay);
    assert_eq!(pane.panel, Panel::Presence);
    assert_eq!(fs::read_to_string(&file).unwrap(), "presence = true\n");
    assert!(pane.message.as_ref().is_some_and(|(text, _)| text.contains("the fixed line and no names")));
}

#[test]
fn discord_presence_takes_an_application_of_your_own_and_goes_back_to_milos() {
    const ID: &str = "1234567890123456789";
    let f = Fixture::folder();
    let file = f.root.join("config/discord.conf");
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("presence does not call Rex"));
    let page = |pane: &mut Setup<_>| frame(pane, 80, 30).join(" ").split_whitespace().collect::<Vec<_>>().join(" ");

    assert_eq!(choose(&mut pane, Row::PresenceOpen), After::Stay);
    assert_eq!(choose(&mut pane, Row::PresenceApp), After::Stay);
    assert_eq!(pane.panel, Panel::Confirm(Confirm::PresenceApp));
    let asked = page(&mut pane);
    assert!(asked.contains("No bot or token is needed.") && asked.contains("Type your Discord application ID."), "{asked}");
    assert!(pane.selected_row() == Some(Row::PresenceId));
    assert_eq!(choose(&mut pane, Row::Continue), After::Ignored, "an application ID is needed first");
    for digit in ID[..16].chars() {
        assert_eq!(pane.key(Key::Char(digit)), After::Stay);
    }
    assert_eq!(choose(&mut pane, Row::Continue), After::Ignored, "too short to be an application ID");
    assert_eq!(choose(&mut pane, Row::PresenceId), After::Stay, "Enter on the ID clears it");
    for digit in ID.chars() {
        pane.key(Key::Char(digit));
    }
    assert!(frame(&mut pane, 80, 30).iter().any(|row| row.contains(&format!("Application ID: {ID}"))));
    assert!(!file.exists());
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert_eq!(pane.panel, Panel::Presence);
    assert_eq!(fs::read_to_string(&file).unwrap(), format!("presence = false\napplication_id = {ID}\n"));
    let rows = frame(&mut pane, 80, 30);
    assert!(rows.iter().any(|row| row.contains("Presence application") && row.trim_end().ends_with("Your own")));

    // Turning it on now names the person's own application, not Milo's.
    assert_eq!(choose(&mut pane, Row::Presence), After::Stay);
    assert!(page(&mut pane).contains("the name and icon of your own Discord application"));
    assert_eq!(pane.key(Key::Char('q')), After::Stay);

    assert_eq!(choose(&mut pane, Row::PresenceApp), After::Stay);
    assert_eq!(pane.panel, Panel::Presence, "going back to Milo's application asks nothing");
    assert_eq!(fs::read_to_string(&file).unwrap(), "presence = false\n");
    assert!(
        pane.message.as_ref().is_some_and(|(text, _)| text.starts_with("Discord presence uses Milo's application."))
    );
    // Esc leaves the choices for the row that opened them.
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert!(pane.panel == Panel::Workflow && pane.selected_row() == Some(Row::PresenceOpen));
}

#[test]
fn every_setup_help_row_fits_a_28_column_pane() {
    let f = Fixture::folder();
    for inside in [false, true] {
        let pane = Setup::new(Some(store(&f)), inside, None, None, |_| panic!("help does not call Rex"));
        pane.help().assert_rows_fit(26);
    }
}
