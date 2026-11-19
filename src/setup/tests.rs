use super::*;
use crate::fixture::Fixture;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

fn store(fixture: &Fixture) -> Store {
    Store {
        init: fixture.root.join("init.lua"),
        config: fixture.root.join("config"),
        state: fixture.root.join("state"),
        claude: fixture.root.join("settings.json"),
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
    const ROOT: &str = "MILO_TEST_SETTINGS_ROOT";
    if let Some(root) = std::env::var_os(ROOT) {
        return Some(root.into());
    }
    let f = Fixture::folder();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(ROOT, &f.root)
        .env("HOME", &f.root)
        .env("XDG_STATE_HOME", f.root.join("state"))
        .env_remove("REX_SESSION")
        .env_remove("REX_BLOCK")
        .env("XDG_CONFIG_HOME", &f.root)
        .env("PATH", &f.root)
        .current_dir(&f.root)
        .output()
        .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    None
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
    assert_eq!(home.key(Key::Enter), After::Stay);
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
    assert!(menu.iter().any(|row| row.contains("Settings")));
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
    let mut pane = Setup::new(Some(store(&f)), false, None, Some(root.join("worktree")), |_| panic!("outside Rex"));
    pane.reload_prefs().unwrap();
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
fn global_preference_save_does_not_copy_worktree_or_stale_values() {
    let Some(root) = isolated_settings("setup::tests::global_preference_save_does_not_copy_worktree_or_stale_values")
    else {
        return;
    };
    let worktree = root.join("worktree");
    Preferences::set_changes_only(&worktree, true).unwrap();
    let global = root.join("milo/preferences.conf");
    fs::write(&global, "icons = nerd\nchanges_only = false\n").unwrap();

    let mut pane = Setup::new(None, false, None, Some(worktree.clone()), |_| panic!("outside Rex"));
    pane.reload_prefs().unwrap();
    choose(&mut pane, Row::HistoryFollow);
    choose(&mut pane, Row::FollowProject);
    fs::write(&global, "icons = off\nchanges_only = false\nfollow_activity = false\n").unwrap();
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

    let mut pane = Setup::new(None, false, None, Some(worktree.clone()), |_| panic!("outside Rex"));
    pane.reload_prefs().unwrap();
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveWorktree), After::Stay);
    assert!(!config.join("worktrees").exists(), "saving no edits creates no override");

    choose(&mut pane, Row::HistoryFollow);
    Preferences::set_changes_only(&worktree, false).unwrap();
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
    pane.key(Key::Char('q'));
    choose(&mut pane, Row::SaveAndExit);
    let scope = frame(&mut pane, 80, 18);
    assert!(scope.iter().any(|row| row.contains("Auto-arrange needs a global save")));
    assert!(!pane.rows().contains(&Row::SaveWorktree));
    assert!(pane.rows().contains(&Row::SaveGlobal));
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
        Panel::Health,
        Panel::Save(SaveKind::Layout),
        Panel::Save(SaveKind::Workflow),
        Panel::Confirm(Confirm::UseGlobal),
        Panel::Confirm(Confirm::Repair),
        Panel::Confirm(Confirm::Remove),
        Panel::Exit,
    ];
    for panel in panels {
        pane.select_panel(panel, 0);
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
    assert_eq!(pane.key(Key::Char('s')), After::Stay);
    let rows = frame(&mut pane, 80, 18);
    assert!(rows.iter().any(|row| row.contains("On save:")));
    assert!(rows.iter().any(|row| row.contains("As global default")));
    assert!(!rows.iter().any(|row| row.contains("For this worktree")));
    assert_eq!(pane.key(Key::Enter), After::Stay); // Back is the initial choice.
    assert!(!f.root.join("init.lua").exists());
    pane.key(Key::Char('s'));
    assert_eq!(choose(&mut pane, Row::SaveGlobal), After::Stay);
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
    assert_eq!(choose(&mut pane, Row::SaveWorktree), After::Stay);
    assert_eq!(pane.scope, Scope::Worktree);
    assert_eq!(pane.saved.as_ref().unwrap().panes, [true, false, false]);
    assert!(!pane.saved.as_ref().unwrap().new_tabs, "auto-arrange is global");
    assert!(pane.layout.new_tabs, "unsaved auto-arrange choice is not lost");
    assert!(frame(&mut pane, 77, 16).iter().any(|row| row.contains("This worktree")));
    assert_eq!(choose(&mut pane, Row::UseGlobal), After::Stay);
    assert_eq!(pane.key(Key::Enter), After::Stay);
    assert_eq!(pane.scope, Scope::Worktree);
    choose(&mut pane, Row::UseGlobal);
    assert_eq!(choose(&mut pane, Row::Continue), After::Stay);
    assert_eq!(pane.scope, Scope::Global);
    assert_eq!(pane.layout.panes, [true, true, true]);
}

#[test]
fn workflow_and_claude_settings_remain_available_outside_rex() {
    let f = Fixture::folder();
    let mut pane = Setup::new(Some(store(&f)), false, None, None, |_| panic!("outside Rex"));
    assert_eq!(pane.panel, Panel::Workflow);
    let rows = frame(&mut pane, 75, 22);
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
    let before = frame(&mut pane, 70, 20);
    assert!(before.iter().any(|line| line.contains("●") && line.contains("main.rs")));
    assert!(before.iter().any(|line| line.contains("Claude 5h · 12% left")));
    assert_eq!(pane.key(click_label(&before, "Activity marks")), After::Stay);
    let rows = frame(&mut pane, 70, 20);
    assert_eq!(pane.key(click_label(&rows, "Usage display")), After::Stay);
    pane.icon_override = Some(IconMode::Off);
    let after = frame(&mut pane, 70, 20);
    assert!(after.iter().any(|line| line.contains("Sample (MILO_ICONS override)")));
    assert!(after.iter().any(|line| line.contains("• main.rs")));
    assert!(after.iter().any(|line| line.contains("12%  Claude 5h")));
    assert!(!f.root.join("config").exists());
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
    assert_eq!(home.key(click_label(&rows, "Settings")), After::Stay);
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
