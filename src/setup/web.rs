//! The local browser edits a versioned draft; only explicit saves touch settings.

use std::fmt::{self, Display, Write as _};

use super::claude::ClaudeSnapshot;
use super::preview::Preview;
use super::*;
use crate::json::{self, Value};
use crate::preferences::PreferenceSnapshot;

const BAD_ACTION: &str = "Unknown or invalid Settings action.";
const UNAVAILABLE: &str = "That choice is unavailable in this Settings session.";
const LOST_TAB: &str = "The original Rex tab is no longer available. Reopen Settings in the tab you want to change.";
const PREFS_CHANGED: &str = "Milo's pane preferences changed. Reopen Settings before saving again.";
const PREFS_READ: &str = "Could not read Milo's pane preferences. They were left alone.";

impl Display for doctor::Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (level, note) = match self {
            Self::Good => ("good", ""),
            Self::Warning(reason) => ("warning", *reason),
            Self::Issue(reason) => ("issue", *reason),
        };
        write!(f, "{{\"level\":\"{level}\",\"note\":{}}}", Quoted(note))
    }
}

pub(crate) struct Editor {
    setup: Setup,
    preferences: Option<PreferenceSnapshot>,
    claude: Option<ClaudeSnapshot>,
    doctor: Option<doctor::Health>,
    version: u64,
    directory: Option<String>,
}

impl Editor {
    pub(crate) fn open() -> Self {
        let inside = rex::inside();
        let store = Store::real();
        let target = inside.then(rex::Target::current).flatten();
        let anchor = store.as_ref().zip(target.as_ref()).and_then(|(store, target)| store.anchor_tab(target));
        let (root, window, size) = if inside {
            anchor.map_or((None, None, None), |(window, root, size)| (root, Some(window), size))
        } else {
            (crate::preferences::worktree(Path::new(".")), None, None)
        };
        Self::new(store, inside, target, root, window, size, rex::run)
    }

    fn new(
        store: Option<Store>,
        inside: bool,
        target: Option<rex::Target>,
        root: Option<PathBuf>,
        window: Option<String>,
        size: Option<(usize, usize)>,
        run: fn(&[&OsStr]) -> bool,
    ) -> Self {
        let (status, claude) = account_at(store.as_ref());
        let mut setup = Setup::new_with_claude(store, inside, target, root, run, status);
        setup.web_window = window;
        setup.tab_size = size;
        if !inside && let Err(why) = setup.reload_layout() {
            setup.report(Err(why));
        }
        if inside && setup.web_window.is_none() && setup.message.is_none() {
            setup.report(Err(LOST_TAB));
        }
        let directory = std::env::current_dir().ok().map(|path| path.to_string_lossy().into_owned());
        let mut editor = Self { setup, preferences: None, claude, doctor: None, version: 1, directory };
        if let Err(why) = editor.load_preferences() {
            editor.setup.report(Err(why));
        }
        editor
    }

