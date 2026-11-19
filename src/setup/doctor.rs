//! Inspect the current installation without starting Rex. Repair and removal
//! are explicit operations using the same ownership checks and rollback as setup.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::Path;

use super::claude::Claude;
use super::install::{Saved, Store, read};
use super::layout::{Layout, launcher_matches, owns_actions};
use super::{lock, rex_files};
use crate::preferences::{IconMode, Preferences, Source, worktree};
use crate::{rex, usage};

fn store() -> Result<Store, String> {
    Store::real().ok_or_else(|| "Could not locate your settings folders. Check HOME and XDG_CONFIG_HOME.".into())
}

pub(crate) fn report() -> Result<String, String> {
    let store = store()?;
    let root = worktree(Path::new("."));
    report_for(&store, root.as_deref())
}

pub(super) fn report_for(store: &Store, root: Option<&Path>) -> Result<String, String> {
    let mut out = String::from("Milo doctor\n");
    if let Some(path) = root {
        writeln!(out, "Worktree: {path:?}").unwrap();
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
            let width = rail.width.parse::<f64>().unwrap_or_default() * 100.0;
            write!(out, "Rail: {scope:?}, {:?}, {width:.1}% wide;", rail.geometry.side).unwrap();
            for role in rail.geometry.order.into_iter().filter(|role| rail.chosen(*role)) {
                write!(out, " {}", role.name()).unwrap();
            }
            out.push('\n');
            let executable = rex_files::executable()?;
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
    let expected = layout.is_ok_and(|(_, active, _)| active);
    writeln!(
        out,
        "Layout listener: {}",
        match (lock::running(&store.state), expected) {
            (Ok(true), true) => "running",
            (Ok(false), false) => "off",
            (Ok(false), true) => "not running; run milo setup --repair",
            (Ok(true), false) => "running unexpectedly; run milo listen --stop",
            (Err(_), _) => "unknown; check access to Milo's state directory",
        }
    )
    .unwrap();
    writeln!(
        out,
        "Claude status line: {}",
        match store.claude() {
            Claude::Off => "not installed",
            Claude::On => "managed by Milo",
            Claude::Foreign | Claude::NotOwned => "user-managed; left alone",
            Claude::Invalid => "unreadable; check Claude settings",
        }
    )
    .unwrap();
    for (provider, source) in usage::sources() {
        writeln!(out, "{provider} limits: {}", source.unwrap_or("no local report; see the usage pane for setup hints"))
            .unwrap();
    }
    out.pop();
    Ok(out)
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
    let store = store()?;
    let disable = Layout { panes: [false; 3], new_tabs: false, ..Layout::default() };
    let rex = store.has_layout()?
        || store.rex_file().try_exists().map_err(|e| e.to_string())?
        || store.listener_file().try_exists().map_err(|e| e.to_string())?
        || store.rex_reload_pending()
        || lock::running(&store.state).map_err(|e| e.to_string())?;
    let claude = (store.claude() == Claude::On).then_some(false);
    if !rex && claude.is_none() {
        return Ok("No managed integration to remove. Preferences and user settings were left alone.".into());
    }
    check_listener(store.save(rex.then_some(&disable), claude, &rex::run)?)?;
    Ok("Removed managed integrations. Saved preferences, user-owned settings and open panes were left alone.".into())
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
