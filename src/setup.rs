//! Preview, try, remember and maintain Milo's pane setup.

mod claude;
pub(crate) mod doctor;
mod draw;
mod files;
mod install;
mod layout;
mod lock;
mod preview;
mod profiles;
mod rex_files;
mod settings;
mod welcome;

pub(crate) use draw::wrap;
pub use profiles::{layout_profile, return_from_peek};
pub use welcome::{Welcome, needs_welcome, skip_welcome};

use std::borrow::Cow;
use std::ffi::OsStr;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::help::{self, Page};
use crate::keys::{ConfirmClick, Key};
use crate::pane::{After, Pane};
use crate::preferences::{IconMode, Preferences, ProjectMode, Source};
use crate::screen::{Screen, Style};
use crate::ui::{DEL, FAINT};
use crate::{paths, rex};
use claude::Claude;
use install::{Saved, Store};
use layout::{Layout, Side, TabPane};
use profiles::Scope;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    Layout,
    Tune,
    Appearance,
    Workflow,
    Health,
    Diagnostics,
    Save(SaveKind),
    Confirm(Confirm),
    Exit,
}

impl Panel {
    fn modal(self) -> bool {
        matches!(self, Self::Save(_) | Self::Confirm(_) | Self::Exit)
    }
}

const SECTIONS: [Panel; 4] = [Panel::Layout, Panel::Appearance, Panel::Workflow, Panel::Health];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveKind {
    Layout,
    Appearance,
    Workflow,
}

