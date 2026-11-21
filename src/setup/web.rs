//! The local browser edits a versioned draft and applies explicit changes to global integrations.

use std::fmt::{self, Display, Write as _};

use super::claude::ClaudeSnapshot;
use super::*;
use crate::json::{self, Value};
use crate::menubar;
use crate::preferences::{Show, UsageAccount};

const BAD_ACTION: &str = "Unknown or invalid Settings action.";
const UNAVAILABLE: &str = "That choice is unavailable in this Settings session.";
const LOST_TAB: &str = "The original Rex tab is no longer available. Reopen Settings in the tab you want to change.";

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
    claude: Option<ClaudeSnapshot>,
    doctor: Option<doctor::Health>,
    version: u64,
    // A local-only browser edit can dismiss this save without hiding a later reminder.
    reopen_panes: Option<u64>,
    directory: Option<String>,
}

pub(crate) enum CloseError {
    Stale,
    Trial(&'static str),
}

impl Editor {
    pub(crate) fn open() -> Self {
        let inside = rex::inside();
        let store = Store::real();
        let target = inside.then(rex::Target::current).flatten();
        let anchor = store.as_ref().zip(target.as_ref()).and_then(|(store, target)| store.anchor_tab(target));
        let (root, window) = if inside {
            anchor.map_or((None, None), |(window, root, _)| (root, Some(window)))
        } else {
            (crate::preferences::worktree(Path::new(".")), None)
        };
        let mut editor = Self::new(store, inside, target, root, window, rex::run);
        editor.setup.alerts = Alerts::real();
        editor.setup.refresh_alerts();
        editor.setup.presence = editor.setup.presence.take().map(presence::Control::starting_helper);
        editor
    }

