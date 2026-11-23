//! Preview, try, remember and maintain Milo's pane setup.

mod claude;
pub(crate) mod doctor;
mod draw;
mod files;
mod install;
mod layout;
pub(crate) mod lock;
mod preview;
mod profiles;
mod rex_files;
mod settings;
pub(crate) mod transfer;
pub(crate) mod web;
mod welcome;

pub(crate) use draw::wrap;
pub use profiles::{layout_profile, return_from_peek};
pub use welcome::{Welcome, needs_welcome, skip_welcome};

use std::borrow::Cow;
use std::ffi::OsStr;
use std::io;
use std::ops::Range;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use crate::alerts::{self, Alerts};
use crate::help::{self, Page};
use crate::keys::{ConfirmClick, Key};
use crate::menubar::{self, Action as MenuAction};
use crate::pane::{After, External, Pane};
use crate::preferences::{IconMode, PreferenceSnapshot, Preferences, ProjectMode, Source, UsageTool};
use crate::presence;
use crate::screen::{Screen, Style};
use crate::ui::{self, DEL, FAINT};
use crate::{paths, rex};
use claude::Claude;
use install::{Saved, Store};
use layout::{Layout, Side, TabPane};
use profiles::Scope;

const PREFS_CHANGED: &str = "Milo's pane preferences changed. Reopen Settings before saving again.";
const PREFS_READ: &str = "Could not read Milo's pane preferences. They were left alone.";
const PREFS_SAVE: &str = "Could not save Milo's pane preferences.";
const PREFS_INHERIT: &str = "Could not clear this worktree's pane preferences.";
const MAINTENANCE_DRAFT: &str = "Save or discard the layout and Claude Code edits before changing the integration.";
const REOPEN_PANES: &str = "Reopen panes to apply.";
const NO_PREFS_CHANGES: &str = "No pane preference changes to save.";

const PRESENCE_UNAVAILABLE: &str = "Could not find your settings folders.";

type SaveNotice = Result<Cow<'static, str>, &'static str>;
type PreferenceSave = Result<PreferenceFeedback, &'static str>;

enum PreferenceChange {
    Save(Scope),
    Inherit,
}

struct PreferenceFeedback {
    text: Cow<'static, str>,
    reopen: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    Layout,
    Tune,
    Appearance,
    Workflow,
    Alerts,
    Health,
    Diagnostics,
    /// The choices for Discord presence, opened from Workflow.
    Presence,
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
    RemoveMenuBar,
    MirrorAlerts,
    /// Turning Discord presence on.
    Presence,
    /// Showing a name on Discord that is not shown yet.
    PresenceDetails(presence::Details),
    /// Using a Discord application of the person's own.
    PresenceApp,
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
    UsageTool(UsageTool),
    FollowProject,
    FollowActivity,
    ChangesOnly,
    HistoryFollow,
    TabTitles,
    ClaudeLimits,
    Alerts,
    AlertsOn,
    AlertsDestination,
    AlertKind(alerts::Kind),
    AlertsContent,
    AlertsTest,
    MenuBar,
    MenuIndicator,
    RemoveMenuBar,
    /// Opens the Discord presence choices.
    PresenceOpen,
    Presence,
    PresenceDetails,
    PresenceApp,
    PresenceId,
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
const OUTLINE_SHARES: &[&str] = &["0.50", "0.60", "0.70", "0.80"];
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
    Row::TabTitles,
    Row::SavePrefs,
    Row::UseGlobalPrefs,
    Row::ClaudeLimits,
    Row::SaveClaude,
    Row::Alerts,
    Row::PresenceOpen,
    Row::MenuBar,
    Row::MenuIndicator,
    Row::RemoveMenuBar,
];
const ALERTS: &[Row] = &[
    Row::AlertsOn,
    Row::AlertsDestination,
    Row::AlertKind(alerts::Kind::Blocked),
    Row::AlertKind(alerts::Kind::Error),
    Row::AlertKind(alerts::Kind::Done),
    Row::AlertKind(alerts::Kind::Notification),
    Row::AlertKind(alerts::Kind::Bell),
    Row::AlertsContent,
    Row::AlertsTest,
    Row::Back,
];
const ALERTS_UNAVAILABLE: &str = "Milo could not read its alert settings. Run milo alerts to see why.";
const NO_OTHER_DESTINATION: &str =
    "No other destination is saved. Save one with milo alerts telegram CHAT or milo alerts discord, or in milo web.";