impl SaveKind {
    fn panel(self) -> Panel {
        match self {
            Self::Layout => Panel::Layout,
            Self::Appearance => Panel::Appearance,
            Self::Workflow => Panel::Workflow,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    UseGlobal,
    UseGlobalPrefs(SaveKind),
    Repair,
    Remove,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    Tree,
    Sidebar,
    Capture,
    Tune,
    Undo,
    SaveLayout,
    UseGlobal,
    Pane(TabPane),
    Side,
    Width,
    Order(usize),
    OutlineShare,
    UsageRows,
    AutoArrange,
    Back,
    Icons,
    Activity,
    GitMarks,
    UsageStyle,
    FollowProject,
    FollowActivity,
    ChangesOnly,
    HistoryFollow,
    ClaudeLimits,
    SaveClaude,
    SavePrefs,
    UseGlobalPrefs,
    Diagnostics,
    Repair,
    Remove,
    SaveGlobal,
    SaveWorktree,
    Cancel,
    Continue,
    SaveAndExit,
    Discard,
}

const LAYOUT: &[Row] = &[Row::Tree, Row::Sidebar, Row::Capture, Row::Tune, Row::UseGlobal];
const TUNE: &[Row] = &[
    Row::Pane(TabPane::Outline),
    Row::Pane(TabPane::History),
    Row::Pane(TabPane::Usage),
    Row::Side,
    Row::Width,
    Row::Order(0),
    Row::Order(1),
    Row::Order(2),
    Row::OutlineShare,
    Row::UsageRows,
    Row::AutoArrange,
    Row::Back,
];
const WORKFLOW: &[Row] = &[
    Row::FollowProject,
    Row::FollowActivity,
    Row::ChangesOnly,
    Row::HistoryFollow,
    Row::SavePrefs,
    Row::UseGlobalPrefs,
    Row::ClaudeLimits,
    Row::SaveClaude,
];

/// The draft is never written when navigating, capturing or trying a layout.
pub struct Setup<R = fn(&[&OsStr]) -> bool> {
    store: Option<Store>,
    inside: bool,
    target: Option<rex::Target>,
    root: Option<PathBuf>,
    run: R,
    layout: Layout,
    saved: Option<Layout>,
    scope: Scope,
    installed: bool,
    trial: Option<(profiles::Trial, Layout)>,
    tab_size: Option<(usize, usize)>,
    prefs: Preferences,
    saved_prefs: Option<Preferences>,
    prefs_scope: Source,
    claude: Claude,
    show_limits: bool,
    panel: Panel,
    selected: usize,
    exit_from: Option<(Panel, usize)>,
    visible: draw::Visible,
    modal_click: ConfirmClick,
    health: String,
    health_top: usize,
    icon_override: Option<IconMode>,
    message: Option<(Cow<'static, str>, Style)>,
    real: bool,
    status_failed: bool,
}

impl Setup {
    pub fn open() -> Self {
        let inside = rex::inside();
        let target = rex::Target::current();
        let store = Store::real();
        // For scoped settings use the caller's shell, never the focused pane.
        let (root, tab_size) = if inside {
            store.as_ref().zip(target.as_ref()).map_or((None, None), |(store, target)| store.tab_context(target))
        } else {
            (crate::preferences::worktree(Path::new(".")), None)
        };
        let mut setup = Self::new(store, inside, target, root, rex::run);
        setup.tab_size = tab_size;
        setup.icon_override = IconMode::environment();
        if let Err(why) = setup.reload_prefs() {
            setup.saved_prefs = None;
            setup.report(Err(why));
        }
        setup.real = true;
        if setup.message.as_ref().is_some_and(|(_, style)| *style == DEL.bold()) {
            setup.failure_status(true);
        }
        setup
    }
}

impl<R: Fn(&[&OsStr]) -> bool> Setup<R> {
    fn new(store: Option<Store>, inside: bool, target: Option<rex::Target>, root: Option<PathBuf>, run: R) -> Self {
        let claude = store.as_ref().map_or(Claude::Invalid, Store::claude);
        let mut setup = Self {
            store,
            inside,
            target,
            root,
            run,
            layout: Layout::default(),
            saved: None,
            scope: Scope::Global,
            installed: false,
            trial: None,
            tab_size: None,
            prefs: Preferences::default(),
            saved_prefs: Some(Preferences::default()),
            prefs_scope: Source::Default,
            claude,
            show_limits: claude == Claude::On,
            panel: if inside { Panel::Layout } else { Panel::Workflow },
            selected: 0,
            exit_from: None,
            visible: draw::Visible::default(),
            modal_click: ConfirmClick::default(),
            health: String::new(),
            health_top: 0,
            icon_override: None,
            message: None,
            real: false,
            status_failed: false,
        };
        if inside {
            match setup.reload_layout() {
                Ok(()) if setup.store.as_ref().is_some_and(Store::rex_reload_pending) => {
                    setup.report(Err("Rex still needs to reload. Open Health to repair the integration."));
                }
                Ok(()) => {}
                Err(why) => {
                    setup.report(Err(why));
                }
            }
            setup.selected = match setup.layout.panes {
                [true, false, false] => 0, // Just the tree
                [true, true, true] => 1,   // Project sidebar
                _ => 3,                    // Customize panes
            };
        }
        setup
    }

    fn reload_layout(&mut self) -> Result<(), &'static str> {
        let (layout, installed, scope) = self.store()?.layout_for(self.root.as_deref())?;
        self.saved = Some(layout.clone());
        self.layout = layout;
        self.installed = installed;
        self.scope = scope;
        Ok(())
    }

    fn store(&self) -> Result<&Store, &'static str> {
        self.store.as_ref().ok_or("Could not find your settings folders.")
    }

    fn target(&self) -> Result<&rex::Target, &'static str> {
        self.target.as_ref().ok_or("Could not find this Rex pane.")
    }