    fn load_preferences(&mut self) -> Result<(), &'static str> {
        self.preferences = None;
        self.setup.saved_prefs = None;
        let snapshot = PreferenceSnapshot::load(&self.setup.store()?.config, self.setup.root.as_deref())
            .map_err(|_| PREFS_READ)?;
        self.setup.prefs = snapshot.prefs;
        self.setup.saved_prefs = Some(snapshot.prefs);
        self.setup.prefs_scope = snapshot.source;
        self.preferences = Some(snapshot);
        Ok(())
    }

    fn load_account(&mut self) {
        let (status, snapshot) = account_at(self.setup.store.as_ref());
        self.setup.claude = status;
        self.setup.show_limits = status == Claude::On;
        self.claude = snapshot;
    }

    fn layout_editable(&self) -> bool {
        self.setup.saved.is_some()
    }

    fn live(&self) -> bool {
        self.setup.web_window.is_some()
    }

    fn require_tab(&self) -> Result<(), &'static str> {
        let setup = &self.setup;
        let window = setup.web_window.as_deref().ok_or(LOST_TAB)?;
        if setup.store()?.tab_matches(setup.target()?, window) { Ok(()) } else { Err(LOST_TAB) }
    }

    fn groups(&self) -> (GroupState, GroupState, GroupState) {
        let setup = &self.setup;
        let layout_editable = self.layout_editable();
        let layout_save = layout_editable && (!setup.inside || self.live());
        let global_readable = setup.saved_global.as_ref().is_none_or(Result::is_ok);
        let preferences_editable = self.preferences.is_some();
        let account_editable = setup.claude.editable() && self.claude.is_some();
        (
            GroupState {
                scope: if setup.scope == Scope::Global { "global" } else { "worktree" },
                dirty: setup.layout_dirty(),
                editable: layout_editable,
                save_global: layout_save && global_readable,
                save_worktree: layout_save && setup.root.is_some() && !setup.only_auto_arrange_dirty(),
                inherit: layout_save && setup.scope == Scope::Worktree && global_readable && setup.trial.is_none(),
                save_account: false,
            },
            GroupState {
                scope: match setup.prefs_scope {
                    Source::Default => "default",
                    Source::Global => "global",
                    Source::Worktree => "worktree",
                },
                dirty: setup.prefs_dirty(),
                editable: preferences_editable,
                save_global: preferences_editable,
                save_worktree: preferences_editable && setup.root.is_some(),
                inherit: preferences_editable && setup.prefs_scope == Source::Worktree,
                save_account: false,
            },
            GroupState {
                scope: "account",
                dirty: setup.claude_dirty(),
                editable: account_editable,
                save_global: false,
                save_worktree: false,
                inherit: false,
                save_account: account_editable,
            },
        )
    }

    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    pub(crate) fn state(&self) -> String {
        let setup = &self.setup;
        let layout = &setup.layout;
        let prefs = &setup.prefs;
        let (width, [outline, history, usage], size) = Preview::new(layout, setup.tab_size).proportions();
        let (cols, rows) = size.unzip();
        let [show_outline, show_history, show_usage] = layout.panes;
        let [first, second, third] = layout.geometry.order.map(TabPane::name);
        let (layout_state, prefs_state, account_state) = self.groups();
        let side = if layout.geometry.side == Side::Left { "left" } else { "right" };
        let icons = match prefs.icons {
            IconMode::Nerd => "nerd",
            IconMode::Plain => "plain",
            IconMode::Off => "off",
        };
        let project = if prefs.project_mode == ProjectMode::Follow { "follow" } else { "pin" };
        let status = match setup.claude {
            Claude::On => "Managed by Milo",
            Claude::Off => "Not enabled",
            Claude::Foreign => "A user-managed status line is in use",
            Claude::NotOwned => "No matching ownership note; left alone",
            Claude::Invalid => "Claude Code settings could not be read",
        };
        let live = self.live();
        let trial = setup.trial.is_some();
        let capture = live && !trial && layout_state.editable;
        let try_layout = capture && layout.panes.contains(&true);
        let auto_arrange = live && layout_state.editable;
        let root = setup.root.as_deref().map(Path::to_string_lossy);
        let mut out = String::with_capacity(1500);
        write!(out, concat!(
            "{{\"version\":{},\"app_version\":\"", env!("CARGO_PKG_VERSION"), "\",",
            "\"context\":{{\"inside_rex\":{},\"worktree\":{},\"directory\":{},\"installed\":{}}},",
            "\"layout\":{{\"panes\":{{\"outline\":{},\"history\":{},\"usage\":{}}},\"side\":\"{}\",\"width\":{},",
            "\"order\":[\"{}\",\"{}\",\"{}\"],\"outline_share\":{},\"usage_rows\":{},\"ratios\":{},\"auto_arrange\":{}}},",
            "\"preview\":{{\"width\":{},\"shares\":[{},{},{}],\"cols\":{},\"rows\":{}}},",
            "\"appearance\":{{\"icons\":\"{}\",\"static_marks\":{},\"text_git_marks\":{},\"usage_large_figures\":{}}},",
            "\"workflow\":{{\"project_mode\":\"{}\",\"follow_activity\":{},\"changes_only\":{},\"history_follow_new\":{}}},",
            "\"account\":{{\"claude_limits\":{},\"claude_status\":{}}},",
            "\"groups\":{{\"layout\":{},\"preferences\":{},\"account\":{}}},",
            "\"capabilities\":{{\"capture\":{},\"try\":{},\"undo\":{},\"refresh\":{},\"auto_arrange\":{}}},",
            "\"dirty\":{},\"trial\":{},\"doctor\":{},\"message\":"),
            self.version, setup.inside, Optional(root.as_deref().map(Quoted)), Optional(self.directory.as_deref().map(Quoted)), setup.installed,
            show_outline, show_history, show_usage, side, layout.width.parse::<f64>().unwrap_or(0.2),
            first, second, third, layout.outline_share.parse::<f64>().unwrap_or(0.7),
            layout.usage_rows.parse::<u32>().unwrap_or(super::layout::DEFAULT_USAGE_ROWS),
            Optional(layout.geometry.shares.map(Ratios)), layout.new_tabs,
            width, outline, history, usage, Optional(cols), Optional(rows),
            icons, prefs.static_marks, prefs.text_git_marks, prefs.usage_large_figures,
            project, prefs.follow_activity, prefs.changes_only, prefs.history_follow_new,
            setup.show_limits, Quoted(status), layout_state, prefs_state, account_state,
            capture, try_layout, trial, live, auto_arrange, setup.dirty(), trial,
            Optional(self.doctor.as_ref()),
        ).expect("writing to a string");
        if let Some((text, style)) = &setup.message {
            write!(out, "{{\"text\":{},\"error\":{}}}", Quoted(text), *style == DEL.bold())
                .expect("writing to a string");
        } else {
            out.push_str("null");
        }
        out.push('}');
        out
    }

    pub(crate) fn action(&mut self, body: &[u8]) -> Result<String, String> {
        let (version, action) = Action::parse(body).ok_or(BAD_ACTION)?;
        if version != self.version {
            return Err("This draft changed in another browser request. Reload the editor state and try again.".into());
        }
        let message = self.apply(action)?;
        self.setup.message = None;
        if let Some(message) = message {
            self.setup.report(message);
        }
        self.version += 1;
        Ok(self.state())
    }

    fn apply(&mut self, action: Action) -> Result<Option<Notice>, String> {
        let (layout, preferences, account) = self.groups();
        let notice = match action {
            Action::Set(edit) => {
                let editable = match edit.group() {
                    Group::Layout => layout.editable,
                    Group::Preferences => preferences.editable,
                    Group::Account => account.editable,
                    Group::All => unreachable!("edits affect one group"),
                };
                if !editable || (matches!(edit, Edit::AutoArrange(_)) && !self.live()) {
                    return Err(UNAVAILABLE.into());
                }
                edit.apply(&mut self.setup);
                None
            }
            Action::Preset(tree) if layout.editable => {
                self.setup.preset(tree);
                None
            }
            Action::Discard(group) => {
                self.discard(group)?;
                Some(Ok("Draft changes discarded. Saved settings were left alone."))
            }
            Action::Save(Group::Layout, WriteScope::Global) if layout.save_global => {
                Some(self.save_layout(Scope::Global)?)
            }
            Action::Save(Group::Layout, WriteScope::Worktree) if layout.save_worktree => {
                Some(self.save_layout(Scope::Worktree)?)
            }
            Action::Save(Group::Preferences, WriteScope::Global) if preferences.save_global => {
                Some(self.save_preferences(false)?)
            }
            Action::Save(Group::Preferences, WriteScope::Worktree) if preferences.save_worktree => {
                Some(self.save_preferences(true)?)
            }
            Action::Save(Group::Account, WriteScope::Account) if account.save_account => Some(self.save_account()?),
            Action::Inherit(Group::Layout) if layout.inherit => Some(self.inherit_layout()?),
            Action::Inherit(Group::Preferences) if preferences.inherit => Some(self.inherit_preferences()?),
            Action::Capture if self.live() && layout.editable && self.setup.trial.is_none() => {
                Some(Ok(self.setup.capture()?))
            }
            Action::Try if self.live() && layout.editable => {
                self.setup.try_layout()?;
                self.doctor = None;
                Some(Ok(if self.setup.trial.as_ref().is_some_and(|(trial, _)| trial.temporary()) {
                    "Previewing a temporary rail in the original Rex tab. Existing panes are untouched."
                } else {
                    "Trying this layout in the original Rex tab. Nothing was saved."
                }))
            }
            Action::Undo if self.setup.trial.is_some() => {
                let message = self.setup.undo()?;
                self.doctor = None;
                Some(Ok(message))
            }
            Action::Refresh if self.live() => Some(self.setup.refresh_preview()),
            Action::Doctor => {
                self.doctor = None;
                let setup = &self.setup;
                let target = if setup.inside { self.require_tab().map(|()| setup.target.as_ref()) } else { Ok(None) };
                self.doctor = Some(doctor::health_anchored(setup.store()?, setup.root.as_deref(), target)?);
                None
            }
            _ => return Err(UNAVAILABLE.into()),
        };
        Ok(notice)
    }

    fn discard(&mut self, group: Group) -> Result<(), &'static str> {
        let setup = &mut self.setup;
        if matches!(group, Group::Layout | Group::All) {
            if setup.trial.is_some() {
                setup.undo()?;
                self.doctor = None;
            }
            if let Some(saved) = &setup.saved {
                setup.layout.clone_from(saved);
            }
        }
        if matches!(group, Group::Preferences | Group::All)
            && let Some(saved) = setup.saved_prefs
        {
            setup.prefs = saved;
        }
        if matches!(group, Group::Account | Group::All) {
            setup.show_limits = setup.claude == Claude::On;
        }
        Ok(())
    }

    fn save_layout(&mut self, scope: Scope) -> Result<Notice, &'static str> {
        let inside = self.setup.inside;
        if inside {
            self.require_tab()?;
        } else if !self.setup.layout_dirty() {
            return Ok(Ok("No layout changes to save."));
        }
        let notice = self.setup.persist_layout(scope, inside)?;
        self.doctor = None;
        Ok(if inside {
            notice
        } else {
            notice.map(|_| match (scope, self.setup.scope) {
                (Scope::Worktree, _) => "Saved this worktree's layout without changing Rex integration.",
                (Scope::Global, Scope::Worktree) => "Saved the global layout. This worktree still uses its own layout.",
                (Scope::Global, _) => "Saved the global layout without changing Rex integration.",
            })
        })
    }

    fn inherit_layout(&mut self) -> Result<Notice, &'static str> {
        if self.setup.inside {
            self.require_tab()?;
        }
        let setup = &mut self.setup;
        let saved = setup.saved.as_ref().ok_or(UNAVAILABLE)?;
        let root = setup.root.as_deref().ok_or(UNAVAILABLE)?;
        setup.store()?.inherit_draft(root, saved)?;
        self.doctor = None;
        let auto = setup.auto_arrange_dirty().then_some(setup.layout.new_tabs);
        if let Err(why) = setup.reload_layout() {
            setup.saved = None;
            return Ok(Err(why));
        }
        if let Some(auto) = auto {
            setup.layout.new_tabs = auto;
        }
        Ok(Ok("This worktree now uses the global layout."))
    }

    fn save_preferences(&mut self, local: bool) -> Result<Notice, &'static str> {
        let setup = &self.setup;
        if !setup.prefs_dirty() {
            return Ok(Ok("No pane preference changes to save."));
        }
        let snapshot = self.preferences.as_ref().ok_or(UNAVAILABLE)?;
        snapshot.save(setup.prefs, &setup.store()?.config, setup.root.as_deref(), local).map_err(prefs_error)?;
        self.doctor = None;
        Ok(self.load_preferences().map(|()| match (local, self.setup.prefs_scope) {
            (true, _) => "Saved pane preferences for this worktree. Reopen panes to apply.",
            (false, Source::Worktree) => "Saved global pane preferences. This worktree still overrides them.",
            (false, _) => "Saved global pane preferences. Reopen panes to apply.",
        }))
    }

    fn inherit_preferences(&mut self) -> Result<Notice, &'static str> {
        let setup = &self.setup;
        let snapshot = self.preferences.as_ref().ok_or(UNAVAILABLE)?;
        let root = setup.root.as_deref().ok_or(UNAVAILABLE)?;
        snapshot.inherit(&setup.store()?.config, root).map_err(prefs_error)?;
        self.doctor = None;
        Ok(self.load_preferences().map(|()| "This worktree now uses global pane preferences. Reopen panes to apply."))
    }

    fn save_account(&mut self) -> Result<Notice, &'static str> {
        let setup = &self.setup;
        if !setup.claude_dirty() {
            return Ok(Ok("No Claude Code changes to save."));
        }
        let expected = self.claude.as_ref().ok_or(UNAVAILABLE)?;
        setup.store()?.save_claude_draft(setup.show_limits, expected)?;
        self.doctor = None;
        self.load_account();
        if self.claude.is_some() {
            Ok(Ok("Saved Claude Code choices."))
        } else {
            Ok(Err("Saved, but could not read Claude Code settings."))
        }
    }
}

