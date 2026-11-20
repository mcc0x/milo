//! Inspect the current installation without starting Rex. Repair and removal
//! are explicit operations using the same ownership checks as setup.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::{self, PipeReader};
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use super::claude::Claude;
use super::install::{RunRex, Saved, Store, read};
use super::layout::{
    Layout, Side, TabPane, actions_template_current, launcher_matches, listener_template_current, probe_lua,
};
use super::profiles::{LayoutError, Scope};
use super::rex_files::{LISTENER_STATE_MISMATCH, LOADER_CONFLICT};
use super::{lock, rex_files};
use crate::git::discover_worktree;
use crate::preferences::{IconMode, Preferences, Show, Source, UsageTool, UsageTools};
use crate::{rex, usage};

pub(crate) const TITLE: &str = "Milo doctor";
pub(crate) const CHECKING: &str = "Checking…";

type Report = Result<String, String>;

#[derive(Default)]
pub(super) enum Health {
    #[default]
    Good,
    Warning(&'static str),
    Issue(&'static str),
}

impl Health {
    fn warn(&mut self, reason: &'static str) {
        if matches!(self, Self::Good) {
            *self = Self::Warning(reason);
        }
    }

    fn issue(&mut self, reason: &'static str) {
        if !matches!(self, Self::Issue(_)) {
            *self = Self::Issue(reason);
        }
    }
}
type Probe = Box<dyn FnOnce() -> Report + Send>;

/// The terminal owns one worker across Home and Settings. When a view leaves,
/// keep its worker until it finishes but discard its result. At most one new
/// request can wait behind it, and leaving again cancels that queued request.
#[derive(Default)]
pub(crate) struct Checks {
    job: Option<Job>,
    next: Option<Probe>,
}

struct Job {
    ready: PipeReader,
    result: Receiver<Report>,
    wanted: bool,
}

impl Checks {
    pub(crate) fn checking(&self) -> bool {
        self.next.is_some() || self.job.as_ref().is_some_and(|job| job.wanted)
    }

    pub(crate) fn start(&mut self, probe: impl FnOnce() -> Report + Send + 'static) -> Result<(), String> {
        if self.checking() {
            return Ok(());
        }
        if self.job.is_some() {
            self.next = Some(Box::new(probe));
            return Ok(());
        }
        self.launch(probe)
    }

    fn launch(&mut self, probe: impl FnOnce() -> Report + Send + 'static) -> Result<(), String> {
        let start = |err| format!("Could not start diagnostics: {err}");
        let (ready, done) = io::pipe().map_err(start)?;
        let (send, result) = mpsc::sync_channel(1);
        let _ = thread::Builder::new()
            .spawn(move || {
                let _ = send.send(probe());
                // Closing the writer wakes the event loop even if the probe failed.
                drop(done);
            })
            .map_err(start)?;
        self.job = Some(Job { ready, result, wanted: true });
        Ok(())
    }

    pub(crate) fn discard(&mut self) {
        self.next = None;
        if let Some(job) = &mut self.job {
            job.wanted = false;
        }
    }

    pub(crate) fn fd(&self) -> Option<RawFd> {
        self.job.as_ref().map(|job| job.ready.as_raw_fd())
    }

    pub(crate) fn collect(&mut self) -> Option<Report> {
        let job = self.job.as_ref()?;
        let result = match job.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Diagnostics ended unexpectedly.".into()),
        };
        let wanted = job.wanted;
        self.job = None;
        if let Some(probe) = self.next.take() {
            return self.launch(probe).err().map(Err);
        }
        wanted.then_some(result)
    }
}

fn store() -> Result<Store, String> {
    Store::real().ok_or_else(|| "Could not locate your settings folders. Check HOME and XDG_CONFIG_HOME.".into())
}

pub(crate) fn report() -> Result<String, String> {
    full_report(&store()?, None)
}

/// The report someone reads: the installation's health, then what the usage
/// tools saved. That look opens their files and databases, so health checks
/// alone do not run it.
pub(super) fn full_report(store: &Store, root: Option<&Path>) -> Result<String, String> {
    let (mut report, tools) = inspect(store, root)?;
    for line in usage::doctor(tools) {
        report.push('\n');
        report.push_str(&line);
    }
    Ok(report)
}

#[cfg(test)]
pub(super) fn report_for(store: &Store, root: Option<&Path>) -> Result<String, String> {
    inspect(store, root).map(|(report, _)| report)
}

fn inspect(store: &Store, root: Option<&Path>) -> Result<(String, UsageTools), String> {
    let target = rex::Target::current();
    let discovered;
    let worktree = if root.is_some() {
        Ok(root)
    } else {
        discovered = discover_worktree(Path::new("."));
        discovered.as_ref().map(|root| root.as_deref()).map_err(|&note| note)
    };
    report_with_worktree(store, worktree, target.as_ref()).map(|(report, _, tools)| (report, tools))
}

pub(super) fn health_anchored(
    store: &Store,
    root: Option<&Path>,
    target: Result<Option<&rex::Target>, &'static str>,
) -> Result<Health, String> {
    let (_, health, _) = report_with_worktree(store, Ok(root), target.ok().flatten())?;
    // The missing original tab matters more than other warnings, but it
    // shouldn't hide a definite problem with the installation.
    Ok(match target {
        Err(reason) if !matches!(health, Health::Issue(_)) => Health::Warning(reason),
        _ => health,
    })
}

fn report_with_worktree(
    store: &Store,
    worktree: Result<Option<&Path>, &'static str>,
    target: Option<&rex::Target>,
) -> Result<(String, Health, UsageTools), String> {
    let mut health = Health::Good;
    let mut tools = UsageTools::default();
    let mut out = String::from(TITLE);
    out.push('\n');
    match worktree {
        Ok(Some(path)) => writeln!(out, "Worktree: {path:?}").unwrap(),
        Ok(None) => out.push_str("Worktree: none detected\n"),
        Err(note) => writeln!(out, "Worktree: {note}").unwrap(),
    }
    let root = worktree.ok().flatten();
    match Preferences::resolve_in(&store.config, root) {
        Ok((p, source)) => {
            let scope = match source {
                Source::Default => "built-in defaults",
                Source::Global => "global default",
                Source::Worktree => "worktree over global default",
            };
            writeln!(out, "Preferences: {scope}").unwrap();
            writeln!(
                out,
                "  Icons: {}{}; Git text marks: {}; activity marks: {}",
                p.icons.effective().label(),
                if IconMode::environment().is_some() { " (MILO_ICONS)" } else { "" },
                on(p.text_git_marks),
                if p.static_marks { "static" } else { "animated" }
            )
            .unwrap();
            writeln!(
                out,
                "  Project: {}; outline: {}; follow writes: {}; follow new commits: {}",
                p.project_mode.label(),
                if p.changes_only { "changes only" } else { "all files" },
                on(p.follow_activity),
                on(p.history_follow_new)
            )
            .unwrap();
            writeln!(out, "  Narrow usage: {}", if p.usage_large_figures { "large figures" } else { "text" }).unwrap();
            writeln!(out, "  Usage tools: {}", usage_tools(p.usage_tools)).unwrap();
            tools = p.usage_tools;
            writeln!(out, "  Rex tab titles: {}", on(p.tab_titles)).unwrap();
        }
        Err(e) => {
            health.issue("Milo could not read your pane preferences.");
            writeln!(out, "Preferences: unreadable ({:?}). Correct the settings file before saving.", e.to_string())
                .unwrap()
        }
    }
    let layout = store.inspect_layout(root);
    match &layout {
        Ok((rail, true, scope)) => {
            let executable = rex_files::executable()?;
            let live = LiveSidebar::read(rail, &executable, target);
            if target.is_some() && matches!(live, LiveSidebar::NotObserved("could not read this Rex tab" | "could not find this Rex tab")) {
                health.warn("Milo could not inspect the original Rex tab.");
            }
            if matches!(live, LiveSidebar::Restored) {
                health.warn("Rex restored the Milo sidebar as shells.");
            }
            if write_sidebar(&mut out, rail, *scope, live) {
                health.warn("Live pane sizes differ from your saved layout.");
            }
            write_rex_installation(&mut out, &mut health, store, &executable, rex::layout_available())?;
        }
        Ok(_) => out.push_str("Rex: not installed; open milo setup in Rex to add the integration.\n"),
        Err(LayoutError::Rex(LOADER_CONFLICT)) => writeln!(
            out,
            "Rex loader conflict ({:?}): {LOADER_CONFLICT} Restore a single copy of the original two lines at the top, after any interpreter line.",
            store.init
        )
        .unwrap(),
        Err(LayoutError::Rex(why)) => {
            writeln!(out, "Rex: {why} Check the managed loader and run milo setup --repair.").unwrap()
        }
        Err(LayoutError::ProfileDirectory(why)) => {
            writeln!(out, "Rex layout profiles: {why} Check the directory {:?}.", store.config.join("layouts")).unwrap()
        }
        Err(LayoutError::Profile { path, reason }) => writeln!(
            out,
            "Saved layout profile {path:?}: {reason} Restore a known-good copy, or back up and move this file aside, then reopen Milo Settings to save a layout."
        )
        .unwrap(),
    }
    if layout.is_err() {
        health.issue("Milo could not load the Rex configuration. Run milo doctor for details.");
    }
    if store.rex_reload_pending() {
        health.warn("Rex has a pending configuration reload.");
        if matches!(layout, Err(LayoutError::Rex(LOADER_CONFLICT))) {
            out.push_str("Rex reload: pending; resolve the loader conflict first.\n");
        } else {
            out.push_str("Rex reload: pending; run milo setup --repair.\n");
        }
    }
    let expected = layout.map(|(_, active, _)| active).ok();
    let listener = if store.listener_state_matches() == Ok(false) {
        Err(LISTENER_STATE_MISMATCH)
    } else {
        lock::running(&store.state).map_err(|_| "unknown; check access to Milo's state directory")
    };
    match (&listener, expected) {
        (Ok(false), Some(true)) => health.issue("The layout listener is stopped. Run milo setup --repair."),
        (Ok(true), Some(false)) => health.warn("The layout listener is running unexpectedly."),
        (Err(_), _) => health.warn("Milo could not verify the layout listener."),
        _ => {}
    }
    writeln!(
        out,
        "Layout listener: {}",
        match (listener, expected) {
            (Ok(true), Some(true)) => "running",
            (Ok(false), Some(false)) => "off",
            (Ok(false), Some(true)) => "not running; run milo setup --repair",
            (Ok(true), Some(false)) => "running unexpectedly; run milo listen --stop",
            (Ok(true), None) => "running; expected state unknown because the Rex layout could not be read",
            (Ok(false), None) => "not running; expected state unknown because the Rex layout could not be read",
            (Err(why), _) => why,
        }
    )
    .unwrap();
    out.push_str("Claude status line: ");
    let claude = store.claude_files();
    match claude.as_ref().map_or(Claude::Invalid, |files| files.status()) {
        Claude::Off => out.push_str("not installed\n"),
        Claude::On if claude.is_ok_and(|files| !files.launcher_matches()) => {
            out.push_str("managed by Milo, but uses an old launcher; turn limits off and back on in setup.\n");
            health.warn("Claude Code uses an old Milo launcher.");
        }
        Claude::On => out.push_str("managed by Milo\n"),
        Claude::Foreign => out.push_str("user-managed; left alone\n"),
        Claude::NotOwned => {
            health.warn("Claude Code’s status line has no matching ownership note.");
            out.push_str("no matching ownership note; left alone. If Milo was installed with a different XDG_STATE_HOME, rerun with that value.\n");
        }
        Claude::Invalid if read(&store.claude_note()).is_err() => {
            health.warn("Milo could not verify Claude Code’s ownership note.");
            writeln!(
                out,
                "unknown; check the Claude Code ownership note in Milo's state directory ({:?})",
                store.claude_state_dir()
            )
            .unwrap()
        }
        Claude::Invalid => {
            health.issue("Milo could not read Claude Code’s settings.");
            writeln!(out, "unreadable; check Claude settings ({:?})", store.claude).unwrap()
        }
    }
    out.pop();
    Ok((out, health, tools))
}

fn write_rex_installation(
    out: &mut String,
    health: &mut Health,
    store: &Store,
    executable: &Path,
    available: Option<bool>,
) -> Result<(), &'static str> {
    let rex_file = store.rex_file();
    let file = read(&rex_file)?;
    let text = file.as_ref().and_then(|f| std::str::from_utf8(&f.bytes).ok()).unwrap_or("");
    let current = actions_template_current(text);
    let launcher = match launcher_matches(text, executable) {
        Some(true) => "matches this binary",
        Some(false) if current.is_some() => {
            health.warn("Rex points to a different Milo binary.");
            "points elsewhere; run milo setup --repair with the binary you want"
        }
        _ => {
            health.warn("Rex uses a custom Milo launcher.");
            "custom launcher; inspect the local milo declaration in rex.lua"
        }
    };
    writeln!(out, "Rex launcher: {launcher} ({executable:?})").unwrap();
    let actions = match current {
        Some(true) => "current",
        Some(false) => {
            health.issue("The managed Rex actions are out of date. Run milo setup --repair.");
            "out of date; run milo setup --repair"
        }
        None => "custom; left alone",
    };
    writeln!(out, "Rex actions file (rex.lua): {actions}").unwrap();
    let listener = match read(&store.listener_file()) {
        Ok(Some(file)) => {
            let current =
                std::str::from_utf8(&file.bytes).ok().and_then(|text| listener_template_current(text, &rex_file));
            match current {
                Some(true) => "current",
                Some(false) => {
                    health.issue("The managed Rex listener is out of date. Run milo setup --repair.");
                    "out of date; run milo setup --repair"
                }
                None => "custom; left alone",
            }
        }
        Ok(None) => {
            health.issue("The Rex listener file is missing. Run milo setup --repair.");
            "missing; run milo setup --repair"
        }
        Err(_) => {
            health.warn("Milo could not read the Rex listener file.");
            "unreadable; check listener.lua"
        }
    };
    writeln!(out, "Rex listener file (listener.lua): {listener}").unwrap();
    let loaded = match available {
        Some(true) => "loaded",
        Some(false) if current.is_none() => {
            health.warn("Not all Milo actions are loaded from your custom Rex configuration.");
            "not all Milo actions are loaded; inspect your custom rex.lua"
        }
        Some(false) => {
            health.issue("Rex did not load all current Milo actions. Run milo setup --repair.");
            "not current; run milo setup --repair"
        }
        None => {
            health.warn("Rex actions could not be checked. Check rex whoami and PATH.");
            "unavailable; check rex whoami and PATH"
        }
    };
    writeln!(out, "Server actions: {loaded}").unwrap();
    Ok(())
}