    fn rows(&self) -> &'static [Row] {
        match self.panel {
            Panel::Layout if !self.inside => &[],
            Panel::Layout if self.scope == Scope::Global => &LAYOUT[..4],
            Panel::Layout => LAYOUT,
            Panel::Tune => TUNE,
            Panel::Appearance => {
                &[Row::Icons, Row::Activity, Row::GitMarks, Row::UsageStyle, Row::SavePrefs, Row::UseGlobalPrefs]
            }
            Panel::Workflow => WORKFLOW,
            Panel::Health => &[Row::Diagnostics, Row::Repair, Row::Remove],
            Panel::Diagnostics => &[],
            Panel::Save(kind)
                if self.root.is_some() && !(kind == SaveKind::Layout && self.exiting_with_only_auto_arrange()) =>
            {
                &[Row::Cancel, Row::SaveGlobal, Row::SaveWorktree]
            }
            Panel::Save(_) => &[Row::Cancel, Row::SaveGlobal],
            Panel::Confirm(_) => &[Row::Cancel, Row::Continue],
            Panel::Exit => &[Row::Cancel, Row::SaveAndExit, Row::Discard],
        }
    }

    fn select_panel(&mut self, panel: Panel, selected: usize) {
        self.panel = panel;
        self.selected = selected.min(self.rows().len().saturating_sub(1));
        self.visible = draw::Visible::default();
        self.modal_click.clear();
        self.message = None;
    }

    fn back(&mut self) {
        let (panel, selected) = match self.panel {
            Panel::Tune => (Panel::Layout, 3),
            Panel::Exit => self.exit_from.take().expect("exit prompt has an editor to return to"),
            Panel::Save(_) if self.exit_from.is_some() => (Panel::Exit, 0),
            Panel::Save(kind) => (kind.panel(), if kind == SaveKind::Layout { 6 } else { 4 }),
            Panel::Confirm(Confirm::UseGlobal) => (Panel::Layout, 7),
            Panel::Confirm(Confirm::UseGlobalPrefs(kind)) => (kind.panel(), 5),
            Panel::Confirm(Confirm::Repair) => (Panel::Health, 1),
            Panel::Confirm(Confirm::Remove) => (Panel::Health, 2),
            Panel::Diagnostics => (Panel::Health, 0),
            _ => (Panel::Layout, 0),
        };
        self.select_panel(panel, selected);
    }

    fn next_panel(&mut self) {
        let next = match self.panel {
            Panel::Layout | Panel::Tune => Panel::Appearance,
            Panel::Appearance => Panel::Workflow,
            Panel::Workflow => Panel::Health,
            Panel::Health | Panel::Diagnostics => Panel::Layout,
            Panel::Save(_) | Panel::Confirm(_) | Panel::Exit => return,
        };
        self.select_panel(next, 0);
    }

    fn layout_dirty(&self) -> bool {
        self.saved.as_ref().is_some_and(|saved| *saved != self.layout)
    }

    fn auto_arrange_dirty(&self) -> bool {
        self.saved.as_ref().is_some_and(|saved| saved.new_tabs != self.layout.new_tabs)
    }

    fn exiting_with_only_auto_arrange(&self) -> bool {
        self.exit_from.is_some()
            && self.auto_arrange_dirty()
            && self.saved.as_ref().is_some_and(|saved| same_geometry(saved, &self.layout))
    }

    fn prefs_dirty(&self) -> bool {
        self.saved_prefs.is_some_and(|saved| saved != self.prefs)
    }

    fn claude_dirty(&self) -> bool {
        self.claude.editable() && self.show_limits != (self.claude == Claude::On)
    }

    fn dirty(&self) -> bool {
        self.layout_dirty() || self.prefs_dirty() || self.claude_dirty()
    }

    fn request_exit(&mut self) -> After {
        if !self.dirty() {
            return self.leave();
        }
        self.exit_from = Some((self.panel, self.selected));
        self.select_panel(Panel::Exit, 0);
        After::Stay
    }

    fn leave(&mut self) -> After {
        if self.trial.is_some()
            && let Err(why) = self.undo()
        {
            if let Some((panel, selected)) = self.exit_from.take() {
                self.select_panel(panel, selected);
            }
            self.report(Err(why));
            return After::Stay;
        }
        self.clear_notice();
        After::Leave
    }

    fn save_and_exit(&mut self) -> After {
        let kind = if self.layout_dirty() {
            SaveKind::Layout
        } else if self.prefs_dirty() {
            // Appearance and Workflow share one file and one scope choice.
            SaveKind::Workflow
        } else {
            if self.claude_dirty()
                && let Err(why) = self.save_claude()
            {
                self.select_panel(Panel::Exit, 0);
                self.report(Err(why));
                return After::Stay;
            }
            return self.leave();
        };
        self.select_panel(Panel::Save(kind), 0);
        After::Stay
    }

    fn save_scope(&mut self, kind: SaveKind, scope: Scope) -> After {
        let result = if kind == SaveKind::Layout { self.save_layout(scope) } else { self.save_prefs(scope) };
        if self.exit_from.is_some() {
            if result.is_ok() {
                return self.save_and_exit();
            }
            // Some integration errors are reported after the file was saved.
            // Don't offer that scope again if its draft is already saved.
            let still_dirty = if kind == SaveKind::Layout { self.layout_dirty() } else { self.prefs_dirty() };
            let layout_saved =
                kind == SaveKind::Layout && scope == Scope::Worktree && self.exiting_with_only_auto_arrange();
            if !still_dirty || layout_saved {
                self.back();
            }
        }
        self.report(result);
        After::Stay
    }

    fn save_layout(&mut self, scope: Scope) -> Result<&'static str, &'static str> {
        if self.trial.as_ref().is_some_and(|(trial, _)| trial.temporary()) {
            self.undo()?;
        }
        let auto_changed = self.auto_arrange_dirty();
        let draft_auto = self.layout.new_tabs;
        let global;
        let layout = if scope == Scope::Global && self.exiting_with_only_auto_arrange() && self.scope == Scope::Worktree
        {
            // A worktree layout must not become the global layout just because
            // the remaining draft is the global auto-arrange switch.
            let (mut saved, _, _) = self.store()?.layout_for(None)?;
            saved.new_tabs = draft_auto;
            global = saved;
            &global
        } else {
            &self.layout
        };
        let saved = self.store()?.save_for(layout, scope, self.root.as_deref(), None, &self.run)?;
        self.reload_layout()?;
        if scope == Scope::Worktree && auto_changed {
            self.layout.new_tabs = draft_auto;
        }
        if let Some((trial, tried)) = &self.trial
            && same_geometry(tried, &self.layout)
        {
            trial.keep()?;
            self.trial = None;
        }
        if self.exit_from.is_none() {
            self.back();
        }
        match saved {
            Saved::ListenerDidNotStart => {
                Err("Saved, but the layout listener did not start. Open Health to repair it.")
            }
            Saved::ListenerDidNotStop => Err("Saved, but the layout listener did not stop. Open Health to repair it."),
            Saved::Ready if scope == Scope::Worktree && auto_changed => {
                Ok("Saved this worktree's layout. Save globally to change auto-arrange.")
            }
            Saved::Ready if scope == Scope::Worktree => Ok("Saved this worktree's layout."),
            Saved::Ready if self.scope == Scope::Worktree => {
                Ok("Saved the global default. This worktree still uses its own layout.")
            }
            Saved::Ready => Ok("Saved the global default."),
        }
    }

    fn reload_prefs(&mut self) -> Result<(), &'static str> {
        let (prefs, source) = Preferences::resolve(self.root.as_deref())
            .map_err(|_| "Could not read Milo's pane preferences. They were left alone.")?;
        self.saved_prefs = Some(prefs);
        self.prefs = prefs;
        self.prefs_scope = source;
        Ok(())
    }

    fn save_prefs(&mut self, scope: Scope) -> Result<&'static str, &'static str> {
        let root = match scope {
            Scope::Global => None,
            Scope::Worktree => Some(self.root.as_deref().ok_or("This tab is not in a Git worktree.")?),
        };
        self.prefs.save(root).map_err(|_| "Could not save Milo's pane preferences.")?;
        self.reload_prefs()?;
        if self.exit_from.is_none() {
            self.back();
        }
        Ok(match (scope, &self.prefs_scope) {
            (Scope::Worktree, _) => "Saved pane preferences for this worktree. Reopen panes to apply.",
            (Scope::Global, Source::Worktree) => "Saved global pane preferences. This worktree still overrides them.",
            (Scope::Global, _) => "Saved global pane preferences. Reopen panes to apply.",
        })
    }

    fn use_global_prefs(&mut self) -> Result<&'static str, &'static str> {
        let root = self.root.as_deref().ok_or("This tab is not in a Git worktree.")?;
        Preferences::clear(root).map_err(|_| "Could not clear this worktree's pane preferences.")?;
        self.reload_prefs()?;
        self.back();
        Ok("This worktree now uses global pane preferences. Reopen panes to apply.")
    }

    fn change_pref(&mut self, row: Row) {
        match row {
            Row::Icons => {
                self.prefs.icons = match self.prefs.icons {
                    IconMode::Nerd => IconMode::Plain,
                    IconMode::Plain => IconMode::Off,
                    IconMode::Off => IconMode::Nerd,
                };
            }
            Row::Activity => self.prefs.static_marks = !self.prefs.static_marks,
            Row::GitMarks => self.prefs.text_git_marks = !self.prefs.text_git_marks,
            Row::UsageStyle => self.prefs.usage_large_figures = !self.prefs.usage_large_figures,
            Row::FollowProject => {
                self.prefs.project_mode = match self.prefs.project_mode {
                    ProjectMode::Follow => ProjectMode::Pin,
                    ProjectMode::Pin => ProjectMode::Follow,
                };
            }
            Row::FollowActivity => self.prefs.follow_activity = !self.prefs.follow_activity,
            Row::ChangesOnly => self.prefs.changes_only = !self.prefs.changes_only,
            Row::HistoryFollow => self.prefs.history_follow_new = !self.prefs.history_follow_new,
            _ => unreachable!("only preferences are changed here"),
        }
    }

    fn refresh_health(&mut self) {
        self.health =
            self.store().map_or_else(str::to_owned, |store| match doctor::report_for(store, self.root.as_deref()) {
                Ok(report) | Err(report) => report,
            });
        self.health_top = 0;
    }

    fn maintain(&mut self, remove: bool) {
        let result = if remove { doctor::remove() } else { doctor::repair() };
        if result.is_ok() {
            if self.inside {
                self.saved = None;
                if let Err(why) = self.reload_layout() {
                    self.back();
                    self.report(Err(why));
                    return;
                }
            }
            self.claude = self.store.as_ref().map_or(Claude::Invalid, Store::claude);
            self.show_limits = self.claude == Claude::On;
            self.back();
        }
        self.report_text(result.map(Cow::Owned).map_err(Cow::Owned));
    }

    fn save_claude(&mut self) -> Result<&'static str, &'static str> {
        if !self.claude.editable() {
            return Err("Milo will leave this Claude Code status line alone.");
        }
        if !self.claude_dirty() {
            return Ok("No Claude Code changes to save.");
        }
        self.store()?.save(None, Some(self.show_limits), &self.run)?;
        self.claude = self.store()?.claude();
        self.show_limits = self.claude == Claude::On;
        Ok("Saved Claude Code choices.")
    }

    fn use_global(&mut self) -> Result<&'static str, &'static str> {
        let root = self.root.as_deref().ok_or("This tab is not in a Git worktree.")?;
        self.store()?.use_global(root)?;
        self.reload_layout()?;
        self.back();
        Ok("This worktree now uses the global default.")
    }

    fn preset(&mut self, tree: bool) {
        self.layout = layout_preset(tree, self.layout.new_tabs);
    }

    fn capture(&mut self) -> Result<&'static str, &'static str> {
        if self.trial.is_some() {
            return Err("Undo the current trial before capturing this tab.");
        }
        let store = self.store()?;
        let target = self.target()?;
        let mut layout = store.capture(target)?;
        let size = store.tab_size(target);
        layout.new_tabs = self.layout.new_tabs;
        self.layout = layout;
        self.tab_size = size.or(self.tab_size);
        Ok("Captured Milo panes from this tab. Nothing was saved.")
    }

    fn refresh_preview(&mut self) -> Result<&'static str, &'static str> {
        self.tab_size = self.store()?.tab_size(self.target()?);
        if self.tab_size.is_some() {
            Ok("Preview updated to this tab's current size.")
        } else {
            Err("Rex did not report tab dimensions; the preview shows estimated proportions.")
        }
    }

    fn try_layout(&mut self) -> Result<&'static str, &'static str> {
        if self.trial.is_some() {
            return Err("Undo the current trial before trying another layout.");
        }
        if !self.layout.panes.contains(&true) {
            return Err("There are no Milo panes to try.");
        }
        let trial = self.store()?.trial(&self.layout, self.target()?)?;
        let temporary = trial.temporary();
        self.trial = Some((trial, self.layout.clone()));
        Ok(if temporary {
            "Previewing a temporary rail. Press u to undo. Existing panes are untouched."
        } else {
            "Trying this layout. Press u to undo. Nothing was saved."
        })
    }

    fn undo(&mut self) -> Result<&'static str, &'static str> {
        let Some((trial, _)) = &self.trial else { return Err("There is no trial to undo.") };
        trial.undo()?;
        self.trial = None;
        Ok("Trial undone. No settings changed.")
    }

    fn disabled(&self, row: Row) -> bool {
        match row {
            Row::Capture => self.saved.is_none() || self.target.is_none() || self.trial.is_some(),
            Row::Undo => self.trial.is_none(),
            Row::UseGlobal => self.scope != Scope::Worktree,
            Row::UseGlobalPrefs => self.prefs_scope != Source::Worktree,
            Row::SaveLayout
            | Row::Tree
            | Row::Sidebar
            | Row::Pane(_)
            | Row::Side
            | Row::Width
            | Row::Order(_)
            | Row::OutlineShare
            | Row::UsageRows
            | Row::AutoArrange => self.saved.is_none(),
            Row::SavePrefs
            | Row::Icons
            | Row::Activity
            | Row::GitMarks
            | Row::UsageStyle
            | Row::FollowProject
            | Row::FollowActivity
            | Row::ChangesOnly
            | Row::HistoryFollow => self.saved_prefs.is_none(),
            Row::ClaudeLimits | Row::SaveClaude => !self.claude.editable(),
            Row::Repair | Row::Remove => self.trial.is_some(),
            _ => false,
        }
    }

    fn activate(&mut self, row: Row) -> After {
        if self.disabled(row) {
            return After::Ignored;
        }
        let result = match row {
            Row::Capture => self.capture(),
            Row::Undo => self.undo(),
            Row::SaveClaude => self.save_claude(),
            Row::SaveAndExit => return self.save_and_exit(),
            Row::Discard => return self.leave(),
            Row::SaveGlobal | Row::SaveWorktree => {
                let Panel::Save(kind) = self.panel else { return After::Ignored };
                let scope = if row == Row::SaveGlobal { Scope::Global } else { Scope::Worktree };
                return self.save_scope(kind, scope);
            }
            Row::Continue => match self.panel {
                Panel::Confirm(Confirm::UseGlobal) => self.use_global(),
                Panel::Confirm(Confirm::UseGlobalPrefs(_)) => self.use_global_prefs(),
                Panel::Confirm(Confirm::Repair | Confirm::Remove) => {
                    self.maintain(self.panel == Panel::Confirm(Confirm::Remove));
                    return After::Stay;
                }
                _ => return After::Ignored,
            },
            _ => {
                self.message = None;
                if !matches!(
                    row,
                    Row::Tune
                        | Row::SaveLayout
                        | Row::UseGlobal
                        | Row::SavePrefs
                        | Row::UseGlobalPrefs
                        | Row::Back
                        | Row::Cancel
                        | Row::Diagnostics
                        | Row::Repair
                        | Row::Remove
                ) {
                    self.failure_status(false);
                }
                match row {
                    Row::Tree => self.preset(true),
                    Row::Sidebar => self.preset(false),
                    Row::Tune => self.select_panel(Panel::Tune, 0),
                    Row::SaveLayout => self.select_panel(Panel::Save(SaveKind::Layout), 0),
                    Row::UseGlobal => self.select_panel(Panel::Confirm(Confirm::UseGlobal), 0),
                    Row::Pane(pane) => self.layout.toggle(pane),
                    Row::Side => {
                        self.layout.geometry.side = match self.layout.geometry.side {
                            Side::Left => Side::Right,
                            Side::Right => Side::Left,
                        };
                    }
                    Row::Width => self.layout.next_width(),
                    Row::Order(index) => {
                        let order = &mut self.layout.geometry.order;
                        let next = TabPane::ALL[(order[index] as usize + 1) % TabPane::ALL.len()];
                        let other = order.iter().position(|&pane| pane == next).expect("order includes every pane");
                        order.swap(index, other);
                    }
                    Row::OutlineShare => {
                        cycle(&mut self.layout.outline_share, &["0.50", "0.60", "0.70", "0.80"]);
                        self.layout.geometry.shares = None;
                    }
                    Row::UsageRows => {
                        cycle(&mut self.layout.usage_rows, &["2", "6", "10", "14", "18"]);
                        self.layout.geometry.shares = None;
                    }
                    Row::AutoArrange => self.layout.new_tabs = !self.layout.new_tabs,
                    Row::Back | Row::Cancel => self.back(),
                    Row::ClaudeLimits => self.show_limits = !self.show_limits,
                    Row::SavePrefs | Row::UseGlobalPrefs => {
                        let kind =
                            if self.panel == Panel::Appearance { SaveKind::Appearance } else { SaveKind::Workflow };
                        let panel = if row == Row::SavePrefs {
                            Panel::Save(kind)
                        } else {
                            Panel::Confirm(Confirm::UseGlobalPrefs(kind))
                        };
                        self.select_panel(panel, 0);
                    }
                    Row::Diagnostics => {
                        self.refresh_health();
                        self.select_panel(Panel::Diagnostics, 0);
                    }
                    Row::Repair => self.select_panel(Panel::Confirm(Confirm::Repair), 0),
                    Row::Remove => self.select_panel(Panel::Confirm(Confirm::Remove), 0),
                    _ => self.change_pref(row),
                }
                return After::Stay;
            }
        };
        self.report(result);
        After::Stay
    }
}