type Notice = Result<&'static str, &'static str>;

fn account_at(store: Option<&Store>) -> (Claude, Option<ClaudeSnapshot>) {
    store
        .and_then(|store| store.claude_snapshot().ok())
        .map_or((Claude::Invalid, None), |(status, snapshot)| (status, Some(snapshot)))
}

fn prefs_error(error: io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::Interrupted { PREFS_CHANGED } else { "Could not save Milo's pane preferences." }
}

#[derive(Clone, Copy)]
enum Group {
    Layout,
    Preferences,
    Account,
    All,
}

enum WriteScope {
    Global,
    Worktree,
    Account,
}

enum Action {
    Set(Edit),
    Preset(bool),
    Save(Group, WriteScope),
    Inherit(Group),
    Discard(Group),
    Capture,
    Try,
    Undo,
    Refresh,
    Doctor,
}

impl Action {
    fn parse(body: &[u8]) -> Option<(u64, Self)> {
        let names = ["action", "version", "field", "value", "group", "scope", "preset"];
        let mut fields: [Option<Value<'_>>; 7] = [None; 7];
        let mut valid = true;
        json::settings_object(std::str::from_utf8(body).ok()?, |member| {
            if let Some(index) = names.iter().position(|name| member.key.is(name)) {
                valid &= fields[index].replace(member.value).is_none();
            } else {
                valid = false;
            }
        })?;
        let [Some(action), Some(version), field, value, group, scope, preset] = fields else { return None };
        if !valid {
            return None;
        }
        let command = match [field, value, group, scope, preset] {
            [Some(field), Some(value), None, None, None] if action.is("set") => Self::Set(Edit::parse(field, value)?),
            [None, None, None, None, Some(preset)] if action.is("preset") => {
                if preset.is("tree") {
                    Self::Preset(true)
                } else if preset.is("sidebar") {
                    Self::Preset(false)
                } else {
                    return None;
                }
            }
            [None, None, Some(group), Some(scope), None] if action.is("save") => {
                match (Group::parse(group)?, scope.string(8)?.as_ref()) {
                    (group @ (Group::Layout | Group::Preferences), "global") => Self::Save(group, WriteScope::Global),
                    (group @ (Group::Layout | Group::Preferences), "worktree") => {
                        Self::Save(group, WriteScope::Worktree)
                    }
                    (Group::Account, "account") => Self::Save(Group::Account, WriteScope::Account),
                    _ => return None,
                }
            }
            [None, None, Some(group), None, None] if action.is("inherit") => match Group::parse(group)? {
                group @ (Group::Layout | Group::Preferences) => Self::Inherit(group),
                _ => return None,
            },
            [None, None, Some(group), None, None] if action.is("discard") => Self::Discard(Group::parse(group)?),
            [None, None, None, None, None] => match action.string(7)?.as_ref() {
                "capture" => Self::Capture,
                "try" => Self::Try,
                "undo" => Self::Undo,
                "refresh" => Self::Refresh,
                "doctor" => Self::Doctor,
                _ => return None,
            },
            _ => return None,
        };
        Some((version.unsigned()?, command))
    }
}

impl Group {
    fn parse(value: Value<'_>) -> Option<Self> {
        Some(match value.string(11)?.as_ref() {
            "layout" => Self::Layout,
            "preferences" => Self::Preferences,
            "account" => Self::Account,
            "all" => Self::All,
            _ => return None,
        })
    }
}

enum Edit {
    Pane(TabPane, bool),
    Side(Side),
    Width(f64),
    Order([TabPane; 3]),
    OutlineShare(f64),
    UsageRows(u32),
    Ratios(Option<[u32; 3]>),
    AutoArrange(bool),
    Icons(IconMode),
    StaticMarks(bool),
    TextGitMarks(bool),
    UsageLargeFigures(bool),
    ProjectMode(ProjectMode),
    FollowActivity(bool),
    ChangesOnly(bool),
    HistoryFollowNew(bool),
    ClaudeLimits(bool),
}

impl Edit {
    fn parse(field: Value<'_>, value: Value<'_>) -> Option<Self> {
        let fraction = || value.number().filter(|n| 0.0 < *n && *n < 1.0);
        Some(match field.string(32)?.as_ref() {
            "layout.side" => Self::Side(match value.string(5)?.as_ref() {
                "left" => Side::Left,
                "right" => Side::Right,
                _ => return None,
            }),
            "layout.width" => Self::Width(fraction()?),
            "layout.order" => {
                let order = triplet(value, |value| pane_named(value.string(7)?.as_ref()))?;
                let [a, b, c] = order;
                if a == b || b == c || c == a {
                    return None;
                }
                Self::Order(order)
            }
            "layout.outline_share" => Self::OutlineShare(fraction()?),
            "layout.usage_rows" => Self::UsageRows(value.unsigned()?.try_into().ok().filter(|&rows| rows > 0)?),
            "layout.ratios" if value.null() => Self::Ratios(None),
            "layout.ratios" => {
                let ratios = triplet(value, |value| value.unsigned().filter(|n| *n <= 1_000_000)?.try_into().ok())?;
                if ratios.iter().sum::<u32>() != 1_000_000 {
                    return None;
                }
                Self::Ratios(Some(ratios))
            }
            "layout.auto_arrange" => Self::AutoArrange(value.boolean()?),
            "appearance.icons" => Self::Icons(match value.string(5)?.as_ref() {
                "nerd" => IconMode::Nerd,
                "plain" => IconMode::Plain,
                "off" => IconMode::Off,
                _ => return None,
            }),
            "appearance.static_marks" => Self::StaticMarks(value.boolean()?),
            "appearance.text_git_marks" => Self::TextGitMarks(value.boolean()?),
            "appearance.usage_large_figures" => Self::UsageLargeFigures(value.boolean()?),
            "workflow.project_mode" => Self::ProjectMode(match value.string(6)?.as_ref() {
                "follow" => ProjectMode::Follow,
                "pin" => ProjectMode::Pin,
                _ => return None,
            }),
            "workflow.follow_activity" => Self::FollowActivity(value.boolean()?),
            "workflow.changes_only" => Self::ChangesOnly(value.boolean()?),
            "workflow.history_follow_new" => Self::HistoryFollowNew(value.boolean()?),
            "account.claude_limits" => Self::ClaudeLimits(value.boolean()?),
            field => Self::Pane(pane_named(field.strip_prefix("layout.panes.")?)?, value.boolean()?),
        })
    }