    fn new(
        store: Option<Store>,
        inside: bool,
        target: Option<rex::Target>,
        root: Option<PathBuf>,
        window: Option<String>,
        run: fn(&[&OsStr]) -> bool,
    ) -> Self {
        let (status, claude) = account_at(store.as_ref());
        let mut setup = Setup::new_with_claude(store, inside, target, root, run, status);
        setup.web_window = window;
        if !inside && let Err(why) = setup.reload_layout() {
            setup.report(Err(why));
        }
        if inside && setup.web_window.is_none() && setup.message.is_none() {
            setup.report(Err(LOST_TAB));
        }
        let directory = std::env::current_dir().ok().map(|path| path.to_string_lossy().into_owned());
        if let Err(why) = setup.reload_prefs() {
            setup.report(Err(why));
        }
        Self { setup, claude, doctor: None, version: 1, reopen_panes: None, directory }
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
        let preferences_editable = setup.saved_prefs.is_some();
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
                scope: match setup.prefs_scope() {
                    Source::Default => "default",
                    Source::Global => "global",
                    Source::Worktree => "worktree",
                },
                dirty: setup.prefs_dirty(),
                editable: preferences_editable,
                save_global: preferences_editable,
                save_worktree: preferences_editable && setup.root.is_some(),
                inherit: preferences_editable && setup.prefs_scope() == Source::Worktree,
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

    #[cfg(test)]
    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    pub(crate) fn close(&mut self, version: u64) -> Result<(), CloseError> {
        if version != self.version {
            return Err(CloseError::Stale);
        }
        if self.setup.trial.is_some() {
            self.setup.undo().map_err(CloseError::Trial)?;
        }
        Ok(())
    }

    pub(crate) fn state(&self) -> String {
        let setup = &self.setup;
        let layout = &setup.layout;
        let prefs = &setup.prefs;
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
        let [usage_codex, usage_claude, usage_opencode] = UsageTool::ALL.map(|tool| prefs.usage_tools[tool].value());
        let [usage_deepseek, usage_openrouter, usage_devin, usage_copilot] =
            UsageAccount::ALL.map(|account| prefs.usage_accounts[account].value());
        let mut out = String::with_capacity(1600);
        write!(out, concat!(
            "{{\"version\":{},",
            "\"context\":{{\"inside_rex\":{},\"worktree\":{},\"directory\":{},\"installed\":{}}},",
            "\"layout\":{{\"panes\":{{\"outline\":{},\"history\":{},\"usage\":{}}},\"side\":\"{}\",\"width\":{},",
            "\"order\":[\"{}\",\"{}\",\"{}\"],\"usage_rows\":{},\"ratios\":{},\"auto_arrange\":{}}},",
            "\"appearance\":{{\"icons\":\"{}\",\"static_marks\":{},\"text_git_marks\":{},\"usage_large_figures\":{},",
            "\"usage_codex\":\"{}\",\"usage_claude\":\"{}\",\"usage_opencode\":\"{}\",",
            "\"usage_deepseek\":\"{}\",\"usage_openrouter\":\"{}\",\"usage_devin\":\"{}\",\"usage_copilot\":\"{}\"}},",
            "\"workflow\":{{\"project_mode\":\"{}\",\"follow_activity\":{},\"changes_only\":{},\"history_follow_new\":{},\"tab_titles\":{}}},",
            "\"menubar\":{{\"installed\":{}}},",
            "\"account\":{{\"claude_limits\":{},\"claude_status\":{}}},",
            "\"groups\":{{\"layout\":{},\"preferences\":{},\"account\":{}}},",
            "\"capabilities\":{{\"capture\":{},\"try\":{},\"undo\":{},\"auto_arrange\":{}}},",
            "\"dirty\":{},\"trial\":{},\"doctor\":{},\"message\":"),
            self.version, setup.inside, Optional(root.as_deref().map(Quoted)), Optional(self.directory.as_deref().map(Quoted)), setup.installed,
            show_outline, show_history, show_usage, side, layout.width.parse::<f64>().unwrap_or(0.2),
            first, second, third,
            layout.usage_rows.parse::<u32>().unwrap_or(super::layout::DEFAULT_USAGE_ROWS),
            Optional(layout.geometry.shares.map(Ratios)), layout.new_tabs,
            icons, prefs.static_marks, prefs.text_git_marks, prefs.usage_large_figures,
            usage_codex, usage_claude, usage_opencode,
            usage_deepseek, usage_openrouter, usage_devin, usage_copilot,
            project, prefs.follow_activity, prefs.changes_only, prefs.history_follow_new, prefs.tab_titles,
            Optional(menubar::installed()),
            setup.show_limits, Quoted(status), layout_state, prefs_state, account_state,
            capture, try_layout, trial, auto_arrange, setup.dirty(), trial,
            Optional(self.doctor.as_ref()),
        ).expect("writing to a string");
        if let Some((text, style)) = &setup.message {
            write!(out, "{{\"text\":{},\"error\":{}}}", Quoted(text), *style == DEL.bold())
                .expect("writing to a string");
        } else {
            out.push_str("null");
        }
        out.push_str(",\"reopen_panes\":");
        if let Some(version) = self.reopen_panes {
            write!(out, "{{\"version\":{version},\"text\":{}}}", Quoted(REOPEN_PANES)).expect("writing to a string");
        } else {
            out.push_str("null");
        }
        out.push_str(",\"alerts\":");
        match (&setup.alerts, &setup.alert_summary) {
            (Some(alerts), Some(summary)) => alerts_state(&mut out, alerts, summary),
            _ => out.push_str("null"),
        }
        out.push_str(",\"presence\":");
        // Read now, not when the editor opened: the helper's state changes on its own.
        match setup.presence.as_ref().and_then(|control| control.status().ok()) {
            Some(status) => {
                let setting = status.setting;
                let own = setting.application.is_some();
                // An ID is text: it is too long a number for a browser to keep exact.
                write!(
                    out,
                    concat!(
                        "{{\"enabled\":{},\"details\":\"{}\",\"application_id\":{},\"official_id\":\"{}\",",
                        "\"status\":{},\"help\":{},\"public\":{{"
                    ),
                    setting.enabled,
                    setting.details.value(),
                    Optional(setting.application.map(|id| format!("\"{id}\""))),
                    presence::OFFICIAL_APPLICATION,
                    Quoted(&status.sentence()),
                    Quoted(presence::APPLICATION_HELP),
                )
                .expect("writing to a string");
                // What each choice of names would make public, to show before it is chosen.
                for (i, details) in presence::Details::ALL.into_iter().enumerate() {
                    let comma = if i == 0 { "" } else { "," };
                    write!(out, "{comma}\"{}\":{}", details.value(), Quoted(presence::public(details, own)))
                        .expect("writing to a string");
                }
                out.push_str("}}");
            }
            None => out.push_str("null"),
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
            self.setup.report_text(message.map_err(Cow::Borrowed));
        }
        self.version += 1;
        Ok(self.state())
    }

    fn apply(&mut self, action: Action) -> Result<Option<SaveNotice>, String> {
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
                self.reopen_panes = None;
                None
            }
            Action::Discard(group) => {
                self.discard(group)?;
                self.reopen_panes = None;
                Some(Ok("Draft changes discarded. Saved settings were left alone.".into()))
            }
            Action::Save(Group::Layout, WriteScope::Global) if layout.save_global => {
                Some(self.save_layout(Scope::Global)?)
            }
            Action::Save(Group::Layout, WriteScope::Worktree) if layout.save_worktree => {
                Some(self.save_layout(Scope::Worktree)?)
            }
            Action::Save(Group::Preferences, WriteScope::Global) if preferences.save_global => {
                Some(self.save_preferences(Scope::Global)?)
            }
            Action::Save(Group::Preferences, WriteScope::Worktree) if preferences.save_worktree => {
                Some(self.save_preferences(Scope::Worktree)?)
            }
            Action::Save(Group::Account, WriteScope::Account) if account.save_account => Some(self.save_account()?),
            Action::Inherit(Group::Layout) if layout.inherit => Some(self.inherit_layout()?),
            Action::Inherit(Group::Preferences) if preferences.inherit => Some(self.inherit_preferences()?),
            Action::Capture if self.live() && layout.editable && self.setup.trial.is_none() => {
                let message = self.setup.capture()?;
                self.reopen_panes = None;
                Some(Ok(message.into()))
            }
            Action::Try if self.live() && layout.editable => {
                self.setup.try_layout()?;
                self.doctor = None;
                let message = if self.setup.trial.as_ref().is_some_and(|(trial, _)| trial.temporary()) {
                    "Previewing a temporary rail in the original Rex tab. Existing panes are untouched."
                } else {
                    "Trying this layout in the original Rex tab. Nothing was saved."
                };
                Some(Ok(message.into()))
            }
            Action::Undo if self.setup.trial.is_some() => {
                let message = self.setup.undo()?;
                self.doctor = None;
                Some(Ok(message.into()))
            }
            Action::Doctor => {
                self.doctor = None;
                let setup = &self.setup;
                let target = if setup.inside { self.require_tab().map(|()| setup.target.as_ref()) } else { Ok(None) };
                self.doctor = Some(doctor::health_anchored(setup.store()?, setup.root.as_deref(), target)?);
                None
            }
            Action::Presence(edit) if self.setup.presence.is_some() => {
                // One global choice, saved at once like the menu app, and no part of a draft.
                let result = match edit {
                    Some(change) => self.setup.change_presence(change),
                    None => self.setup.disable_presence(),
                };
                Some(result.map(Cow::Owned))
            }
            Action::Menubar(action) if menubar::installed().is_some() => {
                // Install also opens an existing app, replacing it only when the bundled copy differs.
                return menubar::run(action).map(|text| Some(Ok(text.into())));
            }
            Action::Alerts(change) => {
                // Like the menu app, alert choices are saved at once and are no part of the draft.
                let result = change.apply(self.setup.alerts.as_ref().ok_or(UNAVAILABLE)?);
                self.setup.refresh_alerts();
                // The page shows what is sent beside the controls; the notice says what happened.
                return result.map(|text| text.lines().next().map(|line| Ok(line.to_owned().into())));
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
            && let Some(saved) = &setup.saved_prefs
        {
            setup.prefs = saved.prefs;
        }
        if matches!(group, Group::Account | Group::All) {
            setup.show_limits = setup.claude == Claude::On;
        }
        Ok(())
    }

    fn save_layout(&mut self, scope: Scope) -> Result<SaveNotice, &'static str> {
        let inside = self.setup.inside;
        if inside {
            self.require_tab()?;
        } else if !self.setup.layout_dirty() {
            return Ok(Ok("No layout changes to save.".into()));
        }
        let notice = self.setup.persist_layout(scope, inside)?;
        self.doctor = None;
        Ok(notice)
    }