fn same_geometry(first: &Layout, second: &Layout) -> bool {
    first.panes == second.panes
        && first.width == second.width
        && first.geometry == second.geometry
        && first.outline_share == second.outline_share
        && first.usage_rows == second.usage_rows
}

fn layout_preset(tree: bool, new_tabs: bool) -> Layout {
    Layout { panes: [true, !tree, !tree], new_tabs, ..Layout::default() }
}

fn cycle(value: &mut String, options: &[&str]) {
    let next = options.iter().skip_while(|&&option| option != value.as_str()).nth(1).unwrap_or(&options[0]);
    value.clear();
    value.push_str(next);
}

impl<R: Fn(&[&OsStr]) -> bool> Pane for Setup<R> {
    fn draw(&mut self, s: &mut Screen) {
        self.draw_setup(s);
    }

    fn tracks_hover(&self) -> bool {
        true
    }

    fn hover_span(&self, x: usize, y: usize) -> Option<Range<usize>> {
        if let Some((_, span)) = self.visible.tab_at(x, y) {
            return Some(span);
        }
        if let Some(span) = self.visible.footer_span_at(x, y) {
            return Some(span);
        }
        let (index, span) = self.visible.row_at(x, y)?;
        (!self.disabled(*self.rows().get(index)?)).then_some(span)
    }

