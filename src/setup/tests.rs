use super::*;
use crate::fixture::Fixture;
use std::fs;

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
    assert!(rows.iter().any(|row| row.starts_with('›') && row.contains("Auto-arrange")));
    assert!(!rows.iter().any(|row| row.contains('┌')));
    assert!(rows[4].contains("Esc back"));
    assert_eq!(pane.key(Key::Esc), After::Stay);
    assert_eq!(pane.panel, Panel::Layout);
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
    assert!(fs::read_to_string(f.root.join("settings.json")).unwrap().contains("milo claude-status"));
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
    let report = pane.health.clone();
    assert!(report.contains("Could not find your settings folders"));

    pane.health = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\nLine 6\nLine 7\nLine 8\nLine 9".into();
    let rows = frame(&mut pane, 75, 8);
    assert_eq!(pane.key(click_at(&rows, 7, "↓ down")), After::Stay);
    assert_eq!(pane.health_top, 1);
    let rows = frame(&mut pane, 75, 8);
    assert_eq!(pane.key(click_at(&rows, 7, "r refresh")), After::Stay);
    assert_eq!(pane.health_top, 0);
    assert_eq!(pane.health, report);
    let rows = frame(&mut pane, 75, 8);
    assert_eq!(pane.key(click_at(&rows, 7, "Esc back")), After::Stay);
    assert_eq!(pane.panel, Panel::Health);
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
    let rows = frame(&mut pane, 10, 5);
    assert_eq!(pane.key(click_at(&rows, 4, "q")), After::Leave);
    assert!(!f.root.join("init.lua").exists());
}