    fn inherit_layout(&mut self) -> Result<SaveNotice, &'static str> {
        if self.setup.inside {
            self.require_tab()?;
        }
        let notice = self.setup.inherit_layout()?;
        self.doctor = None;
        Ok(notice)
    }

    fn save_preferences(&mut self, scope: Scope) -> Result<SaveNotice, &'static str> {
        if !self.setup.prefs_dirty() {
            return Ok(Ok(NO_PREFS_CHANGES.into()));
        }
        let result = self.setup.persist_prefs(scope)?;
        Ok(self.finish_preferences(result))
    }

    fn inherit_preferences(&mut self) -> Result<SaveNotice, &'static str> {
        let result = self.setup.inherit_prefs()?;
        Ok(self.finish_preferences(result))
    }

    fn finish_preferences(&mut self, result: PreferenceSave) -> SaveNotice {
        self.doctor = None;
        self.reopen_panes = None;
        result.map(|feedback| {
            self.reopen_panes = feedback.reopen.then_some(self.version + 1);
            feedback.text
        })
    }

    fn save_account(&mut self) -> Result<SaveNotice, &'static str> {
        let setup = &self.setup;
        if !setup.claude_dirty() {
            return Ok(Ok("No Claude Code changes to save.".into()));
        }
        let expected = self.claude.as_ref().ok_or(UNAVAILABLE)?;
        setup.store()?.save_claude_draft(setup.show_limits, expected)?;
        self.doctor = None;
        self.load_account();
        Ok(self.setup.claude_save_feedback().map(Cow::Borrowed))
    }
}

fn account_at(store: Option<&Store>) -> (Claude, Option<ClaudeSnapshot>) {
    store
        .and_then(|store| store.claude_snapshot().ok())
        .map_or((Claude::Invalid, None), |(status, snapshot)| (status, Some(snapshot)))
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
    Save(Group, WriteScope),
    Inherit(Group),
    Discard(Group),
    Capture,
    Try,
    Undo,
    Doctor,
    Menubar(menubar::Action),
    Alerts(AlertChange),
    /// Change Discord presence, or with `None` turn it off.
    Presence(Option<presence::Change>),
}

impl Action {
    fn parse(body: &[u8]) -> Option<(u64, Self)> {
        let names = ["action", "version", "field", "value", "group", "scope"];
        let mut fields = [None; 6];
        let mut valid = true;
        json::settings_object(std::str::from_utf8(body).ok()?, |member| {
            if let Some(index) = names.iter().position(|name| member.key.is(name)) {
                valid &= fields[index].replace(member.value).is_none();
            } else {
                valid = false;
            }
        })?;
        let [Some(action), Some(version), field, value, group, scope] = fields else { return None };
        if !valid {
            return None;
        }
        let command = match [field, value, group, scope] {
            [Some(field), Some(value), None, None] if action.is("set") => Self::Set(Edit::parse(field, value)?),
            [None, None, Some(group), Some(scope)] if action.is("save") => {
                match (Group::parse(group)?, scope.string(8)?.as_ref()) {
                    (group @ (Group::Layout | Group::Preferences), "global") => Self::Save(group, WriteScope::Global),
                    (group @ (Group::Layout | Group::Preferences), "worktree") => {
                        Self::Save(group, WriteScope::Worktree)
                    }
                    (Group::Account, "account") => Self::Save(Group::Account, WriteScope::Account),
                    _ => return None,
                }
            }
            [None, None, Some(group), None] if action.is("inherit") => match Group::parse(group)? {
                group @ (Group::Layout | Group::Preferences) => Self::Inherit(group),
                _ => return None,
            },
            [None, None, Some(group), None] if action.is("discard") => Self::Discard(Group::parse(group)?),
            [None, Some(value), None, None] if action.is("menubar") => {
                Self::Menubar(match value.string(9)?.as_ref() {
                    "install" => menubar::Action::Install,
                    "uninstall" => menubar::Action::Uninstall,
                    _ => return None,
                })
            }
            [Some(field), Some(value), None, None] if action.is("alerts") => {
                Self::Alerts(AlertChange::parse(field, value)?)
            }
            [None, Some(value), None, None] if action.is("presence") => {
                Self::Presence(match value.string(3)?.as_ref() {
                    "on" => Some(presence::Change::Enable),
                    "off" => None,
                    _ => return None,
                })
            }
            [Some(field), Some(value), None, None] if action.is("presence") => {
                let value = value.string(19)?;
                Self::Presence(Some(match field.string(11)?.as_ref() {
                    "details" => presence::Change::Details(presence::Details::parse(&value)?),
                    "application" if value == "official" => presence::Change::Application(None),
                    "application" => presence::Change::Application(Some(presence::application_id(&value)?)),
                    _ => return None,
                }))
            }
            [None, None, None, None] => match action.string(7)?.as_ref() {
                "capture" => Self::Capture,
                "try" => Self::Try,
                "undo" => Self::Undo,
                "doctor" => Self::Doctor,
                _ => return None,
            },
            _ => return None,
        };
        Some((version.unsigned()?, command))
    }
}