    fn help(&self) -> Page {
        use help::{heading, key};
        let mut rows = vec![
            heading("Setup keys"),
            key("↑↓    ", "move (k j)"),
            key("Enter ", "choose or change"),
            key("Click ", "choose or change"),
            key("1 2 3 4", "switch sections"),
            key("Tab   ", "next section"),
        ];
        if self.inside {
            rows.extend([key("a     ", "try in this tab"), key("u     ", "undo trial")]);
        }
        rows.extend([
            key("s     ", "choose where to save"),
            key("r     ", "refresh preview/report"),
            key("Esc   ", "back or leave settings"),
            key("q     ", "leave settings"),
        ]);
        Page::new(rows)
    }

    fn key(&mut self, key: Key) -> After {
        let after = self.input(key);
        if after != After::Ignored {
            // Several input events may arrive before the next draw. Only act on a
            // click when its target still belongs to the frame on screen.
            self.visible.invalidate();
        }
        after
    }
}

impl<R: Fn(&[&OsStr]) -> bool> Setup<R> {
    fn click(&mut self, x: usize, y: usize) -> After {
        if let Some((panel, _)) = self.visible.tab_at(x, y) {
            self.select_panel(panel, 0);
            return After::Stay;
        }
        let panel = self.panel;
        let after = if let Some(key) = self.visible.footer_at(x, y) {
            if key == Key::Char('?') { After::Help } else { self.input(key) }
        } else {
            let Some((index, _)) = self.visible.row_at(x, y) else { return After::Ignored };
            let Some(&row) = self.rows().get(index) else { return After::Ignored };
            if self.disabled(row)
                || matches!(row, Row::Continue | Row::SaveGlobal | Row::SaveWorktree | Row::SaveAndExit | Row::Discard)
                    && self.modal_click.is_repeat(x, y)
            {
                return After::Ignored;
            }
            self.selected = index;
            self.activate(row)
        };
        if self.panel != panel && self.panel.modal() {
            self.modal_click.opened(x, y);
        }
        after
    }