/// The draft is never written when navigating, capturing or trying a layout.
pub struct Setup<R = fn(&[&OsStr]) -> bool> {
    store: Option<Store>,
    inside: bool,
    target: Option<rex::Target>,
    web_window: Option<String>,
    root: Option<PathBuf>,
    run: R,
    layout: Layout,
    saved: Option<Layout>,
    saved_global: Option<Result<Layout, &'static str>>,
    scope: Scope,
    installed: bool,
    trial: Option<(profiles::Trial, Layout)>,
    tab_size: Option<(usize, usize)>,
    prefs: Preferences,
    saved_prefs: Option<PreferenceSnapshot>,
    claude: Claude,
    show_limits: bool,
    menu_installed: Option<bool>,
    menu_indicator: Option<menubar::Indicator>,
    presence: Option<presence::Control>,
    /// The saved Discord presence choice and what its helper last did, if they could be read.
    presence_view: Option<presence::Status>,
    /// The digits typed so far for a Discord application ID.
    application_draft: String,
    /// What Discord shows now, for the row's detail line.
    presence_note: String,
    panel: Panel,
    selected: usize,
    save_from: Option<(Panel, Option<Row>)>,
    exit_from: Option<(Panel, Option<Row>)>,
    visible: draw::Visible,
    modal_click: ConfirmClick,
    diagnostics: doctor::Checks,
    health: String,
    health_top: usize,
    icon_override: Option<IconMode>,
    message: Option<(Cow<'static, str>, Style)>,
    real: bool,
    status_failed: bool,
    alerts: Option<Alerts>,
    /// The alert choices as last read. They are saved as they are made.
    alert_summary: Option<alerts::Summary>,
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
        setup.menu_installed = menubar::installed();
        setup.reload_menu_indicator();
        setup.alerts = Alerts::real();
        setup.presence = setup.presence.take().map(presence::Control::starting_helper);
        setup.icon_override = IconMode::environment();
        setup.real = true;
        if setup.message.as_ref().is_some_and(|(_, style)| *style == DEL.bold()) {
            setup.failure_status(true);
        }
        setup
    }

    pub(crate) fn swap_diagnostics(&mut self, checks: &mut doctor::Checks) {
        std::mem::swap(&mut self.diagnostics, checks);
    }
}

impl<R: Fn(&[&OsStr]) -> bool> Setup<R> {
    fn new(store: Option<Store>, inside: bool, target: Option<rex::Target>, root: Option<PathBuf>, run: R) -> Self {
        let claude = store.as_ref().map_or(Claude::Invalid, Store::claude);
        let mut setup = Self::new_with_claude(store, inside, target, root, run, claude);
        if let Err(why) = setup.reload_prefs() {
            setup.report(Err(why));
        }
        setup
    }