/// One change to the mirroring of Rex alerts. A secret arrives only in
/// `Telegram` and `Discord`, and is never sent back to the browser.
enum AlertChange {
    Enabled(bool),
    Destination(alerts::Destination),
    Kind(alerts::Kind, bool),
    Content(alerts::Content),
    Telegram { token: String, chat: String },
    Discord(String),
    Test,
    Forget(alerts::Destination),
    /// Reads again whether mirroring is running and what it last noted.
    Refresh,
}

impl AlertChange {
    fn parse(field: Value<'_>, value: Value<'_>) -> Option<Self> {
        let destination = |value: Value<'_>| alerts::Destination::parse(value.string(8)?.as_ref());
        Some(match field.string(32)?.as_ref() {
            "enabled" => Self::Enabled(value.boolean()?),
            "destination" => Self::Destination(destination(value)?),
            "content" => Self::Content(alerts::Content::parse(value.string(8)?.as_ref())?),
            "telegram" => {
                let [token, chat] = value.fields(["token", "chat"]);
                Self::Telegram { token: token?.string(128)?.into_owned(), chat: chat?.string(64)?.into_owned() }
            }
            "discord" => Self::Discord(value.string(256)?.into_owned()),
            "test" if value.null() => Self::Test,
            "forget" => Self::Forget(destination(value)?),
            "refresh" if value.null() => Self::Refresh,
            field => Self::Kind(alerts::Kind::parse(field.strip_prefix("events.")?)?, value.boolean()?),
        })
    }

    fn apply(self, alerts: &Alerts) -> Result<String, String> {
        match self {
            Self::Enabled(true) => alerts.enable(),
            Self::Enabled(false) => alerts.disable(),
            Self::Destination(destination) => alerts.choose(destination),
            Self::Kind(kind, on) => alerts.set_kind(kind, on),
            Self::Content(content) => alerts.set_content(content),
            Self::Telegram { token, chat } => alerts.set_telegram(token.trim(), chat.trim()),
            Self::Discord(link) => alerts.set_discord(link.trim()),
            Self::Test => alerts.test(),
            Self::Forget(destination) => alerts.forget(destination),
            Self::Refresh => Ok(String::new()),
        }
    }
}

/// What the browser shows about mirroring. Whether a secret is saved, never the secret.
fn alerts_state(out: &mut String, alerts: &Alerts, summary: &alerts::Summary) {
    let settings = &summary.settings;
    let [telegram, discord] = alerts::Destination::ALL.map(|destination| summary.saved(destination));
    write!(
        out,
        concat!(
            "{{\"enabled\":{},\"running\":{},\"destination\":{},",
            "\"telegram\":{{\"saved\":{},\"chat\":{}}},\"discord\":{{\"saved\":{}}},\"events\":{{"
        ),
        settings.enabled,
        summary.running,
        Optional(settings.destination.map(|destination| Quoted(destination.name()))),
        telegram,
        Quoted(&settings.telegram_chat),
        discord,
    )
    .expect("writing to a string");
    for (index, kind) in alerts::Kind::ALL.into_iter().enumerate() {
        let comma = if index == 0 { "" } else { "," };
        write!(out, "{comma}\"{}\":{}", kind.name(), settings.mirrors(kind)).expect("writing to a string");
    }
    write!(
        out,
        "}},\"content\":\"{}\",\"place\":{},\"disclosure\":{},\"note\":{}}}",
        settings.content.name(),
        Quoted(alerts.place()),
        Quoted(&alerts::disclosure(settings)),
        Quoted(&summary.note),
    )
    .expect("writing to a string");
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
    UsageRows(u32),
    Ratios(Option<[u32; 3]>),
    AutoArrange(bool),
    Icons(IconMode),
    StaticMarks(bool),
    TextGitMarks(bool),
    UsageLargeFigures(bool),
    UsageTool(UsageTool, Show),
    UsageAccount(UsageAccount, Show),
    ProjectMode(ProjectMode),
    FollowActivity(bool),
    ChangesOnly(bool),
    HistoryFollowNew(bool),
    TabTitles(bool),
    ClaudeLimits(bool),
}

impl Edit {
    fn parse(field: Value<'_>, value: Value<'_>) -> Option<Self> {
        Some(match field.string(32)?.as_ref() {
            "layout.side" => Self::Side(match value.string(5)?.as_ref() {
                "left" => Side::Left,
                "right" => Side::Right,
                _ => return None,
            }),
            "layout.width" => Self::Width(value.number().filter(|n| 0.0 < *n && *n < 1.0)?),
            "layout.order" => {
                let order = triplet(value, |value| pane_named(value.string(7)?.as_ref()))?;
                let [a, b, c] = order;
                if a == b || b == c || c == a {
                    return None;
                }
                Self::Order(order)
            }
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
            "workflow.tab_titles" => Self::TabTitles(value.boolean()?),
            "account.claude_limits" => Self::ClaudeLimits(value.boolean()?),
            field if field.starts_with("appearance.usage_") => {
                let key = &field["appearance.".len()..];
                let show = || Show::parse(value.string(6)?.as_ref());
                if let Some(tool) = UsageTool::ALL.into_iter().find(|tool| tool.key() == key) {
                    Self::UsageTool(tool, show()?)
                } else {
                    let account = UsageAccount::ALL.into_iter().find(|account| account.key() == key)?;
                    Self::UsageAccount(account, show()?)
                }
            }
            field => Self::Pane(pane_named(field.strip_prefix("layout.panes.")?)?, value.boolean()?),
        })
    }