    fn group(&self) -> Group {
        match self {
            Self::Pane(..)
            | Self::Side(_)
            | Self::Width(_)
            | Self::Order(_)
            | Self::OutlineShare(_)
            | Self::UsageRows(_)
            | Self::Ratios(_)
            | Self::AutoArrange(_) => Group::Layout,
            Self::ClaudeLimits(_) => Group::Account,
            _ => Group::Preferences,
        }
    }

    fn apply(self, setup: &mut Setup) {
        let layout = &mut setup.layout;
        let prefs = &mut setup.prefs;
        match self {
            Self::Pane(role, on) => layout.panes[role as usize] = on,
            Self::Side(side) => layout.geometry.side = side,
            Self::Width(width) => set_number(&mut layout.width, width, &layout::WIDTHS.map(|width| width.proportion)),
            Self::Order(order) => layout.geometry.order = order,
            Self::OutlineShare(share) => {
                set_number(&mut layout.outline_share, share, OUTLINE_SHARES);
                layout.geometry.shares = None;
            }
            Self::UsageRows(rows) => {
                if layout.usage_rows.parse() != Ok(rows) {
                    layout.usage_rows = rows.to_string();
                }
                layout.geometry.shares = None;
            }
            Self::Ratios(ratios) => layout.geometry.shares = ratios,
            Self::AutoArrange(on) => layout.new_tabs = on,
            Self::Icons(mode) => prefs.icons = mode,
            Self::StaticMarks(on) => prefs.static_marks = on,
            Self::TextGitMarks(on) => prefs.text_git_marks = on,
            Self::UsageLargeFigures(on) => prefs.usage_large_figures = on,
            Self::ProjectMode(mode) => prefs.project_mode = mode,
            Self::FollowActivity(on) => prefs.follow_activity = on,
            Self::ChangesOnly(on) => prefs.changes_only = on,
            Self::HistoryFollowNew(on) => prefs.history_follow_new = on,
            Self::ClaudeLimits(on) => setup.show_limits = on,
        }
    }
}

fn pane_named(name: &str) -> Option<TabPane> {
    TabPane::ALL.into_iter().find(|pane| pane.name() == name)
}

fn set_number(destination: &mut String, next: f64, presets: &[&str]) {
    if destination.parse() != Ok(next) {
        destination.clear();
        if let Some(preset) = presets.iter().find(|preset| preset.parse() == Ok(next)) {
            destination.push_str(preset);
        } else {
            write!(destination, "{next}").expect("writing to a string");
        }
    }
}

fn triplet<'a, T>(value: Value<'a>, mut parse: impl FnMut(Value<'a>) -> Option<T>) -> Option<[T; 3]> {
    let mut values = value.elements()?;
    let result = [parse(values.next()?)?, parse(values.next()?)?, parse(values.next()?)?];
    values.next().is_none().then_some(result)
}

struct GroupState {
    scope: &'static str,
    dirty: bool,
    editable: bool,
    save_global: bool,
    save_worktree: bool,
    inherit: bool,
    save_account: bool,
}

impl Display for GroupState {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            out,
            concat!(
                "{{\"scope\":\"{}\",\"dirty\":{},\"editable\":{},\"save_global\":{},",
                "\"save_worktree\":{},\"inherit\":{},\"save_account\":{}}}"
            ),
            self.scope,
            self.dirty,
            self.editable,
            self.save_global,
            self.save_worktree,
            self.inherit,
            self.save_account
        )
    }
}