    fn input(&mut self, key: Key) -> After {
        let modal = self.panel.modal();
        if let Some(rows) = key.rows(self.visible.len) {
            let rows = if matches!(key, Key::WheelUp | Key::WheelDown) {
                let page = self.visible.len.clamp(1, 3).cast_signed();
                rows.clamp(-page, page)
            } else {
                rows
            };
            if self.panel == Panel::Diagnostics {
                self.health_top = self.health_top.saturating_add_signed(rows);
            } else {
                self.selected = self.selected.saturating_add_signed(rows).min(self.rows().len().saturating_sub(1));
            }
            return After::Stay;
        }
        let action = match key {
            Key::Enter | Key::Char(' ') => self.rows().get(self.selected).copied(),
            Key::Click(x, y) => return self.click(x, y),
            Key::Ctrl(b'i') if !modal => {
                self.next_panel();
                return After::Stay;
            }
            Key::Char(digit @ '1'..='4') if !modal => {
                let panel = SECTIONS[usize::from(digit as u8 - b'1')];
                self.select_panel(panel, 0);
                return After::Stay;
            }
            Key::Char('a') if self.inside && matches!(self.panel, Panel::Layout | Panel::Tune) => {
                let result = self.try_layout();
                self.report(result);
                return After::Stay;
            }
            Key::Char('u') if self.inside && !modal => Some(Row::Undo),
            Key::Char('r') if self.panel == Panel::Diagnostics => {
                self.refresh_health();
                return After::Stay;
            }
            Key::Char('r') if self.inside && matches!(self.panel, Panel::Layout | Panel::Tune) => {
                let result = self.refresh_preview();
                self.report(result);
                return After::Stay;
            }
            Key::Char('s') => match self.panel {
                Panel::Layout | Panel::Tune if self.inside => Some(Row::SaveLayout),
                Panel::Appearance | Panel::Workflow => Some(Row::SavePrefs),
                _ => None,
            },
            Key::Esc | Key::Char('q') if modal => {
                self.back();
                return After::Stay;
            }
            Key::Esc if matches!(self.panel, Panel::Tune | Panel::Diagnostics) => {
                self.back();
                return After::Stay;
            }
            Key::Char('q') | Key::Esc => return self.request_exit(),
            _ => None,
        };
        action.map_or(After::Ignored, |row| self.activate(row))
    }
}