    fn group(&self) -> Group {
        match self {
            Self::Pane(..)
            | Self::Side(_)
            | Self::Width(_)
            | Self::Order(_)
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
            Self::UsageTool(tool, show) => prefs.usage_tools[tool] = show,
            Self::UsageAccount(account, show) => prefs.usage_accounts[account] = show,
            Self::ProjectMode(mode) => prefs.project_mode = mode,
            Self::FollowActivity(on) => prefs.follow_activity = on,
            Self::ChangesOnly(on) => prefs.changes_only = on,
            Self::HistoryFollowNew(on) => prefs.history_follow_new = on,
            Self::TabTitles(on) => prefs.tab_titles = on,
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
            claude_state: None,
        }
    }

    fn outside(store: Store, root: Option<&Path>) -> Editor {
        Editor::new(Some(store), false, None, root.map(Path::to_path_buf), None, |_| {
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

    fn assert_feedback(json: &str, text: &str, reopen: bool) {
        let state = Value::parse(json.as_bytes()).unwrap();
        let message = field(state, "message");
        assert!(field(message, "text").is(text), "{json}");
        assert_eq!(field(message, "error").boolean(), Some(false));
        let instruction = field(state, "reopen_panes");
        if reopen {
            assert!(field(instruction, "text").is(REOPEN_PANES));
            assert_eq!(field(instruction, "version").unsigned(), field(state, "version").unsigned());
        } else {
            assert!(instruction.null(), "{json}");
        }
    }

    #[test]
    fn menu_app_status_and_actions_respect_availability_and_draft_version() {
        let f = Fixture::folder();
        let mut editor = outside(fixture_store(&f), None);
        let old_version = editor.version;
        let before = set(&mut editor, "workflow.project_mode", "\"pin\"");
        let state = Value::parse(before.as_bytes()).unwrap();
        let status = field(field(state, "menubar"), "installed");
        match menubar::installed() {
            Some(installed) => assert_eq!(status.boolean(), Some(installed)),
            None => assert!(status.null()),
        }
        for (operation, expected) in [("install", menubar::Action::Install), ("uninstall", menubar::Action::Uninstall)] {
            let request = format!(r#""action":"menubar","value":"{operation}""#);
            let body = format!(r#"{{"version":{},{request}}}"#, editor.version);
            assert!(matches!(Action::parse(body.as_bytes()), Some((_, Action::Menubar(parsed))) if parsed == expected));
            if status.null() {
                assert_eq!(action(&mut editor, &request).unwrap_err(), UNAVAILABLE);
            }
        }
        for request in [
            r#""action":"menubar""#,
            r#""action":"menubar","value":"toggle""#,
            r#""action":"menubar","value":true"#,
            r#""action":"menubar","value":"install","group":"preferences""#,
        ] {
            assert_eq!(action(&mut editor, request).unwrap_err(), BAD_ACTION);
        }
        let stale = format!(r#"{{"version":{old_version},"action":"menubar","value":"uninstall"}}"#);
        assert!(editor.action(stale.as_bytes()).unwrap_err().contains("draft changed"));
        assert_eq!(editor.state(), before);
        assert!(!editor.setup.store().unwrap().config.exists());
    }

    #[test]
    fn layout_edits_stay_in_the_draft_until_a_scoped_save() {
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
        set(&mut editor, "layout.usage_rows", "10");
        let state = set(&mut editor, "layout.ratios", "[750000,0,250000]");
        let layout = field(Value::parse(state.as_bytes()).unwrap(), "layout");
        assert!(field(layout, "side").is("right"));
        assert_eq!(field(layout, "width").number(), Some(0.35));
        assert_eq!(field(layout, "usage_rows").unsigned(), Some(10));
        assert_eq!(triplet(field(layout, "ratios"), Value::unsigned), Some([750_000, 0, 250_000]));
        assert!(!store.config.exists());

        let saved = action(&mut editor, r#""action":"save","group":"layout","scope":"worktree""#).unwrap();
        assert_feedback(&saved, "Saved layout for “it's a \"worktree\"”. Rex integration unchanged.", false);
        let (remembered, installed, scope) = store.layout_for(Some(&root)).unwrap();
        assert_eq!(scope, Scope::Worktree);
        assert!(!installed);
        assert_eq!(remembered.geometry.side, Side::Right);
        assert_eq!(remembered.geometry.order, [TabPane::Usage, TabPane::History, TabPane::Outline]);
        assert_eq!(remembered.geometry.shares, Some([750_000, 0, 250_000]));
        assert_eq!(remembered.width, "0.35");
        assert_eq!(remembered.usage_rows, "10");
        assert!(!remembered.chosen(TabPane::History));
        assert!(!editor.setup.dirty());
        assert!(!store.init.exists() && !store.rex_file().exists() && !store.state.exists());

        let reset = set(&mut editor, "layout.ratios", "null");
        assert!(field(field(Value::parse(reset.as_bytes()).unwrap(), "layout"), "ratios").null());
        assert_eq!(store.layout_for(Some(&root)).unwrap().0.geometry.shares, remembered.geometry.shares);
        set(&mut editor, "layout.side", "\"left\"");
        set(&mut editor, "workflow.changes_only", "true");
        action(&mut editor, r#""action":"discard","group":"layout""#).unwrap();
        assert_eq!(editor.setup.layout.geometry.side, Side::Right);
        assert_eq!(editor.setup.layout.geometry.shares, remembered.geometry.shares);
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
            r#""action":"remove""#,
        ] {
            assert!(action(&mut editor, request).is_err(), "{request}");
            assert_eq!(editor.state(), before, "{request}");
        }
        let old_version = field(Value::parse(before.as_bytes()).unwrap(), "version").unsigned().unwrap();
        let after = set(&mut editor, "layout.side", "\"right\"");
        assert_eq!(field(Value::parse(after.as_bytes()).unwrap(), "version").unsigned(), Some(old_version + 1));
        let stale = format!(r#"{{"version":{old_version},"action":"set","field":"layout.side","value":"left"}}"#);
        assert!(editor.action(stale.as_bytes()).unwrap_err().contains("draft changed"));
        assert_eq!(editor.state(), after);
        assert!(!editor.setup.store().unwrap().config.exists());
    }

    #[test]
    fn unused_controls_are_neither_advertised_nor_accepted() {
        let f = Fixture::folder();
        let mut editor = outside(fixture_store(&f), None);
        let before = editor.state();
        let state = Value::parse(before.as_bytes()).unwrap();
        assert!(state.fields(["preview", "app_version"]).into_iter().all(|field| field.is_none()));
        assert!(field(state, "layout").fields(["outline_share"])[0].is_none());
        assert!(field(state, "capabilities").fields(["refresh"])[0].is_none());
        for request in [
            r#""action":"preset","preset":"tree""#,
            r#""action":"refresh""#,
            r#""action":"set","field":"layout.outline_share","value":0.6"#,
        ] {
            assert_eq!(action(&mut editor, request).unwrap_err(), BAD_ACTION, "{request}");
        }
        assert_eq!(editor.state(), before);
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
        assert_eq!(first.setup.prefs_scope(), Source::Worktree);
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
        assert_eq!(newer.setup.prefs_scope(), Source::Default);
        assert_eq!(newer.setup.prefs, Preferences::default());
    }

    #[test]
    fn pane_reopen_reminder_is_only_set_when_effective_pane_preferences_change() {
        let f = Fixture::folder();
        let root = f.root.join("checkout");
        let mut editor = outside(fixture_store(&f), Some(&root));
        let save_global = r#""action":"save","group":"preferences","scope":"global""#;
        let save_worktree = r#""action":"save","group":"preferences","scope":"worktree""#;

        assert_feedback(&action(&mut editor, save_worktree).unwrap(), NO_PREFS_CHANGES, false);
        set(&mut editor, "workflow.changes_only", "true");
        let saved = action(&mut editor, save_worktree).unwrap();
        assert_feedback(&saved, "Saved pane preferences for “checkout”. Reopen panes to apply.", true);

        let version = editor.version();
        let checked = action(&mut editor, r#""action":"doctor""#).unwrap();
        let checked = Value::parse(checked.as_bytes()).unwrap();
        assert!(field(checked, "message").null());
        assert_eq!(field(field(checked, "reopen_panes"), "version").unsigned(), Some(version));

        let edited = set(&mut editor, "workflow.tab_titles", "true");
        assert!(field(Value::parse(edited.as_bytes()).unwrap(), "reopen_panes").null());
        let saved = action(&mut editor, save_global).unwrap();
        assert_feedback(&saved, "Saved global pane preferences. Milo uses the title setting when naming Rex tabs.", false);

        set(&mut editor, "workflow.history_follow_new", "true");
        let saved = action(&mut editor, save_global).unwrap();
        assert_feedback(&saved, "Saved global pane preferences. Reopen panes to apply.", true);

        set(&mut editor, "workflow.changes_only", "false");
        let saved = action(&mut editor, save_global).unwrap();
        assert_feedback(&saved, "Saved global pane preferences. “checkout” keeps its preferences.", false);

        let inherited = action(&mut editor, r#""action":"inherit","group":"preferences""#).unwrap();
        assert_feedback(&inherited, "“checkout” now uses global pane preferences. Reopen panes to apply.", true);
    }

    #[test]
    fn usage_tool_choices_are_reported_edited_and_saved_with_the_other_preferences() {
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let root = &f.root;
        let mut editor = outside(store.clone(), Some(root));
        let before = editor.state();
        let appearance = field(Value::parse(before.as_bytes()).unwrap(), "appearance");
        for key in ["usage_codex", "usage_claude", "usage_opencode"] {
            assert!(field(appearance, key).is("auto"), "{key}");
        }
        for request in [
            r#""action":"set","field":"appearance.usage_opencode","value":"sometimes""#,
            r#""action":"set","field":"appearance.usage_opencode","value":true"#,
            r#""action":"set","field":"appearance.usage_gemini","value":"auto""#,
        ] {
            assert_eq!(action(&mut editor, request).unwrap_err(), BAD_ACTION, "{request}");
            assert_eq!(editor.state(), before, "{request}");
        }

        set(&mut editor, "appearance.usage_claude", "\"always\"");
        let draft = set(&mut editor, "appearance.usage_opencode", "\"hide\"");
        let draft = Value::parse(draft.as_bytes()).unwrap();
        assert!(field(field(draft, "appearance"), "usage_opencode").is("hide"));
        assert_eq!(field(field(field(draft, "groups"), "preferences"), "dirty").boolean(), Some(true));
        assert!(!store.config.exists(), "a draft writes nothing");
        action(&mut editor, r#""action":"save","group":"preferences","scope":"global""#).unwrap();
        let choices = |prefs: Preferences| UsageTool::ALL.map(|tool| prefs.usage_tools[tool]);
        let saved = PreferenceSnapshot::load(&store.config, Some(root)).unwrap();
        assert_eq!((choices(saved.prefs), saved.source), ([Show::Auto, Show::Always, Show::Hide], Source::Global));

        // A later session starts from the saved choices, and a worktree can differ from them.
        let mut later = outside(store.clone(), Some(root));
        let appearance = later.state();
        let appearance = field(Value::parse(appearance.as_bytes()).unwrap(), "appearance");
        assert!(field(appearance, "usage_claude").is("always") && field(appearance, "usage_opencode").is("hide"));
        set(&mut later, "appearance.usage_opencode", "\"auto\"");
        action(&mut later, r#""action":"save","group":"preferences","scope":"worktree""#).unwrap();
        let local = PreferenceSnapshot::load(&store.config, Some(root)).unwrap();
        assert_eq!((choices(local.prefs), local.source), ([Show::Auto, Show::Always, Show::Auto], Source::Worktree));
        let global = PreferenceSnapshot::load(&store.config, None).unwrap();
        assert_eq!(choices(global.prefs), [Show::Auto, Show::Always, Show::Hide]);
    }

    #[test]
    fn online_accounts_start_hidden_in_the_editor_and_a_choice_is_saved_without_touching_the_tools() {
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let mut editor = outside(store.clone(), None);
        let before = editor.state();
        let appearance = field(Value::parse(before.as_bytes()).unwrap(), "appearance");
        for key in ["usage_deepseek", "usage_openrouter", "usage_devin", "usage_copilot"] {
            assert!(field(appearance, key).is("hide"), "{key}");
        }
        let request = r#""action":"set","field":"appearance.usage_deepseek","value":"on""#;
        assert_eq!(action(&mut editor, request).unwrap_err(), BAD_ACTION);

        set(&mut editor, "appearance.usage_deepseek", "\"auto\"");
        set(&mut editor, "appearance.usage_copilot", "\"always\"");
        assert!(!store.config.exists(), "a draft writes nothing");
        action(&mut editor, r#""action":"save","group":"preferences","scope":"global""#).unwrap();
        let saved = PreferenceSnapshot::load(&store.config, None).unwrap().prefs;
        let accounts = UsageAccount::ALL.map(|account| saved.usage_accounts[account]);
        assert_eq!(accounts, [Show::Auto, Show::Hide, Show::Hide, Show::Always]);
        assert!(UsageTool::ALL.into_iter().all(|tool| saved.usage_tools[tool] == Show::Auto));

        // Hiding it again is saved too, so the provider is no longer contacted.
        let mut later = outside(store.clone(), None);
        set(&mut later, "appearance.usage_deepseek", "\"hide\"");
        action(&mut later, r#""action":"save","group":"preferences","scope":"global""#).unwrap();
        let saved = PreferenceSnapshot::load(&store.config, None).unwrap().prefs;
        assert_eq!(saved.usage_accounts[UsageAccount::DeepSeek], Show::Hide);
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
        let saved = action(&mut editor, r#""action":"save","group":"account","scope":"account""#).unwrap();
        assert_feedback(&saved, "Saved Claude Code account settings. Milo's status line is enabled.", false);
        assert!(store.claude() == Claude::On);
        assert!(editor.setup.prefs_dirty());
        assert!(!store.config.exists() && !store.init.exists());
    }

    #[test]
    fn a_rex_launch_without_the_original_target_can_edit_the_draft_but_cannot_install() {
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let mut editor = Editor::new(Some(store.clone()), true, None, None, None, |_| {
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

        let mut lost = Editor::new(Some(store.clone()), true, None, None, None, |_| {
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

    #[test]
    fn browser_alert_choices_are_saved_at_once_and_a_secret_never_comes_back() {
        const TOKEN: &str = "123456:SYNTHETICsyntheticSYNTHETIC00";
        let f = Fixture::folder();
        let mut editor = outside(fixture_store(&f), None);
        assert!(field(Value::parse(editor.state().as_bytes()).unwrap(), "alerts").null());
        let change = |editor: &mut Editor, field: &str, value: &str| {
            action(editor, &format!("\"action\":\"alerts\",\"field\":{field:?},\"value\":{value}"))
        };
        assert!(change(&mut editor, "enabled", "true").is_err());

        editor.setup.alerts = Some(Alerts::at(&f.root.join("alerts"), &f.root.join("alert-state")));
        editor.setup.refresh_alerts();
        let saved = |editor: &Editor| editor.setup.alerts.as_ref().unwrap().settings().unwrap();
        assert!(change(&mut editor, "enabled", "true").unwrap_err().contains("Nothing was turned on"));
        for (name, value) in
            [("telegram", "{\"token\":\"x\",\"chat\":\"1\"}"), ("discord", "\"https://example.org/x\"")]
        {
            assert!(change(&mut editor, name, value).is_err(), "{name}");
        }
        for (name, value) in
            [("events.everything", "true"), ("content", "\"all\""), ("test", "true"), ("forget", "\"x\"")]
        {
            assert_eq!(change(&mut editor, name, value).unwrap_err(), BAD_ACTION, "{name}");
        }

        let telegram = format!("{{\"token\":\"{TOKEN}\",\"chat\":\"-1001234567890\"}}");
        let state = change(&mut editor, "telegram", &telegram).unwrap();
        assert!(!state.contains("SYNTHETIC"), "{state}");
        let alerts = field(Value::parse(state.as_bytes()).unwrap(), "alerts");
        assert!(field(alerts, "destination").is("telegram"));
        assert_eq!(field(field(alerts, "telegram"), "saved").boolean(), Some(true));
        assert_eq!(field(alerts, "enabled").boolean(), Some(false));
        assert!(field(alerts, "disclosure").string(600).unwrap().contains("No text from the program"));

        change(&mut editor, "events.bell", "true").unwrap();
        change(&mut editor, "content", "\"text\"").unwrap();
        let state = change(&mut editor, "enabled", "true").unwrap();
        let alerts = field(Value::parse(state.as_bytes()).unwrap(), "alerts");
        assert_eq!(field(alerts, "enabled").boolean(), Some(true));
        assert_eq!(field(field(alerts, "events"), "bell").boolean(), Some(true));
        assert!(field(alerts, "disclosure").string(600).unwrap().contains("up to 300 characters"));
        assert!(saved(&editor).enabled && saved(&editor).content == alerts::Content::Text);
        // Alert choices are no draft: there is nothing to save or discard.
        assert_eq!(field(Value::parse(state.as_bytes()).unwrap(), "dirty").boolean(), Some(false));

        change(&mut editor, "enabled", "false").unwrap();
        assert!(!saved(&editor).enabled);
        let state = change(&mut editor, "forget", "\"telegram\"").unwrap();
        let alerts = field(Value::parse(state.as_bytes()).unwrap(), "alerts");
        assert!(field(alerts, "destination").null());
        assert_eq!(field(field(alerts, "telegram"), "saved").boolean(), Some(false));
    }

    #[test]
    fn discord_presence_is_one_saved_choice_that_only_an_explicit_action_changes() {
        const ID: &str = "1234567890123456789";
        let f = Fixture::folder();
        let store = fixture_store(&f);
        let file = store.config.join("discord.conf");
        let mut editor = outside(store, None);
        // Enabled, the details, the person's own application, the status line, and what a choice makes public.
        let presence = |json: &str, shown: &str| {
            let state = Value::parse(json.as_bytes()).unwrap();
            let presence = field(state, "presence");
            let id = field(presence, "application_id");
            (
                field(presence, "enabled").boolean().unwrap(),
                field(presence, "details").string(8).unwrap().into_owned(),
                (!id.null()).then(|| id.string(19).unwrap().into_owned()),
                field(presence, "status").string(400).unwrap().into_owned(),
                field(field(presence, "public"), shown).string(900).unwrap().into_owned(),
            )
        };
        let start = editor.state();
        let (enabled, details, id, status, public) = presence(&start, "generic");
        assert!(!enabled && id.is_none());
        assert_eq!(details, "generic");
        assert_eq!(status, "Milo is not connected to Discord and shows nothing there.");
        assert_eq!(public, presence::public(presence::Details::Generic, false));
        // The page holds what every choice would make public before any is turned on.
        for choice in presence::Details::ALL {
            assert_eq!(presence(&start, choice.value()).4, presence::public(choice, false));
        }
        let official = field(field(Value::parse(start.as_bytes()).unwrap(), "presence"), "official_id");
        assert!(official.is("1573574411418599586"));

        let draft = set(&mut editor, "workflow.tab_titles", "true");
        for request in [
            r#""action":"presence""#,
            r#""action":"presence","value":"yes""#,
            r#""action":"presence","value":true"#,
            r#""action":"presence","value":"1234567890123456789""#,
            r#""action":"presence","field":"details","value":"titles""#,
            r#""action":"presence","field":"details","value":true"#,
            r#""action":"presence","field":"application","value":1234567890123456789"#,
            r#""action":"presence","field":"application","value":"12345678901234567890""#,
            r#""action":"presence","field":"token","value":"secret""#,
            r#""action":"presence","value":"off","group":"preferences""#,
        ] {
            assert_eq!(action(&mut editor, request).unwrap_err(), BAD_ACTION, "{request}");
        }
        assert_eq!(editor.state(), draft);
        assert!(!file.exists());

        // Turning it on needs no application ID: Milo's own is used.
        let on = action(&mut editor, r#""action":"presence","value":"on""#).unwrap();
        let (enabled, details, id, ..) = presence(&on, "generic");
        assert!(enabled && id.is_none());
        assert_eq!(details, "generic");
        assert_eq!(fs::read_to_string(&file).unwrap(), "presence = true\n");
        let state = Value::parse(on.as_bytes()).unwrap();
        assert!(field(field(state, "message"), "text").string(400).unwrap().starts_with("Discord presence is on."));
        assert_eq!(field(state, "dirty").boolean(), Some(true), "the pane preference draft is kept");
        assert!(!editor.setup.store().unwrap().config.join("preferences.conf").exists());

        let both = action(&mut editor, r#""action":"presence","field":"details","value":"both""#).unwrap();
        assert_eq!(presence(&both, "both").1, "both");
        let own = action(&mut editor, &format!(r#""action":"presence","field":"application","value":"{ID}""#)).unwrap();
        let (_, _, id, _, public) = presence(&own, "both");
        assert_eq!(id.as_deref(), Some(ID), "the ID survives as text");
        assert_eq!(public, presence::public(presence::Details::Both, true), "the notice names the person's own application");
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("presence = true\napplication_id = {ID}\ndetails = both\n"));

        let off = action(&mut editor, r#""action":"presence","value":"off""#).unwrap();
        let (enabled, details, id, status, _) = presence(&off, "both");
        assert!(!enabled);
        assert_eq!((details.as_str(), id.as_deref()), ("both", Some(ID)), "the choices are remembered for next time");
        assert_eq!(status, "Milo is not connected to Discord and shows nothing there.");

        let reset = action(&mut editor, r#""action":"presence","field":"application","value":"official""#).unwrap();
        assert!(presence(&reset, "generic").2.is_none());
        action(&mut editor, r#""action":"presence","field":"details","value":"generic""#).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "presence = false\n");

        // Another surface changed the choice; the page shows what is saved now.
        fs::write(&file, "presence = true\ndetails = project\n").unwrap();
        let (enabled, details, ..) = presence(&editor.state(), "project");
        assert!(enabled);
        assert_eq!(details, "project");
    }
}
