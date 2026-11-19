//! Inspect the current installation without starting Rex. Repair and removal
//! are explicit operations using the same ownership checks as setup.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::{self, PipeReader};
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use super::claude::{Claude, OWN_CLAUDE};
use super::install::{RunRex, Saved, Store, read};
use super::layout::{Layout, Side, TabPane, launcher_matches, owns_actions, probe_lua};
use super::profiles::Scope;
use super::{lock, rex_files};
use crate::preferences::{IconMode, Preferences, Source, worktree};
use crate::{rex, usage};

pub(crate) const TITLE: &str = "Milo doctor";
pub(crate) const CHECKING: &str = "Checking…";

type Report = Result<String, String>;
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
    let store = store()?;
    let root = worktree(Path::new("."));
    report_for(&store, root.as_deref())
}

pub(super) fn report_for(store: &Store, root: Option<&Path>) -> Result<String, String> {
    let mut out = String::from(TITLE);
    out.push('\n');
    if let Some(path) = root {
        writeln!(out, "Worktree: {path:?}").unwrap();
    } else {
        out.push_str("Worktree: none detected\n");
    }
    match Preferences::resolve(root) {
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
        }
        Err(e) => {
            writeln!(out, "Preferences: unreadable ({:?}). Correct the settings file before saving.", e.to_string())
                .unwrap()
        }
    }
    let layout = store.layout_for(root);
    match &layout {
        Ok((rail, true, scope)) => {
            let executable = rex_files::executable()?;
            let live = LiveSidebar::read(rail, &executable);
            write_sidebar(&mut out, rail, *scope, live);
            let file = read(&store.rex_file())?;
            let text = file.as_ref().and_then(|f| std::str::from_utf8(&f.bytes).ok()).unwrap_or("");
            let status = match launcher_matches(text, &executable) {
                Some(true) => "matches this binary",
                Some(false) if owns_actions(text) => {
                    "points elsewhere; run milo setup --repair with the binary you want"
                }
                _ => "custom launcher; inspect the local milo declaration in rex.lua",
            };
            writeln!(out, "Rex launcher: {status} ({executable:?})").unwrap();
            writeln!(
                out,
                "Server actions: {}",
                match rex::layout_available() {
                    Some(true) => "loaded",
                    Some(false) => "not current; run milo setup --repair",
                    None => "unavailable; check rex whoami and PATH",
                }
            )
            .unwrap();
        }
        Ok(_) => out.push_str("Rex: not installed; open milo setup in Rex to add the integration.\n"),
        Err(why) => writeln!(out, "Rex: {why} Check the managed loader and run milo setup --repair.").unwrap(),
    }
    if store.rex_reload_pending() {
        out.push_str("Rex reload: pending; run milo setup --repair.\n");
    }
    let expected = layout.map(|(_, active, _)| active).ok();
    writeln!(
        out,
        "Layout listener: {}",
        match (lock::running(&store.state), expected) {
            (Ok(true), Some(true)) => "running",
            (Ok(false), Some(false)) => "off",
            (Ok(false), Some(true)) => "not running; run milo setup --repair",
            (Ok(true), Some(false)) => "running unexpectedly; run milo listen --stop",
            (Ok(true), None) => "running; expected state unknown because the Rex layout could not be read",
            (Ok(false), None) => "not running; expected state unknown because the Rex layout could not be read",
            (Err(_), _) => "unknown; check access to Milo's state directory",
        }
    )
    .unwrap();
    out.push_str("Claude status line: ");
    match store.claude() {
        Claude::Off => out.push_str("not installed\n"),
        Claude::On => out.push_str("managed by Milo\n"),
        Claude::Foreign | Claude::NotOwned => out.push_str("user-managed; left alone\n"),
        Claude::Invalid if read(&store.state.join(OWN_CLAUDE)).is_err() => writeln!(
            out,
            "unknown; check the Claude Code ownership note in Milo's state directory ({:?})",
            store.state
        )
        .unwrap(),
        Claude::Invalid => writeln!(out, "unreadable; check Claude settings ({:?})", store.claude).unwrap(),
    }
    for (provider, source) in usage::sources() {
        writeln!(out, "{provider} limits: {}", source.unwrap_or("no local report; see the usage pane for setup hints"))
            .unwrap();
    }
    out.pop();
    Ok(out)
}