struct Optional<T>(Option<T>);

impl<T: Display> Display for Optional<T> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(value) => value.fmt(out),
            None => out.write_str("null"),
        }
    }
}

struct Ratios([u32; 3]);

impl Display for Ratios {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [outline, history, usage] = self.0;
        write!(out, "[{outline},{history},{usage}]")
    }
}

struct Quoted<'a>(&'a str);

impl Display for Quoted<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_char('"')?;
        for c in self.0.chars() {
            match c {
                '"' | '\\' => {
                    out.write_char('\\')?;
                    out.write_char(c)?;
                }
                '\n' => out.write_str("\\n")?,
                '\r' => out.write_str("\\r")?,
                '\t' => out.write_str("\\t")?,
                '\0'..='\u{1f}' => write!(out, "\\u{:04x}", u32::from(c))?,
                _ => out.write_char(c)?,
            }
        }
        out.write_char('"')
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::fs;

    fn fixture_store(folder: &Fixture) -> Store {
        Store {
            init: folder.root.join("init.lua"),
            config: folder.root.join("config"),
            state: folder.root.join("state"),
            claude: folder.root.join("claude.json"),
        }
    }

    fn outside(store: Store, root: Option<&Path>) -> Editor {
        Editor::new(Some(store), false, None, root.map(Path::to_path_buf), None, None, |_| {
            panic!("outside Rex must never run an integration command")
        })
    }

    impl Editor {
        pub(crate) fn test_outside(folder: &Fixture) -> Self {
            outside(fixture_store(folder), None)
        }
    }

    fn action(editor: &mut Editor, fields: &str) -> Result<String, String> {
        editor.action(format!("{{\"version\":{}, {fields}}}", editor.version).as_bytes())
    }

    fn set(editor: &mut Editor, field: &str, value: &str) -> String {
        action(editor, &format!("\"action\":\"set\",\"field\":{field:?},\"value\":{value}")).unwrap()
    }

    fn field<'a>(json: Value<'a>, name: &str) -> Value<'a> {
        json.fields([name])[0].unwrap()
    }

    #[test]
    fn layout_edits_preview_without_writing_until_a_scoped_save() {
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let root = f.root.join("it's a \"worktree\"");
        let mut editor = outside(store.clone(), Some(&root));
        let defaults = editor.state();
        let state = Value::parse(defaults.as_bytes()).unwrap();
        let context = field(state, "context");
        assert_eq!(field(context, "inside_rex").boolean(), Some(false));
        assert!(field(context, "worktree").is(root.to_str().unwrap()));
        assert_eq!(field(field(state, "capabilities"), "try").boolean(), Some(false));

        set(&mut editor, "layout.side", "\"right\"");
        set(&mut editor, "layout.width", "0.35");
        set(&mut editor, "layout.order", "[\"usage\",\"history\",\"outline\"]");
        set(&mut editor, "layout.panes.history", "false");
        set(&mut editor, "layout.outline_share", "0.6");
        set(&mut editor, "layout.usage_rows", "10");
        let state = set(&mut editor, "layout.ratios", "[750000,0,250000]");
        let parsed = Value::parse(state.as_bytes()).unwrap();
        let preview = field(parsed, "preview");
        assert_eq!(field(preview, "width").number(), Some(0.35));
        assert!(field(preview, "rows").null());
        let shares: Vec<_> = field(preview, "shares").elements().unwrap().map(|part| part.number().unwrap()).collect();
        assert_eq!(shares, [0.75, 0.0, 0.25]);
        assert!(!store.config.exists());

        action(&mut editor, r#""action":"save","group":"layout","scope":"worktree""#).unwrap();
        let (remembered, installed, scope) = store.layout_for(Some(&root)).unwrap();
        assert_eq!(scope, Scope::Worktree);
        assert!(!installed);
        assert_eq!(remembered.geometry.side, Side::Right);
        assert_eq!(remembered.geometry.order, [TabPane::Usage, TabPane::History, TabPane::Outline]);
        assert_eq!(remembered.geometry.shares, Some([750_000, 0, 250_000]));
        assert_eq!(remembered.outline_share, "0.60");
        assert_eq!(remembered.usage_rows, "10");
        assert!(!remembered.chosen(TabPane::History));
        assert!(!editor.setup.dirty());
        assert!(!store.init.exists() && !store.rex_file().exists() && !store.state.exists());

        set(&mut editor, "layout.side", "\"left\"");
        set(&mut editor, "workflow.changes_only", "true");
        action(&mut editor, r#""action":"discard","group":"layout""#).unwrap();
        assert_eq!(editor.setup.layout.geometry.side, Side::Right);
        assert!(editor.setup.prefs_dirty());
        action(&mut editor, r#""action":"inherit","group":"layout""#).unwrap();
        assert_eq!(store.layout_for(Some(&root)).unwrap().2, Scope::Global);
        assert_eq!(editor.setup.layout.geometry.side, Side::Left);
        assert!(editor.setup.prefs_dirty());
    }

    #[test]
    fn invalid_stale_and_disabled_requests_leave_the_draft_unchanged() {
        let f = Fixture::folder();
        let mut editor = outside(fixture_store(&f), None);
        let before = editor.state();
        for request in [
            r#""action":"set","field":"layout.unknown","value":0"#,
            r#""action":"set","field":"layout.width","value":1"#,
            r#""action":"set","field":"layout.width","value":"0.20""#,
            r#""action":"set","field":"layout.order","value":["usage","usage","outline"]"#,
            r#""action":"set","field":"layout.ratios","value":[600000,200000,199999]"#,
            r#""action":"set","field":"layout.usage_rows","value":0"#,
            r#""action":"set","field":"layout.usage_rows","value":4294967296"#,
            r#""action":"set","field":"appearance.static_marks","value":1"#,
            r#""action":"set","field":"layout.side","value":"left","extra":1"#,
            r#""action":"set","field":"layout.side","f\u0069eld":"layout.side","value":"right""#,
            r#""action":"set","field":"layout.auto_arrange","value":true"#,
            r#""action":"save","group":"account","scope":"global""#,
            r#""action":"try""#,
            r#""action":"capture""#,
            r#""action":"undo""#,
            r#""action":"refresh""#,
            r#""action":"remove""#,
        ] {
            assert!(action(&mut editor, request).is_err(), "{request}");
            assert_eq!(editor.state(), before, "{request}");
        }
        let old_version = editor.version;
        let after = set(&mut editor, "layout.side", "\"right\"");
        assert!(
            editor
                .action(format!("{{\"version\":{old_version},\"action\":\"preset\",\"preset\":\"tree\"}}").as_bytes())
                .is_err()
        );
        assert_eq!(editor.state(), after);
        assert!(!editor.setup.store().unwrap().config.exists());
    }

    #[test]
    fn stale_layout_save_and_inherit_cannot_replace_a_newer_profile() {
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let root = &f.root;
        let mut a = outside(store.clone(), Some(root));
        let mut b = outside(store.clone(), Some(root));
        set(&mut a, "layout.width", "0.4");
        action(&mut a, r#""action":"save","group":"layout","scope":"worktree""#).unwrap();
        let mut stale_override = outside(store.clone(), Some(root));

        set(&mut b, "layout.width", "0.5");
        let latest = b.state();
        assert!(
            action(&mut b, r#""action":"save","group":"layout","scope":"worktree""#).unwrap_err().contains("changed")
        );
        assert_eq!(b.state(), latest);
        set(&mut a, "layout.width", "0.6");
        action(&mut a, r#""action":"save","group":"layout","scope":"worktree""#).unwrap();
        assert!(action(&mut stale_override, r#""action":"inherit","group":"layout""#).unwrap_err().contains("changed"));
        assert_eq!(store.layout_for(Some(root)).unwrap().0.width, "0.6");
    }

    #[test]
    fn preferences_share_scope_and_guard_stale_saves_and_inheritance() {
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let root = &f.root;
        let mut first = outside(store.clone(), Some(root));
        let mut stale = outside(store.clone(), Some(root));
        set(&mut first, "appearance.icons", "\"off\"");
        set(&mut first, "workflow.project_mode", "\"pin\"");
        action(&mut first, r#""action":"save","group":"preferences","scope":"worktree""#).unwrap();
        assert_eq!(first.setup.prefs_scope, Source::Worktree);
        assert!(!first.setup.prefs_dirty());

        set(&mut stale, "appearance.static_marks", "true");
        assert_eq!(
            action(&mut stale, r#""action":"save","group":"preferences","scope":"worktree""#).unwrap_err(),
            PREFS_CHANGED
        );
        let mut newer = outside(store.clone(), Some(root));
        set(&mut newer, "workflow.changes_only", "true");
        action(&mut newer, r#""action":"save","group":"preferences","scope":"worktree""#).unwrap();
        assert_eq!(action(&mut first, r#""action":"inherit","group":"preferences""#).unwrap_err(), PREFS_CHANGED);
        let kept = PreferenceSnapshot::load(&store.config, Some(root)).unwrap();
        assert_eq!(kept.prefs.icons, IconMode::Off);
        assert_eq!(kept.prefs.project_mode, ProjectMode::Pin);
        assert!(kept.prefs.changes_only);

        action(&mut newer, r#""action":"inherit","group":"preferences""#).unwrap();
        assert_eq!(newer.setup.prefs_scope, Source::Default);
        assert_eq!(newer.setup.prefs, Preferences::default());
    }

    #[test]
    fn account_saves_are_separate_and_never_overwrite_a_changed_status_line() {
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let mut editor = outside(store.clone(), None);
        set(&mut editor, "account.claude_limits", "true");
        set(&mut editor, "workflow.history_follow_new", "true");
        assert!(!store.claude.exists());
        let foreign = r#"{"statusLine":{"type":"command","command":"user-status"}}"#;
        fs::write(&store.claude, foreign).unwrap();
        let before = editor.state();
        assert!(
            action(&mut editor, r#""action":"save","group":"account","scope":"account""#)
                .unwrap_err()
                .contains("changed")
        );
        assert_eq!(editor.state(), before);
        assert_eq!(fs::read_to_string(&store.claude).unwrap(), foreign);
        let mut readonly = outside(store.clone(), None);
        assert!(action(&mut readonly, r#""action":"set","field":"account.claude_limits","value":true"#).is_err());

        fs::remove_file(&store.claude).unwrap();
        action(&mut editor, r#""action":"save","group":"account","scope":"account""#).unwrap();
        assert!(store.claude() == Claude::On);
        assert!(editor.setup.prefs_dirty());
        assert!(!store.config.exists() && !store.init.exists());
    }

    #[test]
    fn a_rex_launch_without_the_original_target_may_preview_but_cannot_install() {
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let mut editor = Editor::new(Some(store.clone()), true, None, None, None, None, |_| {
            panic!("a lost original tab must not perform integration commands")
        });
        set(&mut editor, "layout.width", "0.4");
        assert!(action(&mut editor, r#""action":"save","group":"layout","scope":"global""#).is_err());
        assert!(action(&mut editor, r#""action":"try""#).is_err());
        assert!(!store.config.exists() && !store.init.exists() && !store.state.exists());
    }

    #[test]
    fn browser_health_distinguishes_a_lost_rex_tab_from_an_offline_launch() {
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let mut offline = outside(store.clone(), None);
        let checked = action(&mut offline, r#""action":"doctor""#).unwrap();
        let verdict = field(Value::parse(checked.as_bytes()).unwrap(), "doctor");
        assert!(field(verdict, "level").is("good"));

        let mut lost = Editor::new(Some(store.clone()), true, None, None, None, None, |_| {
            panic!("Health must not change the integration")
        });
        let checked = action(&mut lost, r#""action":"doctor""#).unwrap();
        let verdict = field(Value::parse(checked.as_bytes()).unwrap(), "doctor");
        assert!(field(verdict, "level").is("warning"));
        assert!(field(verdict, "note").is(LOST_TAB));

        fs::write(&store.claude, "{").unwrap();
        let checked = action(&mut lost, r#""action":"doctor""#).unwrap();
        let verdict = field(Value::parse(checked.as_bytes()).unwrap(), "doctor");
        assert!(field(verdict, "level").is("issue"));
        assert!(field(verdict, "note").is("Milo could not read Claude Code’s settings."));
    }

    #[test]
    fn saved_changes_invalidate_browser_health_but_draft_edits_do_not() {
        let f = Fixture::folder();
        let mut editor = outside(fixture_store(&f), Some(&f.root));
        for (draft, operation) in [
            (Some(("layout.width", "0.4")), r#""action":"save","group":"layout","scope":"worktree""#),
            (None, r#""action":"inherit","group":"layout""#),
            (Some(("appearance.static_marks", "true")), r#""action":"save","group":"preferences","scope":"worktree""#),
            (None, r#""action":"inherit","group":"preferences""#),
            (Some(("account.claude_limits", "true")), r#""action":"save","group":"account","scope":"account""#),
        ] {
            action(&mut editor, r#""action":"doctor""#).unwrap();
            if let Some((field, value)) = draft {
                set(&mut editor, field, value);
            }
            assert!(action(&mut editor, r#""action":"try""#).is_err());
            assert!(editor.doctor.is_some(), "draft edits and rejected actions must not clear Health");
            let saved = action(&mut editor, operation).unwrap();
            assert!(field(Value::parse(saved.as_bytes()).unwrap(), "doctor").null(), "{operation}");
        }
    }

    #[test]
    fn failed_browser_health_rerun_clears_the_previous_verdict() {
        let f = Fixture::folder();
        let mut editor = outside(fixture_store(&f), None);
        action(&mut editor, r#""action":"doctor""#).unwrap();
        assert!(editor.doctor.is_some());

        editor.setup.store = None;
        assert!(action(&mut editor, r#""action":"doctor""#).is_err());
        let state = editor.state();
        assert!(field(Value::parse(state.as_bytes()).unwrap(), "doctor").null());
    }
}