    fn new_with_claude(
        store: Option<Store>,
        inside: bool,
        target: Option<rex::Target>,
        root: Option<PathBuf>,
        run: R,
        claude: Claude,
    ) -> Self {
        let presence = store.as_ref().map(|store| presence::Control::at(store.config.clone(), store.state.clone()));
        let mut setup = Self {
            store,
            inside,
            target,
            web_window: None,
            root,
            run,
            layout: Layout::default(),
            saved: None,
            saved_global: None,
            scope: Scope::Global,
            installed: false,
            trial: None,
            tab_size: None,
            prefs: Preferences::default(),
            saved_prefs: None,
            claude,
            show_limits: claude == Claude::On,
            menu_installed: None,
            menu_indicator: None,
            presence,
            presence_view: None,
            application_draft: String::new(),
            presence_note: String::new(),
            panel: if inside { Panel::Layout } else { Panel::Workflow },
            selected: 0,
            save_from: None,
            exit_from: None,
            visible: draw::Visible::default(),
            modal_click: ConfirmClick::default(),
            diagnostics: doctor::Checks::default(),
            health: String::new(),
            health_top: 0,
            icon_override: None,
            message: None,
            real: false,
            status_failed: false,
            alerts: None,
            alert_summary: None,
        };
        setup.reload_presence();
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
        self.saved_global = (scope == Scope::Worktree).then(|| {
            let (mut global, _, _) = self.store()?.layout_for(None)?;
            // Auto-arrange is global, including in the displayed worktree draft.
            global.new_tabs = layout.new_tabs;
            Ok(global)
        });
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
            Panel::Presence => &[Row::Presence, Row::PresenceDetails, Row::PresenceApp, Row::Back],
            Panel::Appearance => &[
                Row::Icons,
                Row::Activity,
                Row::GitMarks,
                Row::UsageStyle,
                Row::UsageTool(UsageTool::Codex),
                Row::UsageTool(UsageTool::Claude),
                Row::UsageTool(UsageTool::OpenCode),
                Row::SavePrefs,
                Row::UseGlobalPrefs,
            ],
            Panel::Workflow => match self.menu_installed {
                Some(true) => WORKFLOW,
                Some(false) => &WORKFLOW[..WORKFLOW.len() - 1],
                None => &WORKFLOW[..WORKFLOW.len() - 3],
            },
            Panel::Alerts => ALERTS,
            Panel::Health => &[Row::Diagnostics, Row::Repair, Row::Remove],
            Panel::Diagnostics => &[],
            Panel::Save(kind)
                if self.root.is_some() && !(kind == SaveKind::Layout && self.only_auto_arrange_dirty()) =>
            {
                &[Row::Cancel, Row::SaveGlobal, Row::SaveWorktree]
            }
            Panel::Save(_) => &[Row::Cancel, Row::SaveGlobal],
            Panel::Confirm(Confirm::PresenceApp) => &[Row::Cancel, Row::PresenceId, Row::Continue],
            Panel::Confirm(_) => &[Row::Cancel, Row::Continue],
            Panel::Exit => &[Row::Cancel, Row::SaveAndExit, Row::Discard],
        }
    }

    fn select_panel(&mut self, panel: Panel, row: Option<Row>) {
        let previous = self.panel;
        if previous == Panel::Diagnostics && panel != previous {
            self.diagnostics.discard();
        }
        self.panel = panel;
        self.selected = row.and_then(|row| self.rows().iter().position(|&candidate| candidate == row)).unwrap_or(0);
        self.visible = draw::Visible::default();
        self.modal_click.clear();
        self.message = None;
        if matches!(panel, Panel::Workflow | Panel::Presence) {
            // The helper comes and goes with Rex and Discord, so say what is true now.
            self.reload_presence();
        }
        if panel == Panel::Workflow {
            self.reload_menu_indicator();
        }
        if panel == Panel::Diagnostics && panel != previous {
            self.refresh_health();
        }
        if panel == Panel::Alerts && previous != panel {
            self.refresh_alerts();
        }
    }

    fn selected_row(&self) -> Option<Row> {
        self.rows().get(self.selected).copied()
    }

    fn open_save(&mut self, kind: SaveKind) {
        self.save_from = Some((self.panel, self.selected_row()));
        self.select_panel(Panel::Save(kind), None);
    }

    fn back(&mut self) {
        let (panel, selected) = match self.panel {
            Panel::Tune => (Panel::Layout, Some(Row::Tune)),
            Panel::Presence => (Panel::Workflow, Some(Row::PresenceOpen)),
            Panel::Exit => self.exit_from.take().expect("exit prompt has an editor to return to"),
            Panel::Save(_) if self.exit_from.is_some() => (Panel::Exit, Some(Row::Cancel)),
            Panel::Save(_) => self.save_from.take().expect("save prompt has an editor to return to"),
            Panel::Confirm(Confirm::UseGlobal) => {
                let row = if self.scope == Scope::Worktree { Row::UseGlobal } else { Row::Tune };
                (Panel::Layout, Some(row))
            }
            Panel::Confirm(Confirm::UseGlobalPrefs(kind)) => (kind.panel(), Some(Row::UseGlobalPrefs)),
            Panel::Confirm(Confirm::Repair) => (Panel::Health, Some(Row::Repair)),
            Panel::Confirm(Confirm::Remove) => (Panel::Health, Some(Row::Remove)),
            Panel::Confirm(Confirm::RemoveMenuBar) => (Panel::Workflow, Some(Row::RemoveMenuBar)),
            Panel::Alerts => (Panel::Workflow, Some(Row::Alerts)),
            Panel::Confirm(Confirm::MirrorAlerts) => (Panel::Alerts, Some(Row::AlertsOn)),
            Panel::Confirm(Confirm::Presence) => (Panel::Presence, Some(Row::Presence)),
            Panel::Confirm(Confirm::PresenceDetails(_)) => (Panel::Presence, Some(Row::PresenceDetails)),
            Panel::Confirm(Confirm::PresenceApp) => (Panel::Presence, Some(Row::PresenceApp)),
            Panel::Diagnostics => (Panel::Health, Some(Row::Diagnostics)),
            _ => (Panel::Layout, None),
        };
        self.select_panel(panel, selected);
    }

    fn next_panel(&mut self) {
        let next = match self.panel {
            Panel::Layout | Panel::Tune => Panel::Appearance,
            Panel::Appearance => Panel::Workflow,
            Panel::Workflow | Panel::Alerts | Panel::Presence => Panel::Health,
            Panel::Health | Panel::Diagnostics => Panel::Layout,
            Panel::Save(_) | Panel::Confirm(_) | Panel::Exit => return,
        };
        self.select_panel(next, None);
    }

    fn layout_dirty(&self) -> bool {
        self.saved.as_ref().is_some_and(|saved| *saved != self.layout)
    }

    fn auto_arrange_dirty(&self) -> bool {
        self.saved.as_ref().is_some_and(|saved| saved.new_tabs != self.layout.new_tabs)
    }

    fn only_auto_arrange_dirty(&self) -> bool {
        self.auto_arrange_dirty() && self.saved.as_ref().is_some_and(|saved| same_geometry(saved, &self.layout))
    }

    fn prefs_scope(&self) -> Source {
        self.saved_prefs.as_ref().map_or(Source::Default, |saved| saved.source)
    }

    fn prefs_dirty(&self) -> bool {
        self.saved_prefs.as_ref().is_some_and(|saved| saved.prefs != self.prefs)
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
        self.exit_from = Some((self.panel, self.selected_row()));
        self.select_panel(Panel::Exit, None);
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
        self.diagnostics.discard();
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
                self.select_panel(Panel::Exit, None);
                self.report(Err(why));
                return After::Stay;
            }
            return self.leave();
        };
        self.select_panel(Panel::Save(kind), None);
        After::Stay
    }

    fn save_scope(&mut self, kind: SaveKind, scope: Scope) -> After {
        let result = if kind == SaveKind::Layout { self.save_layout(scope) } else { self.save_prefs(scope) };
        if self.exit_from.is_some() {
            if result.is_ok() {
                let after = self.save_and_exit();
                if after == After::Stay && self.panel != Panel::Exit {
                    self.report_text(result.map_err(Cow::Borrowed));
                }
                return after;
            }
            // Some integration errors are reported after the file was saved.
            // Don't offer that scope again if its draft is already saved.
            let still_dirty = if kind == SaveKind::Layout { self.layout_dirty() } else { self.prefs_dirty() };
            let layout_saved = kind == SaveKind::Layout && scope == Scope::Worktree && self.only_auto_arrange_dirty();
            if !still_dirty || layout_saved {
                self.back();
            }
        }
        self.report_text(result.map_err(Cow::Borrowed));
        After::Stay
    }

    fn save_layout(&mut self, scope: Scope) -> SaveNotice {
        if self.trial.as_ref().is_some_and(|(trial, _)| trial.temporary()) {
            self.undo()?;
        }
        let message = self.persist_layout(scope, true)?;
        if self.exit_from.is_none() {
            self.back();
        }
        message
    }

    /// The browser can remember geometry without setting up Rex. Keep draft
    /// interpretation, conflict detection and trial finalization shared.
    fn persist_layout(&mut self, scope: Scope, integrate_rex: bool) -> Result<SaveNotice, &'static str> {
        let auto_changed = self.auto_arrange_dirty();
        let draft_auto = self.layout.new_tabs;
        let before = self.saved.as_ref().ok_or("Reopen Settings before saving the layout.")?;
        let only_auto = auto_changed && same_geometry(before, &self.layout);
        let expected = match (scope, &self.saved_global) {
            (Scope::Global, Some(global)) => (global.as_ref().map_err(|&why| why)?, Scope::Global),
            _ => (before, self.scope),
        };
        let global;
        let layout = if scope == Scope::Global && only_auto && self.scope == Scope::Worktree {
            // Do not promote the worktree layout just to save the global switch.
            global = Layout { new_tabs: draft_auto, ..expected.0.clone() };
            &global
        } else {
            &self.layout
        };
        let expected = profiles::DraftBaseline {
            destination: expected,
            worktree: (scope == Scope::Global && self.scope == Scope::Worktree).then_some(before),
        };
        let store = self.store()?;
        let (saved, warning) = if integrate_rex {
            match store.save_draft(layout, expected, scope, self.root.as_deref(), &self.run) {
                Ok(saved) => (saved, None),
                Err(install::RELOAD_REMINDER_NOT_CLEARED) => (Saved::Ready, Some(install::RELOAD_REMINDER_NOT_CLEARED)),
                Err(why) => return Err(why),
            }
        } else {
            (store.save_offline_draft(layout, expected, scope, self.root.as_deref())?, None)
        };
        let remaining_auto = (scope == Scope::Worktree && auto_changed).then_some(draft_auto);
        let result = self.finish_layout_save(saved, remaining_auto).and_then(|()| warning.map_or(Ok(()), Err));
        Ok(result.map(|()| self.layout_save_feedback(scope, integrate_rex, only_auto, auto_changed)))
    }

    fn finish_layout_save(&mut self, saved: Saved, remaining_auto: Option<bool>) -> Result<(), &'static str> {
        if let Err(why) = self.reload_layout() {
            self.saved = None;
            return Err(why);
        }
        if let Some(draft_auto) = remaining_auto {
            self.layout.new_tabs = draft_auto;
        }
        if self.trial.as_ref().is_some_and(|(trial, _)| trial.temporary()) {
            self.undo()?;
        }
        if let Some((trial, tried)) = &self.trial
            && same_geometry(tried, &self.layout)
        {
            trial.keep()?;
            self.trial = None;
        }
        match saved {
            Saved::ListenerDidNotStart => {
                Err("Saved, but the layout listener did not start. Open Health to repair it.")
            }
            Saved::ListenerDidNotStop => Err("Saved, but the layout listener did not stop. Open Health to repair it."),
            Saved::Ready => Ok(()),
        }
    }

    fn layout_save_feedback(
        &self,
        scope: Scope,
        integrate_rex: bool,
        only_auto: bool,
        auto_changed: bool,
    ) -> Cow<'static, str> {
        let auto_effect = if self.layout.new_tabs {
            "New tabs will be arranged automatically."
        } else {
            "New tabs won't be arranged automatically."
        };
        if only_auto && scope == Scope::Global {
            return format!("Saved global auto-arrange. {auto_effect}").into();
        }
        let mut message = match (scope, self.scope, integrate_rex) {
            (Scope::Worktree, _, _) => {
                let worktree = self.worktree_subject("this worktree");
                let effect = if auto_changed {
                    "Auto-arrange still needs a global save."
                } else if integrate_rex {
                    "Used when arranging its tabs."
                } else {
                    "Rex integration unchanged."
                };
                format!("Saved layout for {worktree}. {effect}").into()
            }
            (Scope::Global, Scope::Worktree, _) => {
                let worktree = self.worktree_subject("This worktree");
                format!("Saved the global default. {worktree} keeps its layout.").into()
            }
            (Scope::Global, _, true) => {
                Cow::Borrowed("Saved the global default. Used when arranging tabs without an override.")
            }
            (Scope::Global, _, false) => Cow::Borrowed("Saved the global default layout. Rex integration unchanged."),
        };
        if scope == Scope::Global && auto_changed {
            let message = message.to_mut();
            message.push(' ');
            message.push_str(auto_effect);
        }
        message
    }

    fn worktree_name(&self) -> Option<&str> {
        let name = self.root.as_deref()?.file_name()?.to_str()?;
        // Do not let a directory name inject control characters or reorder its status text.
        (!name.chars().any(|ch| {
            ch.is_control()
                || matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        }))
        .then_some(name)
    }

    fn worktree_subject(&self, fallback: &'static str) -> WorktreeName<'_> {
        WorktreeName(self.worktree_name(), fallback)
    }

    fn reload_prefs(&mut self) -> Result<(), &'static str> {
        self.saved_prefs = None;
        let store = self.store()?;
        let snapshot = PreferenceSnapshot::load(&store.config, self.root.as_deref()).map_err(|_| PREFS_READ)?;
        self.prefs = snapshot.prefs;
        self.saved_prefs = Some(snapshot);
        Ok(())
    }

    fn persist_prefs(&mut self, scope: Scope) -> Result<PreferenceSave, &'static str> {
        let snapshot = self.saved_prefs.as_ref().ok_or(PREFS_SAVE)?;
        let before = snapshot.prefs;
        snapshot
            .save(self.prefs, &self.store()?.config, self.root.as_deref(), scope == Scope::Worktree)
            .map_err(|error| prefs_write_error(error, PREFS_SAVE))?;
        Ok(self.reload_prefs().map(|()| self.preference_feedback(before, PreferenceChange::Save(scope))))
    }

    fn inherit_prefs(&mut self) -> Result<PreferenceSave, &'static str> {
        let snapshot = self.saved_prefs.as_ref().ok_or(PREFS_INHERIT)?;
        let before = snapshot.prefs;
        let root = self.root.as_deref().ok_or("This tab is not in a Git worktree.")?;
        snapshot.inherit(&self.store()?.config, root).map_err(|error| prefs_write_error(error, PREFS_INHERIT))?;
        Ok(self.reload_prefs().map(|()| self.preference_feedback(before, PreferenceChange::Inherit)))
    }

    fn preference_feedback(&self, before: Preferences, change: PreferenceChange) -> PreferenceFeedback {
        let titles_changed = before.tab_titles != self.prefs.tab_titles;
        let reopen = (Preferences { tab_titles: self.prefs.tab_titles, ..before }) != self.prefs;
        let effect = if reopen {
            REOPEN_PANES
        } else if titles_changed {
            "Milo uses the title setting when naming Rex tabs."
        } else {
            "Pane behavior is unchanged."
        };
        let text = match change {
            PreferenceChange::Save(Scope::Global) if before == self.prefs && self.prefs_scope() == Source::Worktree => {
                let worktree = self.worktree_subject("This worktree");
                format!("Saved global pane preferences. {worktree} keeps its preferences.")
            }
            PreferenceChange::Save(Scope::Global) => format!("Saved global pane preferences. {effect}"),
            PreferenceChange::Save(Scope::Worktree) => {
                let worktree = self.worktree_subject("this worktree");
                format!("Saved pane preferences for {worktree}. {effect}")
            }
            PreferenceChange::Inherit => {
                let worktree = self.worktree_subject("This worktree");
                format!("{worktree} now uses global pane preferences. {effect}")
            }
        };
        PreferenceFeedback { text: text.into(), reopen }
    }

    fn save_prefs(&mut self, scope: Scope) -> SaveNotice {
        let message = if self.prefs_dirty() { self.persist_prefs(scope)??.text } else { NO_PREFS_CHANGES.into() };
        if self.exit_from.is_none() {
            self.back();
        }
        Ok(message)
    }

    fn use_global_prefs(&mut self) -> SaveNotice {
        let feedback = self.inherit_prefs()??;
        self.back();
        Ok(feedback.text)
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
            Row::UsageTool(tool) => self.prefs.usage_tools[tool] = self.prefs.usage_tools[tool].next(),
            Row::FollowProject => {
                self.prefs.project_mode = match self.prefs.project_mode {
                    ProjectMode::Follow => ProjectMode::Pin,
                    ProjectMode::Pin => ProjectMode::Follow,
                };
            }
            Row::FollowActivity => self.prefs.follow_activity = !self.prefs.follow_activity,
            Row::ChangesOnly => self.prefs.changes_only = !self.prefs.changes_only,
            Row::HistoryFollow => self.prefs.history_follow_new = !self.prefs.history_follow_new,
            Row::TabTitles => self.prefs.tab_titles = !self.prefs.tab_titles,
            _ => unreachable!("only preferences are changed here"),
        }
    }

    fn refresh_health(&mut self) {
        if self.diagnostics.checking() {
            return;
        }
        let store = self.store.clone();
        let root = self.root.clone();
        if let Err(error) = self.diagnostics.start(move || {
            let store = store.ok_or_else(|| "Could not find your settings folders.".to_owned())?;
            doctor::full_report(&store, root.as_deref())
        }) {
            self.health = error;
        }
        self.health_top = 0;
    }

    fn maintain(&mut self, remove: bool) {
        if self.layout_dirty() || self.claude_dirty() {
            self.report(Err(MAINTENANCE_DRAFT));
            return;
        }
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
        self.claude_save_feedback()
    }

    fn claude_save_feedback(&self) -> Result<&'static str, &'static str> {
        match self.claude {
            Claude::On => Ok("Saved Claude Code account settings. Milo's status line is enabled."),
            Claude::Off => Ok("Saved Claude Code account settings. Milo's status line is disabled."),
            _ => Err("Saved Claude Code account settings, but could not confirm Milo's status line."),
        }
    }

    fn reload_menu_indicator(&mut self) {
        self.menu_indicator = self
            .menu_installed
            .and(self.store.as_ref())
            .and_then(|store| menubar::Indicator::load(&menubar::Indicator::file(&store.config)).ok());
    }

    fn change_menu_app(&mut self, action: MenuAction) {
        let install = action == MenuAction::Install;
        let result = menubar::run(action);
        self.menu_installed = menubar::installed();
        let row = if !install && self.menu_installed == Some(true) { Row::RemoveMenuBar } else { Row::MenuBar };
        self.select_panel(Panel::Workflow, Some(row));
        self.report_text(result.map(Cow::Owned).map_err(|why| Cow::Owned(format!("Milo: {why}"))));
    }

    /// Reads the alert choices again, which asks where secrets are kept what is saved.
    fn refresh_alerts(&mut self) {
        self.alert_summary = self.alerts.as_ref().and_then(|alerts| alerts.summary().ok());
    }

    /// Alert choices are saved as they are made, like the menu bar app, and
    /// are no part of the draft.
    fn change_alerts(&mut self, change: impl FnOnce(&Alerts) -> Result<String, String>) {
        let result = self.alerts.as_ref().ok_or_else(|| ALERTS_UNAVAILABLE.to_owned()).and_then(change);
        self.refresh_alerts();
        // There is room for a line or two; the first line says what happened.
        let first = |text: String| Cow::Owned(text.lines().next().unwrap_or_default().to_owned());
        self.report_text(result.map(first).map_err(Cow::Owned));
    }

    fn activate_alert(&mut self, row: Row) {
        let Some(summary) = &self.alert_summary else {
            self.report(Err(ALERTS_UNAVAILABLE));
            return;
        };
        let settings = summary.settings.clone();
        let saved = summary.saved;
        match row {
            Row::AlertsOn if settings.enabled => self.change_alerts(Alerts::disable),
            Row::AlertsOn => self.select_panel(Panel::Confirm(Confirm::MirrorAlerts), None),
            Row::AlertsDestination => {
                let other = alerts::Destination::ALL
                    .into_iter()
                    .find(|&destination| Some(destination) != settings.destination && saved[destination as usize]);
                match other {
                    Some(destination) => self.change_alerts(|alerts| alerts.choose_for_review(destination)),
                    None => self.report(Err(NO_OTHER_DESTINATION)),
                }
            }
            Row::AlertKind(kind) => self.change_alerts(|alerts| alerts.set_kind(kind, !settings.mirrors(kind))),
            Row::AlertsContent => {
                let next = match settings.content {
                    alerts::Content::Metadata => alerts::Content::Text,
                    alerts::Content::Text => alerts::Content::Metadata,
                };
                self.change_alerts(|alerts| alerts.set_content(next));
            }
            Row::AlertsTest => self.change_alerts(Alerts::test),
            _ => {}
        }
    }

    fn reload_presence(&mut self) {
        self.presence_view = self.presence.as_ref().and_then(|control| control.status().ok());
        self.presence_note =
            self.presence_view.as_ref().map(|status| status.sentence().into_owned()).unwrap_or_default();
    }

    /// Saves one change to Discord presence at once: it is one global choice, not part of a draft.
    /// The change is to the choice this screen last showed.
    fn change_presence(&mut self, change: presence::Change) -> Result<String, &'static str> {
        let shown = self.presence_view.as_ref().map(|status| status.setting);
        self.change_shown_presence(change, shown)
    }

    /// Saves one change to the Discord presence choice a surface had `shown` when it was asked for.
    fn change_shown_presence(
        &mut self,
        change: presence::Change,
        shown: Option<presence::Setting>,
    ) -> Result<String, &'static str> {
        let result = match (&self.presence, shown) {
            (Some(control), Some(shown)) => control.apply(change, shown),
            _ => Err(PRESENCE_UNAVAILABLE),
        };
        self.reload_presence();
        result
    }

    fn disable_presence(&mut self) -> Result<String, &'static str> {
        let result = self.presence.as_ref().ok_or(PRESENCE_UNAVAILABLE)?.disable();
        self.reload_presence();
        result
    }

    fn report_presence(&mut self, result: Result<String, &'static str>) {
        self.report_text(result.map(Cow::Owned).map_err(Cow::Borrowed));
    }

    /// Turning presence off needs no question. Turning it on first says what becomes public.
    fn choose_presence(&mut self) {
        let Some(status) = &self.presence_view else { return };
        if status.setting.enabled {
            let result = self.disable_presence();
            self.report_presence(result);
        } else {
            self.select_panel(Panel::Confirm(Confirm::Presence), None);
        }
    }

    /// Moves to the next choice of names. One that would show a name not
    /// shown now is asked about first; while presence is off it is only saved.
    fn choose_presence_details(&mut self) {
        let Some(status) = &self.presence_view else { return };
        let setting = status.setting;
        let next = setting.details.next();
        if setting.enabled && next.shows_more_than(setting.details) {
            self.select_panel(Panel::Confirm(Confirm::PresenceDetails(next)), None);
        } else {
            let result = self.change_presence(presence::Change::Details(next));
            self.report_presence(result);
        }
    }

    /// Goes back to Milo's application from a person's own, or asks for the ID of theirs.
    fn choose_presence_application(&mut self) {
        let Some(status) = &self.presence_view else { return };
        if status.setting.application.is_some() {
            let result = self.change_presence(presence::Change::Application(None));
            self.report_presence(result);
        } else {
            self.application_draft.clear();
            self.select_panel(Panel::Confirm(Confirm::PresenceApp), Some(Row::PresenceId));
            self.show_application_draft();
        }
    }

    /// The row has no room for the whole number, so the notice shows what was typed.
    fn show_application_draft(&mut self) {
        let text = if self.application_draft.is_empty() {
            Cow::Borrowed(
                "Type your Discord application ID. Pasted text is not read here: milo presence application APPLICATION_ID takes one.",
            )
        } else {
            Cow::Owned(format!("Application ID: {}", self.application_draft))
        };
        self.message = Some((text, FAINT));
    }

    fn type_application_digit(&mut self, digit: char) {
        // Discord's IDs are at most this long.
        if self.application_draft.len() < 19 {
            self.application_draft.push(digit);
        }
        self.show_application_draft();
    }

    /// Carries out what a presence question asked about.
    fn confirm_presence(&mut self, confirm: Confirm) -> After {
        let change = match confirm {
            Confirm::Presence => presence::Change::Enable,
            Confirm::PresenceDetails(details) => presence::Change::Details(details),
            Confirm::PresenceApp => match presence::application_id(&self.application_draft) {
                Some(own) => presence::Change::Application(Some(own)),
                None => return After::Ignored,
            },
            _ => return After::Ignored,
        };
        let result = self.change_presence(change);
        if result.is_ok() {
            self.back();
        }
        self.report_presence(result);
        After::Stay
    }

    fn use_global(&mut self) -> SaveNotice {
        let message = self.inherit_layout()??;
        self.back();
        Ok(message)
    }

    fn inherit_layout(&mut self) -> Result<SaveNotice, &'static str> {
        let root = self.root.as_deref().ok_or("This tab is not in a Git worktree.")?;
        let saved = self.saved.as_ref().ok_or("Reopen Settings before changing the layout.")?;
        self.store()?.inherit_draft(root, saved)?;
        let auto_arrange = self.auto_arrange_dirty().then_some(self.layout.new_tabs);
        if let Err(why) = self.reload_layout() {
            self.saved = None;
            return Ok(Err(why));
        }
        if let Some(auto_arrange) = auto_arrange {
            self.layout.new_tabs = auto_arrange;
        }
        Ok(Ok(format!("{} now uses the global default layout.", self.worktree_subject("This worktree")).into()))
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
        let window = self.web_window.as_deref();
        let mut layout = store.capture(target, window)?;
        if window.is_none() {
            self.tab_size = store.tab_size(target, None).or(self.tab_size);
        }
        layout.new_tabs = self.layout.new_tabs;
        self.layout = layout;
        Ok("Captured Milo panes from this tab. Nothing was saved.")
    }

    fn refresh_preview(&mut self) -> Result<&'static str, &'static str> {
        self.tab_size = self.store()?.tab_size(self.target()?, self.web_window.as_deref());
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
        let trial = self.store()?.trial(&self.layout, self.target()?, self.web_window.as_deref())?;
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
            Row::UseGlobalPrefs => self.prefs_scope() != Source::Worktree,
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
            | Row::UsageTool(_)
            | Row::FollowProject
            | Row::FollowActivity
            | Row::ChangesOnly
            | Row::HistoryFollow
            | Row::TabTitles => self.saved_prefs.is_none(),
            Row::ClaudeLimits | Row::SaveClaude => !self.claude.editable(),
            Row::MenuIndicator => self.menu_installed.is_none() || self.store.is_none(),
            Row::Presence | Row::PresenceDetails | Row::PresenceApp => self.presence_view.is_none(),
            Row::Continue if self.panel == Panel::Confirm(Confirm::MirrorAlerts) => !self.visible.alert_review_complete,
            Row::Continue if self.panel == Panel::Confirm(Confirm::PresenceApp) => {
                presence::application_id(&self.application_draft).is_none()
            }
            Row::Repair | Row::Remove => self.trial.is_some(),
            _ => false,
        }
    }

    fn activate(&mut self, row: Row) -> After {
        if self.disabled(row) {
            return After::Ignored;
        }
        let result = match row {
            Row::Capture => self.capture().map(Cow::Borrowed),
            Row::Undo => self.undo().map(Cow::Borrowed),
            Row::SaveClaude => self.save_claude().map(Cow::Borrowed),
            Row::MenuBar if self.menu_installed.is_some() => {
                self.change_menu_app(MenuAction::Install);
                return After::Stay;
            }
            Row::MenuIndicator => {
                let result = self
                    .store()
                    .map_err(str::to_owned)
                    .and_then(|store| menubar::Indicator::advance(&store.config, self.menu_indicator));
                self.reload_menu_indicator();
                self.report_text(result.map(Cow::Borrowed).map_err(Cow::Owned));
                return After::Stay;
            }
            Row::RemoveMenuBar if self.menu_installed == Some(true) => {
                self.select_panel(Panel::Confirm(Confirm::RemoveMenuBar), None);
                return After::Stay;
            }
            Row::MenuBar | Row::RemoveMenuBar => return After::Ignored,
            Row::Alerts => {
                self.select_panel(Panel::Alerts, None);
                return After::Stay;
            }
            Row::AlertsOn | Row::AlertsDestination | Row::AlertKind(_) | Row::AlertsContent | Row::AlertsTest => {
                self.activate_alert(row);
                return After::Stay;
            }
            Row::PresenceOpen => {
                self.select_panel(Panel::Presence, None);
                return After::Stay;
            }
            Row::Presence => {
                self.choose_presence();
                return After::Stay;
            }
            Row::PresenceDetails => {
                self.choose_presence_details();
                return After::Stay;
            }
            Row::PresenceApp => {
                self.choose_presence_application();
                return After::Stay;
            }
            Row::PresenceId => {
                self.application_draft.clear();
                self.show_application_draft();
                return After::Stay;
            }
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
                Panel::Confirm(Confirm::RemoveMenuBar) => {
                    self.change_menu_app(MenuAction::Uninstall);
                    return After::Stay;
                }
                Panel::Confirm(Confirm::MirrorAlerts) => {
                    let scope = alerts::ConsentFor::All;
                    // Changing panels refreshes the summary. Use the one the question actually showed.
                    let reviewed = self.alert_summary.as_ref().and_then(|summary| summary.consent.token(scope));
                    self.select_panel(Panel::Alerts, Some(Row::AlertsOn));
                    self.change_alerts(|alerts| {
                        let token = reviewed.ok_or("No saved destination can be reviewed. Nothing was turned on.")?;
                        alerts.enable_reviewed(scope, token)
                    });
                    return After::Stay;
                }
                Panel::Confirm(confirm @ (Confirm::Presence | Confirm::PresenceDetails(_) | Confirm::PresenceApp)) => {
                    return self.confirm_presence(confirm);
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
                    Row::Tune => self.select_panel(Panel::Tune, None),
                    Row::SaveLayout => self.open_save(SaveKind::Layout),
                    Row::UseGlobal => self.select_panel(Panel::Confirm(Confirm::UseGlobal), None),
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
                        cycle(&mut self.layout.outline_share, OUTLINE_SHARES);
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
                        if row == Row::SavePrefs {
                            self.open_save(kind);
                        } else {
                            self.select_panel(Panel::Confirm(Confirm::UseGlobalPrefs(kind)), None);
                        }
                    }
                    Row::Diagnostics => self.select_panel(Panel::Diagnostics, None),
                    Row::Repair => self.select_panel(Panel::Confirm(Confirm::Repair), None),
                    Row::Remove => self.select_panel(Panel::Confirm(Confirm::Remove), None),
                    _ => self.change_pref(row),
                }
                return After::Stay;
            }
        };
        self.report_text(result.map_err(Cow::Borrowed));
        After::Stay
    }
}