/// Only a choice someone made is spelled out, so a tool left alone goes unnamed.
fn usage_tools(tools: UsageTools) -> String {
    let mut out = String::new();
    for tool in UsageTool::ALL {
        let choice = match tools[tool] {
            Show::Auto => continue,
            Show::Always => "always shown",
            Show::Hide => "hidden",
        };
        write!(out, "{} {choice}; ", tool.label()).unwrap();
    }
    out.push_str(match out.len() {
        0 => "automatic",
        _ if UsageTool::ALL.into_iter().any(|tool| tools[tool] == Show::Auto) => "others automatic",
        _ => "",
    });
    out.truncate(out.trim_end_matches("; ").len());
    out
}

enum LiveUsage {
    Absent,
    Unmeasured,
    Rows(u32),
}

enum LiveSidebar {
    NotObserved(&'static str),
    Restored,
    Observed { side: Side, width: f64, usage: LiveUsage, expected_rows: Option<u32>, usage_differs: bool },
}

impl LiveSidebar {
    fn read(saved: &Layout, executable: &Path, target: Option<&rex::Target>) -> Self {
        let Some(target) = target else {
            return Self::NotObserved("run Doctor in the Rex tab you want to check");
        };
        let Some(block) = target.block.to_str().filter(|id| id.starts_with("block:")) else {
            return Self::NotObserved("could not find this Rex tab");
        };
        let caller = format!("caller={block}");
        let lua = probe_lua(saved, executable);
        rex::diagnostic_script(target, &lua, &caller)
            .as_deref()
            .and_then(Self::parse)
            .unwrap_or(Self::NotObserved("could not read this Rex tab"))
    }