impl<R> Setup<R> {
    fn failure_status(&mut self, failed: bool) {
        if self.real && self.inside && failed != self.status_failed {
            crate::term::setup_status(failed);
            self.status_failed = failed;
        }
    }

    fn clear_notice(&mut self) {
        self.message = None;
        self.failure_status(false);
    }

    fn report(&mut self, result: Result<&'static str, &'static str>) {
        self.report_text(result.map(Cow::Borrowed).map_err(Cow::Borrowed));
    }

    fn report_text(&mut self, result: Result<Cow<'static, str>, Cow<'static, str>>) {
        self.failure_status(result.is_err());
        self.message = Some(match result {
            Ok(message) => (message, FAINT),
            Err(why) => (why, DEL.bold()),
        });
    }
}

impl<R> Drop for Setup<R> {
    fn drop(&mut self) {
        if let Some((trial, _)) = &self.trial {
            // Ctrl-C and a disconnected terminal bypass Pane::key.
            let _ = trial.undo();
        }
        self.failure_status(false);
    }
}

/// Rex calls this separately so listener lifetime does not depend on a pane.
pub fn listen(stop: bool) -> i32 {
    fn run(stop: bool) -> io::Result<()> {
        let state = paths::state_dir().ok_or(io::ErrorKind::NotFound)?;
        if stop {
            lock::stop(&state)
        } else {
            let config = paths::config_dir().ok_or(io::ErrorKind::NotFound)?;
            rex_files::check_config_dir(&config).map_err(io::Error::other)?;
            lock::listen(&state, &config.join("listener.lua"))
        }
    }
    let Err(error) = run(stop) else { return 0 };
    if stop {
        eprintln!("milo: could not stop the Rex listener: {error}");
    } else if error.kind() == io::ErrorKind::NotFound {
        eprintln!("milo: could not start the layout listener. Check the managed Rex integration with milo doctor.");
    } else {
        eprintln!("milo: could not start the Rex listener: {error}");
    }
    1
}

#[cfg(test)]
mod tests;