struct WorktreeName<'a>(Option<&'a str>, &'static str);

impl std::fmt::Display for WorktreeName<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(name) => write!(f, "“{name}”"),
            None => f.write_str(self.1),
        }
    }
}

fn same_geometry(first: &Layout, second: &Layout) -> bool {
    first.panes == second.panes
        && first.width == second.width
        && first.geometry == second.geometry
        && first.outline_share == second.outline_share
        && first.usage_rows == second.usage_rows
}

fn prefs_write_error(error: io::Error, fallback: &'static str) -> &'static str {
    if error.kind() == io::ErrorKind::Interrupted { PREFS_CHANGED } else { fallback }
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

    fn external_fd(&self) -> Option<RawFd> {
        self.diagnostics.fd()
    }

    fn check_external(&mut self) -> External {
        match self.diagnostics.collect() {
            Some(Ok(report) | Err(report)) => {
                self.health = report;
                self.health_top = 0;
                External::Redraw
            }
            None => External::Unchanged,
        }
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
            key("↑↓   ", "move (k j)"),
            key("Enter", "choose or change"),
            key("Click", "choose or change"),
            key("1-4  ", "switch sections"),
            key("Tab  ", "next section"),
        ];
        if self.inside {
            rows.extend([key("a    ", "try in this tab"), key("u    ", "undo trial")]);
        }
        rows.extend([
            key("s    ", "save, asks where"),
            key("r    ", "refresh this view"),
            key("Esc  ", "back or leave"),
            key("q    ", "leave settings"),
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
            self.select_panel(panel, None);
            return After::Stay;
        }
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
        // A worktree save can redraw the same save panel with a global choice in its place.
        if after == After::Stay && self.panel.modal() {
            self.modal_click.opened(x, y);
        }
        after
    }

    fn input(&mut self, key: Key) -> After {
        let modal = self.panel.modal();
        if let Some(rows) = ui::selection_delta(key, self.visible.len) {
            let (current, limit) = if self.panel == Panel::Diagnostics {
                (&mut self.health_top, self.visible.scroll_limit)
            } else {
                let limit = self.rows().len().saturating_sub(1);
                (&mut self.selected, limit)
            };
            let next = current.saturating_add_signed(rows).min(limit);
            if next == *current {
                return After::Ignored;
            }
            *current = next;
            return After::Stay;
        }
        let action = match key {
            Key::Enter | Key::Char(' ') => self.selected_row(),
            Key::Click(x, y) => return self.click(x, y),
            Key::Ctrl(b'i') if !modal => {
                self.next_panel();
                return After::Stay;
            }
            Key::Char(digit @ '0'..='9') if self.panel == Panel::Confirm(Confirm::PresenceApp) => {
                self.type_application_digit(digit);
                return After::Stay;
            }
            Key::Char(digit @ '1'..='4') if !modal => {
                let panel = SECTIONS[usize::from(digit as u8 - b'1')];
                self.select_panel(panel, None);
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
            Key::Char('r') if self.panel == Panel::Alerts => {
                self.refresh_alerts();
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
            Key::Esc if matches!(self.panel, Panel::Tune | Panel::Alerts | Panel::Presence | Panel::Diagnostics) => {
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
            crate::alerts::resume();
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