    fn parse(reply: &str) -> Option<Self> {
        let reason = match reply {
            "no_tab" => "could not find this Rex tab",
            "restored" => return Some(Self::Restored),
            "unverified" => "pane processes could not be verified; inspect them before closing any panes",
            "no_rail" => "no Milo sidebar in this tab",
            "mixed" => "Milo panes are not in one sidebar",
            "trial" => "a setup trial is active",
            "zoomed" => "unzoom this tab and try again",
            _ => {
                let mut fields = reply.split_ascii_whitespace();
                if fields.next()? != "ok" {
                    return None;
                }
                let side = match fields.next()? {
                    "l" => Side::Left,
                    "r" => Side::Right,
                    _ => return None,
                };
                let width: f64 = fields.next()?.parse().ok()?;
                if !(width > 0.0 && width < 1.0) {
                    return None;
                }
                let usage = match fields.next()? {
                    "absent" => LiveUsage::Absent,
                    "-" => LiveUsage::Unmeasured,
                    rows => LiveUsage::Rows(rows.parse::<std::num::NonZeroU32>().ok()?.get()),
                };
                let expected_rows = match fields.next()? {
                    "-" => None,
                    rows => Some(rows.parse::<std::num::NonZeroU32>().ok()?.get()),
                };
                let usage_differs = match fields.next()? {
                    "different" if matches!(usage, LiveUsage::Rows(_)) && expected_rows.is_some() => true,
                    "ok" => false,
                    _ => return None,
                };
                return fields.next().is_none().then_some(Self::Observed {
                    side,
                    width,
                    usage,
                    expected_rows,
                    usage_differs,
                });
            }
        };
        Some(Self::NotObserved(reason))
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}

fn row_unit(rows: u32) -> &'static str {
    if rows == 1 { "row" } else { "rows" }
}

fn write_sidebar(out: &mut String, saved: &Layout, scope: Scope, live: LiveSidebar) -> bool {
    let scope = match scope {
        Scope::Global => "global default",
        Scope::Worktree => "worktree override",
    };
    let width = saved.width.parse::<f64>().unwrap_or_default();
    let has_saved_sidebar = saved.panes.contains(&true);
    if has_saved_sidebar {
        let side = side_name(saved.geometry.side);
        writeln!(out, "Saved sidebar: {:.1}% wide, {side} ({scope})", width * 100.0).unwrap();
        out.push_str("  Panes:");
        for role in saved.geometry.order.into_iter().filter(|role| saved.chosen(*role)) {
            write!(out, " {}", role.name()).unwrap();
        }
        out.push('\n');
    } else {
        writeln!(out, "Saved sidebar: none ({scope})").unwrap();
    }

    let expected_rows = match &live {
        LiveSidebar::Observed { expected_rows, .. } => *expected_rows,
        LiveSidebar::NotObserved(_) | LiveSidebar::Restored => None,
    };
    out.push_str("Saved Usage: ");
    if !saved.chosen(TabPane::Usage) {
        out.push_str("not included");
    } else if !saved.chosen(TabPane::Outline) && !saved.chosen(TabPane::History) {
        out.push_str("fills the sidebar");
    } else if saved.geometry.shares.is_some() {
        out.push_str("captured height");
        if let Some(rows) = expected_rows {
            write!(out, " (about {rows} {} here)", row_unit(rows)).unwrap();
        }
    } else {
        let saved_rows = saved.usage_rows.parse().unwrap_or_default();
        write!(out, "{} {}", saved.usage_rows, row_unit(saved_rows)).unwrap();
        if let Some(rows) = expected_rows.filter(|&rows| rows != saved_rows) {
            write!(out, " (about {rows} here)").unwrap();
        }
    }
    out.push('\n');

    let (live_width, usage_differs) = match live {
        LiveSidebar::NotObserved(reason) => {
            writeln!(out, "Live sidebar: not observed ({reason})").unwrap();
            return false;
        }
        LiveSidebar::Restored => {
            out.push_str(
                "Live sidebar: restored shells, not Milo panes.\n\
                 Finish any work in those shells, then run Milo: Arrange This Tab in Rex and confirm the rebuild.\n",
            );
            return false;
        }
        LiveSidebar::Observed { side, width, usage, usage_differs, .. } => {
            let side = side_name(side);
            writeln!(out, "Live sidebar: {:.1}% wide, {side}", width * 100.0).unwrap();
            match usage {
                LiveUsage::Absent => out.push_str("Live Usage: not in this sidebar\n"),
                LiveUsage::Unmeasured => out.push_str("Live Usage: row count unavailable\n"),
                LiveUsage::Rows(rows) => {
                    writeln!(out, "Live Usage: {rows} {}", row_unit(rows)).unwrap();
                }
            }
            (width, usage_differs)
        }
    };
    // Ignore tiny changes in the width fraction that are beneath display precision.
    if has_saved_sidebar && ((width - live_width).abs() > 0.0005 || usage_differs) {
        out.push_str(
            "Sidebar sizes differ from the saved layout.\n\
             Rex's Autobalance splits can resize panes when they open or close.\n\
             If enabled, turn it off in Rex Settings → Interface → Splits & Focus.\n\
             Then close and re-add the Milo panes to apply the saved sizes.\n",
        );
        return true;
    }
    false
}

fn on(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

pub(crate) fn repair() -> Result<String, String> {
    let store = store()?;
    let active = store.has_layout()?;
    if !store.listener_state_matches()? {
        return Err(LISTENER_STATE_MISMATCH.into());
    }
    if !active && !store.rex_reload_pending() {
        return Err("No managed Rex integration to repair. Run milo setup inside Rex first.".into());
    }
    let before = store.layout();
    let layout = match &before {
        Ok((layout, _)) => layout.clone(),
        // A missing loader target can be regenerated. Never replace an unreadable or edited file.
        Err(_) if read(&store.rex_file())?.is_none() => Layout::default(),
        Err(why) => return Err((*why).into()),
    };
    let saved = store.save_with(Some(&layout), None, &rex::run, |_| {
        if (!store.has_layout()? && !store.rex_reload_pending()) || store.layout() != before {
            return Err("The Rex layout changed while preparing repair. Run milo doctor again.");
        }
        Ok(())
    })?;
    check_listener(saved)?;
    if layout.enabled() {
        let file = read(&store.rex_file())?.ok_or("Rex layout is missing.")?;
        let text = std::str::from_utf8(&file.bytes).unwrap_or("");
        if launcher_matches(text, &rex_files::executable()?) != Some(true) {
            return Err(
                "Rex uses a custom launcher. Update its local milo path in rex.lua; Milo left that code alone.".into(),
            );
        }
        if actions_template_current(text).is_some()
            && rex::layout_available() != Some(true)
            && (!rex::run(&[OsStr::new("config"), OsStr::new("reload")]) || rex::layout_available() != Some(true))
        {
            return Err(
                "Rex did not load Milo's current actions. Check for actions that override Milo's in init.lua and run rex config check."
                    .into(),
            );
        }
    }
    Ok("Repaired the managed Rex integration. Your preferences were kept.".into())
}

pub(crate) fn remove() -> Result<String, String> {
    remove_from(&store()?, &rex::run)
}

fn remove_from(store: &Store, run: &RunRex<'_>) -> Result<String, String> {
    let disable = Layout { panes: [false; 3], new_tabs: false, ..Layout::default() };
    let rex = store.has_layout()?
        || store.rex_file().try_exists().map_err(|e| e.to_string())?
        || store.listener_file().try_exists().map_err(|e| e.to_string())?
        || store.rex_reload_pending()
        || lock::running(&store.state).map_err(|e| e.to_string())?;
    if rex && !store.listener_state_matches()? {
        return Err(LISTENER_STATE_MISMATCH.into());
    }
    let (claude, has_valid_note) =
        store.claude_files().map_or((Claude::Invalid, false), |files| files.status_with_ownership());
    let claude = match claude {
        Claude::On | Claude::Off | Claude::Foreign => has_valid_note.then_some(false),
        Claude::Invalid => {
            let note = read(&store.claude_note())
                .map_err(|_| "Could not verify Claude Code ownership. Check access to Milo's state directory.")?;
            if note.is_some() {
                return Err(format!(
                    "Could not remove Milo's Claude Code status line. Make sure {:?} is readable and contains valid JSON, then try again.",
                    store.claude
                ));
            }
            None
        }
        Claude::NotOwned => {
            return Err(concat!(
                "Could not verify ownership of the Milo command in Claude Code settings. ",
                "Rerun with the original XDG_STATE_HOME or remove the status line in Claude Code yourself.",
            )
            .into());
        }
    };
    if !rex && claude.is_none() {
        return Ok("No managed integration to remove. Preferences and user settings were left alone.".into());
    }
    let saved = store.save(rex.then_some(&disable), claude, run)?;
    check_listener(saved.listener)?;
    let mut message = String::from(
        "Removed managed integrations. Saved preferences, user-owned settings and open panes were left alone.",
    );
    if saved.reload_failed {
        message.push_str("\nRex was not reloaded. Restart Rex or run milo setup --repair to unload Milo's actions.");
    }
    Ok(message)
}

fn check_listener(result: Saved) -> Result<(), String> {
    match result {
        Saved::Ready => Ok(()),
        Saved::ListenerDidNotStart => Err("Settings saved, but the listener did not start. Run milo doctor.".into()),
        Saved::ListenerDidNotStop => {
            Err("Settings saved, but the listener did not stop. Run milo listen --stop.".into())
        }
    }
}

#[cfg(test)]
pub(crate) fn wait_ready(fd: Option<RawFd>) {
    let mut poll =
        [libc::pollfd { fd: fd.expect("diagnostics should have a wake pipe"), events: libc::POLLIN, revents: 0 }];
    assert_eq!(crate::term::wait_ready(&mut poll, 5_000), 1, "diagnostics did not complete");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn fixture_store(fixture: &Fixture) -> Store {
        Store {
            init: fixture.root.join("init.lua"),
            config: fixture.root.join("config"),
            state: fixture.root.join("state"),
            claude: fixture.root.join("settings.json"),
            claude_state: None,
        }
    }

    #[test]
    fn doctor_still_recommends_repair_when_the_active_loader_target_is_missing() {
        let fixture = Fixture::folder();
        let store = fixture_store(&fixture);
        let marker = std::str::from_utf8(rex_files::MANAGED).unwrap();
        let contents = format!("{marker}\n{}\n", rex_files::loader(&store.rex_file()));
        std::fs::write(&store.init, &contents).unwrap();

        let report = report_for(&store, Some(&fixture.root)).unwrap();
        assert!(report.contains("Rex: Could not read the saved Rex layout."), "{report}");
        assert!(report.contains("run milo setup --repair"), "{report}");
        assert!(!report.contains("Saved layout profile"), "{report}");
        assert_eq!(std::fs::read_to_string(store.init).unwrap(), contents);
    }

    #[test]
    fn doctor_separates_managed_templates_on_disk_from_loaded_actions() {
        let fixture = Fixture::folder();
        let store = fixture_store(&fixture);
        store.save(Some(&Layout::default()), None, &|_| true).unwrap();
        let executable = rex_files::executable().unwrap();
        let actions_path = store.rex_file();
        let listener_path = store.listener_file();
        let current_actions = std::fs::read_to_string(&actions_path).unwrap();
        let current_listener = std::fs::read_to_string(&listener_path).unwrap();
        let check = |available| {
            let mut report = String::new();
            let mut health = Health::Good;
            write_rex_installation(&mut report, &mut health, &store, &executable, available).unwrap();
            (report, health)
        };

        let (report, health) = check(Some(false));
        assert!(report.contains("Rex actions file (rex.lua): current"), "{report}");
        assert!(report.contains("Rex listener file (listener.lua): current"), "{report}");
        assert!(report.contains("Server actions: not current"), "{report}");
        assert!(matches!(health, Health::Issue(_)));

        let before = current_listener.strip_suffix(include_str!("listener.lua")).unwrap();
        let old_listener = format!("{before}{}", include_str!("listener_0_1_2.lua"));
        std::fs::write(&listener_path, &old_listener).unwrap();
        let (report, health) = check(Some(true));
        assert!(report.contains("Rex listener file (listener.lua): out of date"), "{report}");
        assert!(report.contains("Server actions: loaded"), "{report}");
        assert!(matches!(health, Health::Issue(_)));
        assert_eq!(std::fs::read_to_string(&listener_path).unwrap(), old_listener);

        let custom_actions = current_actions + "\n-- my own Rex actions\n";
        let custom_listener = old_listener + "\n-- my own listener\n";
        std::fs::write(&actions_path, &custom_actions).unwrap();
        std::fs::write(&listener_path, &custom_listener).unwrap();
        let (report, health) = check(Some(false));
        assert!(report.contains("Rex actions file (rex.lua): custom; left alone"), "{report}");
        assert!(report.contains("Rex listener file (listener.lua): custom; left alone"), "{report}");
        assert!(report.contains("Server actions: not all Milo actions are loaded"), "{report}");
        assert!(!report.contains("repair"), "{report}");
        assert!(matches!(health, Health::Warning(_)));
        assert_eq!(std::fs::read_to_string(&actions_path).unwrap(), custom_actions);
        assert_eq!(std::fs::read_to_string(&listener_path).unwrap(), custom_listener);
    }

    #[test]
    fn doctor_names_a_usage_tool_only_when_a_choice_was_made_for_it() {
        let fixture = Fixture::folder();
        let store = fixture_store(&fixture);
        let report = report_for(&store, Some(&fixture.root)).unwrap();
        assert!(report.contains("\n  Usage tools: automatic\n"), "{report}");
        assert!(!report.contains("OpenCode") && !report.contains("limits:"), "{report}");

        let mut tools = UsageTools::default();
        tools[UsageTool::OpenCode] = Show::Hide;
        assert_eq!(usage_tools(tools), "OpenCode hidden; others automatic");
        tools[UsageTool::Codex] = Show::Always;
        tools[UsageTool::Claude] = Show::Always;
        assert_eq!(usage_tools(tools), "Codex always shown; Claude Code always shown; OpenCode hidden");
    }

    #[test]
    fn removal_reports_that_rex_was_not_reloaded() {
        let fixture = Fixture::folder();
        let store = fixture_store(&fixture);
        store.save(Some(&Layout::default()), None, &|_| true).unwrap();
        let message = remove_from(&store, &|_| false).unwrap();
        assert!(message.contains("Removed managed integrations"));
        assert!(message.contains("Rex was not reloaded"));
        assert!(!store.init.exists());
    }

    #[test]
    fn removal_clears_a_stale_claude_note_when_the_status_line_is_absent() {
        let kept = b"{\n  \"keep\" : [1, 2]\n}\n";
        for settings in [None, Some(kept.as_slice())] {
            let fixture = Fixture::folder();
            let store = fixture_store(&fixture);
            let no_rex = |_: &[&OsStr]| panic!("Claude settings do not need Rex");
            store.save(None, Some(true), &no_rex).unwrap();
            if let Some(settings) = settings {
                std::fs::write(&store.claude, settings).unwrap();
            } else {
                std::fs::remove_file(&store.claude).unwrap();
            }
            let before = read(&store.claude).unwrap();

            assert!(remove_from(&store, &no_rex).unwrap().contains("Removed managed integrations"));
            assert!(!store.claude_note().exists());
            assert!(read(&store.claude).unwrap() == before);
            assert!(remove_from(&store, &no_rex).unwrap().contains("No managed integration"));
        }
    }

    #[test]
    fn removal_clears_only_the_active_profiles_stale_claude_note() {
        let fixture = Fixture::folder();
        let default = fixture_store(&fixture);
        let active = Store {
            claude: fixture.root.join("alternate/settings.json"),
            claude_state: Some(default.state.join("alternate")),
            ..default.clone()
        };
        let no_rex = |_: &[&OsStr]| panic!("Claude settings do not need Rex");
        for store in [&default, &active] {
            store.save(None, Some(true), &no_rex).unwrap();
        }
        let foreign = "{\r\n   \"statusLine\": {\"type\":\"command\",\"command\":\"my-status\"}, \"keep\" : 7\r\n}\r\n";
        std::fs::write(&active.claude, foreign).unwrap();
        let default_settings = std::fs::read(&default.claude).unwrap();
        let default_note = std::fs::read(default.claude_note()).unwrap();

        assert!(remove_from(&active, &no_rex).unwrap().contains("Removed managed integrations"));
        assert!(!active.claude_note().exists());
        assert_eq!(std::fs::read_to_string(&active.claude).unwrap(), foreign);
        assert_eq!(std::fs::read(&default.claude).unwrap(), default_settings);
        assert_eq!(std::fs::read(default.claude_note()).unwrap(), default_note);
    }

    #[test]
    fn removal_leaves_an_unrecognized_claude_note_alone() {
        let fixture = Fixture::folder();
        let store = fixture_store(&fixture);
        let no_rex = |_: &[&OsStr]| panic!("Claude settings do not need Rex");
        let foreign = br#"{"statusLine":{"type":"command","command":"my-status"}}"#;
        std::fs::write(&store.claude, foreign).unwrap();
        std::fs::create_dir_all(&store.state).unwrap();
        let changed = b"updated\nmy-status";
        std::fs::write(store.claude_note(), changed).unwrap();

        assert!(remove_from(&store, &no_rex).unwrap().contains("No managed integration"));
        assert_eq!(std::fs::read(store.claude_note()).unwrap(), changed);
        assert_eq!(std::fs::read(&store.claude).unwrap(), foreign);
    }

    #[test]
    fn invalid_claude_settings_block_removal_only_if_milo_might_own_the_status_line() {
        let fixture = Fixture::folder();
        let store = fixture_store(&fixture);
        let run = |_: &[&OsStr]| panic!("Claude settings do not need Rex");
        store.save(None, Some(true), &run).unwrap();
        let valid = std::fs::read_to_string(&store.claude).unwrap();
        let invalid = format!("{valid}{{");
        std::fs::write(&store.claude, &invalid).unwrap();
        let note = store.claude_note();

        let error = remove_from(&store, &run).unwrap_err();
        assert!(error.contains("Could not remove Milo's Claude Code status line"));
        assert_eq!(std::fs::read_to_string(&store.claude).unwrap(), invalid);
        assert!(note.exists());

        std::fs::remove_file(&note).unwrap();
        assert!(remove_from(&store, &run).unwrap().contains("No managed integration"));
        assert_eq!(std::fs::read_to_string(&store.claude).unwrap(), invalid);
    }

    #[test]
    fn doctor_names_the_state_directory_when_claude_ownership_cannot_be_read() {
        let fixture = Fixture::folder();
        let store = fixture_store(&fixture);
        std::fs::write(&store.claude, b"{}").unwrap();
        std::fs::create_dir_all(store.claude_note()).unwrap(); // Not a readable note, even when run as root.

        let report = report_for(&store, None).unwrap();
        assert!(report.contains(&format!(
            "Claude status line: unknown; check the Claude Code ownership note in Milo's state directory ({:?})",
            store.state
        )));
        assert!(!report.contains("unreadable; check Claude settings"));
    }

    #[test]
    fn removal_and_repair_save_refuse_to_switch_state_directories() {
        let fixture = Fixture::folder();
        let installed = fixture_store(&fixture);
        installed.save(Some(&Layout::default()), Some(true), &|_| true).unwrap();
        let elsewhere = Store { state: fixture.root.join("elsewhere/milo"), ..installed.clone() };
        let not_rex = |_: &[&OsStr]| panic!("the XDG mismatch must be detected before invoking Rex");

        assert_eq!(remove_from(&elsewhere, &not_rex).unwrap_err(), LISTENER_STATE_MISMATCH);
        assert_eq!(
            elsewhere.save_with(Some(&Layout::default()), None, &not_rex, |_| Ok(())).unwrap_err(),
            LISTENER_STATE_MISMATCH,
        );
        assert!(!elsewhere.state.parent().unwrap().exists());
        assert!(installed.has_layout().unwrap() && installed.listener_file().exists());
        assert!(installed.claude() == Claude::On);
        remove_from(&installed, &|_| true).unwrap();
        assert!(!installed.has_layout().unwrap() && !installed.listener_file().exists());
        assert!(installed.claude() == Claude::Off);
    }

    #[test]
    fn removal_reports_the_missing_note_for_a_claude_only_installation() {
        let fixture = Fixture::folder();
        let installed = fixture_store(&fixture);
        let not_rex = |_: &[&OsStr]| panic!("Claude settings do not need Rex");
        installed.save(None, Some(true), &not_rex).unwrap();
        let elsewhere = Store { state: fixture.root.join("elsewhere/milo"), ..installed.clone() };

        let error = remove_from(&elsewhere, &not_rex).unwrap_err();
        assert!(error.contains("original XDG_STATE_HOME"), "{error}");
        let report = report_for(&elsewhere, None).unwrap();
        assert!(report.contains("no matching ownership note"), "{report}");
        assert!(installed.claude() == Claude::On);
    }

    #[test]
    fn replacing_a_discarded_diagnostic_runs_only_the_latest_request() {
        let mut checks = Checks::default();
        let (release, gate) = mpsc::channel();
        checks
            .start(move || {
                gate.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
                Ok("abandoned".into())
            })
            .unwrap();
        checks.discard();
        assert!(!checks.checking());

        let (cancelled, was_cancelled) = mpsc::channel();
        checks
            .start(move || {
                cancelled.send(()).unwrap();
                Ok(String::new())
            })
            .unwrap();
        checks.discard();
        assert_eq!(was_cancelled.try_recv(), Err(TryRecvError::Disconnected));

        let (started, did_start) = mpsc::channel();
        checks
            .start(move || {
                started.send(()).unwrap();
                Ok("fresh".into())
            })
            .unwrap();
        checks.start(|| panic!("refresh should reuse the queued check")).unwrap();
        assert!(checks.checking());
        assert_eq!(did_start.try_recv(), Err(TryRecvError::Empty));
        assert!(checks.collect().is_none());

        release.send(()).unwrap();
        wait_ready(checks.fd());
        assert!(checks.collect().is_none(), "the previous screen's result must not be shown");
        wait_ready(checks.fd());
        assert_eq!(checks.collect(), Some(Ok("fresh".into())));
        assert!(did_start.try_recv().is_ok());
        assert!(!checks.checking());
        assert!(checks.fd().is_none());
    }

    fn sidebar(saved: &Layout, scope: Scope, response: &str) -> String {
        let mut report = String::new();
        write_sidebar(&mut report, saved, scope, LiveSidebar::parse(response).unwrap());
        report
    }

    #[test]
    fn doctor_compares_width_and_usage_against_the_effective_layout() {
        let saved = Layout::default();
        let matching = sidebar(&saved, Scope::Worktree, "ok l 0.20001 2 2 ok");
        assert!(matching.contains("Saved sidebar: 20.0% wide, left (worktree override)"));
        assert!(matching.contains("Live sidebar: 20.0% wide, left"));
        assert!(matching.contains("Saved Usage: 2 rows"));
        assert!(matching.contains("Live Usage: 2 rows"));
        assert!(!matching.contains("Autobalance"));

        for reply in ["ok r 0.33 2 2 ok", "ok l 0.2 10 2 different"] {
            let different = sidebar(&saved, Scope::Global, reply);
            assert!(different.contains("global default"));
            assert!(different.contains("Autobalance splits can resize panes"));
            assert!(different.contains("Interface → Splits & Focus"));
        }
    }

    #[test]
    fn doctor_does_not_infer_dimensions_when_the_sidebar_or_size_is_missing() {
        let saved = Layout::default();
        for response in ["no_tab", "no_rail", "mixed", "trial", "zoomed"] {
            let report = sidebar(&saved, Scope::Global, response);
            assert!(report.contains("Live sidebar: not observed"));
            assert!(!report.contains("Autobalance"));
        }

        let no_usage = sidebar(&saved, Scope::Global, "ok l 0.2 absent - ok");
        assert!(no_usage.contains("Live Usage: not in this sidebar"));
        let no_size = sidebar(&saved, Scope::Global, "ok l 0.2 - - ok");
        assert!(no_size.contains("Live Usage: row count unavailable"));
        assert!(!no_usage.contains("Autobalance"));
        assert!(!no_size.contains("Autobalance"));
    }

    #[test]
    fn doctor_distinguishes_restored_shells_from_an_unverified_sidebar() {
        let saved = Layout::default();
        let restored = sidebar(&saved, Scope::Global, "restored");
        assert!(restored.contains("Live sidebar: restored shells, not Milo panes."));
        assert!(restored.contains("Milo: Arrange This Tab in Rex and confirm the rebuild."));
        assert!(!restored.contains("Autobalance"));
        let unknown = sidebar(&saved, Scope::Global, "unverified");
        assert!(unknown.contains("pane processes could not be verified"));
        assert!(!unknown.contains("confirm the rebuild"));
    }

    #[test]
    fn doctor_describes_proportional_usage_without_showing_a_stale_row_setting() {
        let mut saved = Layout::default();
        saved.geometry.shares = Some([500_000, 250_000, 250_000]);
        let report = sidebar(&saved, Scope::Global, "ok l 0.2 10 10 ok");
        assert!(report.contains("Saved Usage: captured height (about 10 rows here)"));
        assert!(!report.contains("Saved Usage: 2 rows"));

        saved.panes = [false, false, true];
        let report = sidebar(&saved, Scope::Global, "ok l 0.2 40 - ok");
        assert!(report.contains("Saved Usage: fills the sidebar"));
        assert!(!report.contains("Autobalance"));
    }
}