enum LiveUsage {
    Absent,
    Unmeasured,
    Rows(u32),
}

enum LiveSidebar {
    NotObserved(&'static str),
    Observed { side: Side, width: f64, usage: LiveUsage, expected_rows: Option<u32>, usage_differs: bool },
}

impl LiveSidebar {
    fn read(saved: &Layout, executable: &Path) -> Self {
        let Some(target) = rex::Target::current() else {
            return Self::NotObserved("run Doctor in the Rex tab you want to check");
        };
        let Some(block) = target.block.to_str().filter(|id| id.starts_with("block:")) else {
            return Self::NotObserved("could not find this Rex tab");
        };
        let caller = format!("caller={block}");
        let lua = probe_lua(saved, executable);
        rex::diagnostic_script(&target, &lua, &caller)
            .as_deref()
            .and_then(Self::parse)
            .unwrap_or(Self::NotObserved("could not read this Rex tab"))
    }

    fn parse(reply: &str) -> Option<Self> {
        let reason = match reply {
            "no_tab" => "could not find this Rex tab",
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

fn write_sidebar(out: &mut String, saved: &Layout, scope: Scope, live: LiveSidebar) {
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
        LiveSidebar::NotObserved(_) => None,
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
            return;
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
    }
}

fn on(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

pub(crate) fn repair() -> Result<String, String> {
    let store = store()?;
    if !store.has_layout()? && !store.rex_reload_pending() {
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
        if store.layout() != before || (!store.has_layout()? && !store.rex_reload_pending()) {
            return Err("The Rex layout changed while preparing repair. Run milo doctor again.");
        }
        Ok(())
    })?;
    check_listener(saved)?;
    if layout.enabled()
        && rex::layout_available() != Some(true)
        && (!rex::run(&[OsStr::new("config"), OsStr::new("reload")]) || rex::layout_available() != Some(true))
    {
        return Err(
            "Rex did not load Milo's current actions. Check rex.lua for custom actions and run rex config check."
                .into(),
        );
    }
    if layout.enabled() {
        let file = read(&store.rex_file())?.ok_or("Rex layout is missing.")?;
        if launcher_matches(std::str::from_utf8(&file.bytes).unwrap_or(""), &rex_files::executable()?) != Some(true) {
            return Err(
                "Rex uses a custom launcher. Update its local milo path in rex.lua; Milo left that code alone.".into(),
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
    let claude = match store.claude() {
        Claude::On => Some(false),
        Claude::Invalid => {
            let note = read(&store.state.join(OWN_CLAUDE))
                .map_err(|_| "Could not verify Claude Code ownership. Check access to Milo's state directory.")?;
            if note.is_some() {
                return Err(format!(
                    "Could not remove Milo's Claude Code status line. Make sure {:?} is readable and contains valid JSON, then try again.",
                    store.claude
                ));
            }
            None
        }
        Claude::Off | Claude::Foreign | Claude::NotOwned => None,
    };
    if !rex && claude.is_none() {
        return Ok("No managed integration to remove. Preferences and user settings were left alone.".into());
    }
    let saved = store.save(rex.then_some(&disable), claude, run)?;
    check_listener(saved.listener)?;
    let mut message =
        String::from("Removed managed integrations. Saved preferences, user-owned settings and open panes were left alone.");
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
        }
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
    fn invalid_claude_settings_block_removal_only_if_milo_might_own_the_status_line() {
        let fixture = Fixture::folder();
        let store = fixture_store(&fixture);
        let run = |_: &[&OsStr]| panic!("Claude settings do not need Rex");
        store.save(None, Some(true), &run).unwrap();
        let valid = std::fs::read_to_string(&store.claude).unwrap();
        let invalid = format!("{valid}{{");
        std::fs::write(&store.claude, &invalid).unwrap();
        let note = store.state.join(OWN_CLAUDE);

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
        std::fs::create_dir_all(store.state.join(OWN_CLAUDE)).unwrap(); // Not a readable note, even when run as root.

        let report = report_for(&store, None).unwrap();
        assert!(report.contains(&format!(
            "Claude status line: unknown; check the Claude Code ownership note in Milo's state directory ({:?})",
            store.state
        )));
        assert!(!report.contains("unreadable; check Claude settings"));
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
