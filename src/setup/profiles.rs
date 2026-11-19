//! Semantic Rex rail preferences, independent of any session or block ID.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use super::files::{self, Change, Image};
use super::install::{RunRex, Saved, Store, read};
use super::layout::{Layout, TabPane, owns_actions, probe_lua};
use super::rex_files::{check_config_dir, executable};
use crate::{git, rex};

const BAD_PROFILE: &str = "Could not read the saved Milo layout. It was left alone.";
const NO_WORKTREE: &str = "Open a shell in a Git worktree first.";
const NO_TAB: &str = "Could not find this Rex tab.";
const MIXED_RAIL: &str = "Milo panes must share one side rail, separate from other panes.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Global,
    Worktree,
}

struct Location {
    path: PathBuf,
    owner: String,
}

impl Location {
    fn new(config: &Path, root: Option<&Path>) -> Result<Self, &'static str> {
        let directory = config.join("layouts");
        check_config_dir(config)?;
        check_config_dir(&directory)?;
        let (name, owner) = if let Some(root) = root {
            let bytes = root.as_os_str().as_encoded_bytes();
            let hash = bytes
                .iter()
                .fold(0xcbf2_9ce4_8422_2325, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
            (format!("worktree-{hash:016x}"), hex(bytes))
        } else {
            ("global".into(), "global".into())
        };
        Ok(Self { path: directory.join(name), owner })
    }

    fn lock(&self) -> Result<std::fs::File, &'static str> {
        super::lock::setup(self.path.parent().expect("profile has a directory"))
    }

    fn load(&self) -> Result<Option<(Layout, Image)>, &'static str> {
        let Some(image) = read(&self.path)? else { return Ok(None) };
        let text = std::str::from_utf8(&image.bytes).map_err(|_| BAD_PROFILE)?;
        let (header, code) = text.split_once('\n').ok_or(BAD_PROFILE)?;
        if header.strip_prefix("milo-layout-v1 ") != Some(self.owner.as_str()) {
            return Err(BAD_PROFILE);
        }
        let rail = Layout::parse_rail(code.trim_end_matches('\n')).ok_or(BAD_PROFILE)?;
        Ok(Some((rail, image)))
    }

    fn plan(self, layout: Option<&Layout>, changes: &mut Vec<Change>) -> Result<(), &'static str> {
        let before = self.load()?.map(|(_, image)| image);
        changes.extend(self.change(before, layout));
        Ok(())
    }

    fn change(self, before: Option<Image>, layout: Option<&Layout>) -> Option<Change> {
        let bytes = layout.map(|rail| format!("milo-layout-v1 {}\n{}\n", self.owner, rail.rail_code()).into_bytes());
        Change::new(self.path, before, bytes)
    }
}

fn selected(config: &Path, root: Option<&Path>) -> Result<Option<(Layout, Scope)>, &'static str> {
    if let Some(root) = root
        && let Some((layout, _)) = Location::new(config, Some(root))?.load()?
    {
        return Ok(Some((layout, Scope::Worktree)));
    }
    Ok(Location::new(config, None)?.load()?.map(|(layout, _)| (layout, Scope::Global)))
}

fn worktree(cwd: &Path) -> Option<PathBuf> {
    if !cwd.is_absolute() {
        return None;
    }
    git::worktree(cwd)
}

impl Store {
    pub fn layout_for(&self, root: Option<&Path>) -> Result<(Layout, bool, Scope), &'static str> {
        let (legacy, active) = self.layout()?;
        let new_tabs = legacy.new_tabs;
        let (mut layout, scope) = selected(&self.config, root)?.unwrap_or((legacy, Scope::Global));
        layout.new_tabs = new_tabs;
        Ok((layout, active, scope))
    }

    pub fn save_for(
        &self,
        layout: &Layout,
        scope: Scope,
        root: Option<&Path>,
        claude: Option<bool>,
        run: &RunRex<'_>,
    ) -> Result<Saved, &'static str> {
        self.save_for_with(layout, scope, root, claude, run, |_| Ok(()))
    }

    pub fn save_for_with(
        &self,
        layout: &Layout,
        scope: Scope,
        root: Option<&Path>,
        claude: Option<bool>,
        run: &RunRex<'_>,
        extra: impl FnOnce(&mut Vec<Change>) -> Result<(), &'static str>,
    ) -> Result<Saved, &'static str> {
        let location = Location::new(&self.config, root_for(scope, root)?)?;
        let _profiles = location.lock()?;
        // Keep the legacy choices as the fallback when a global capture is reset.
        let (mut fallback, _) = self.layout()?;
        if scope == Scope::Global {
            fallback.new_tabs = layout.new_tabs;
        }
        if !fallback.enabled() {
            fallback.panes = Layout::default().panes;
        }
        self.save_with(Some(&fallback), claude, run, |changes| {
            if let Some(image) = read(&self.rex_file())?
                && !std::str::from_utf8(&image.bytes).is_ok_and(owns_actions)
            {
                return Err(
                    "Milo's rex.lua is customized. It was left alone; layout profiles need the shipped actions.",
                );
            }
            location.plan(Some(layout), changes)?;
            extra(changes)
        })
    }

    pub fn use_global(&self, root: &Path) -> Result<(), &'static str> {
        update(&self.config, Scope::Worktree, Some(root), None)
    }

    pub fn tab_context(&self, target: &rex::Target) -> (Option<PathBuf>, Option<(usize, usize)>) {
        let Some((cwd, size)) = probe_context(target) else { return (None, None) };
        (cwd.and_then(|cwd| worktree(Path::new(&cwd))), size)
    }

    /// Estimate the full tab in cells from a laid-out terminal, excluding floats.
    pub fn tab_size(&self, target: &rex::Target) -> Option<(usize, usize)> {
        probe_context(target)?.1
    }

    pub fn capture(&self, target: &rex::Target) -> Result<Layout, &'static str> {
        let reply = probe(Some(target), &target.session, &Layout::default(), "milo_operation=capture", &[])?;
        Layout::parse_rail(reply.strip_prefix("ok ").ok_or_else(|| failure(&reply))?).ok_or(MIXED_RAIL)
    }

    pub fn trial(&self, layout: &Layout, target: &rex::Target) -> Result<Trial, &'static str> {
        let result = probe(Some(target), &target.session, layout, "milo_operation=trial", &[])?;
        let mut values = result.split_ascii_whitespace();
        if values.next() != Some("ok") {
            return Err(failure(&result));
        }
        let window = values.next().filter(|id| rex_id(id, "window:")).ok_or(NO_TAB)?.to_owned();
        let temporary = match values.next() {
            Some("floating") => true,
            Some("tiled") => false,
            _ => return Err(NO_TAB),
        };
        let mut blocks = [None, None, None];
        for record in values {
            let (role, id) = record.split_once('=').ok_or(NO_TAB)?;
            let pane = TabPane::ALL.into_iter().find(|pane| pane.name() == role).ok_or(NO_TAB)?;
            if !rex_id(id, "block:") || blocks[pane as usize].replace(id.to_owned()).is_some() {
                return Err(NO_TAB);
            }
        }
        Ok(Trial { session: target.session.clone(), window, blocks, temporary })
    }
}

/// A trial can undo only the exact blocks it created, in their original tab.
pub(super) struct Trial {
    session: OsString,
    window: String,
    blocks: [Option<String>; 3],
    temporary: bool,
}

impl Trial {
    pub fn temporary(&self) -> bool {
        self.temporary
    }

    pub fn undo(&self) -> Result<(), &'static str> {
        self.apply("milo_operation=undo", "Could not undo every pane from this trial.")
    }

    pub fn keep(&self) -> Result<(), &'static str> {
        self.apply("milo_operation=keep", "Saved, but could not finish every pane from the trial.")
    }

    fn apply(&self, operation: &str, failure: &'static str) -> Result<(), &'static str> {
        let mut args = Vec::with_capacity(4);
        args.push(format!("window={}", self.window));
        for (pane, block) in TabPane::ALL.into_iter().zip(&self.blocks) {
            if let Some(block) = block {
                args.push(format!("{}={block}", pane.name()));
            }
        }
        if args.len() == 1 {
            return Ok(());
        }
        let result = probe(None, &self.session, &Layout::default(), operation, &args)?;
        if result == "ok" { Ok(()) } else { Err(failure) }
    }
}

fn failure(reply: &str) -> &'static str {
    match reply {
        "mixed" => MIXED_RAIL,
        "no_rail" => "Add Milo panes in a side rail before remembering this layout.",
        "no_shell" => "Focus the shell Milo should use before trying this layout.",
        "no_panes" => "Choose at least one pane before trying a layout.",
        "no_replay" => "Rex could not add the trial panes.",
        "zoomed" => "Unzoom this tab before trying a layout.",
        "trial" => "Finish or undo the Milo setup trial in this tab first.",
        _ => NO_TAB,
    }
}

fn rex_id(id: &str, kind: &str) -> bool {
    id.strip_prefix(kind)
        .is_some_and(|tail| !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
}

fn probe(
    target: Option<&rex::Target>,
    session: &OsStr,
    layout: &Layout,
    operation: &str,
    args: &[String],
) -> Result<String, &'static str> {
    let script = probe_lua(layout, &executable()?);
    rex::script(target, session, &script, std::iter::once(operation).chain(args.iter().map(String::as_str)))
        .ok_or(NO_TAB)
}

type TabSize = (usize, usize);

fn probe_context(target: &rex::Target) -> Option<(Option<OsString>, Option<TabSize>)> {
    let reported = probe(Some(target), &target.session, &Layout::default(), "milo_operation=context", &[]).ok()?;
    let mut fields = reported.strip_prefix("ok ")?.split_ascii_whitespace();
    let cwd = unhex(fields.next()?);
    let cols = fields.next()?.parse().ok()?;
    let rows = fields.next()?.parse().ok()?;
    Some((cwd, (cols != 0 && rows != 0).then_some((cols, rows))))
}

/// Called after Outline restores the terminal, before its peek block exits.
pub fn return_from_peek(origin: &OsStr) {
    let Some(origin) = origin.to_str().filter(|id| rex_id(id, "block:")) else { return };
    let Some(target) = rex::Target::current() else { return };
    let _ = probe(
        Some(&target),
        &target.session,
        &Layout::default(),
        "milo_operation=return_peek",
        &[format!("origin={origin}")],
    );
}

fn root_for(scope: Scope, root: Option<&Path>) -> Result<Option<&Path>, &'static str> {
    match scope {
        Scope::Global => Ok(None),
        Scope::Worktree => root.map(Some).ok_or(NO_WORKTREE),
    }
}

fn update(config: &Path, scope: Scope, root: Option<&Path>, layout: Option<&Layout>) -> Result<(), &'static str> {
    let location = Location::new(config, root_for(scope, root)?)?;
    let _profiles = location.lock()?;
    let mut changes = Vec::new();
    location.plan(layout, &mut changes)?;
    files::install(&changes, || true)
}

/// A divider drag changes proportions, not the pane choices, side, or policy
/// for the other axis. An older tab must not put those defaults back.
fn remember_resize(config: &Path, root: &Path, before: &Layout, after: &Layout) -> Result<(), &'static str> {
    if before.panes != after.panes
        || before.geometry.order != after.geometry.order
        || before.geometry.side != after.geometry.side
    {
        return Err("The Milo rail changed.");
    }
    let width_changed = before.width != after.width;
    let shares_changed = before.geometry.shares != after.geometry.shares;
    if !width_changed && !shares_changed {
        return Ok(());
    }

    let location = Location::new(config, Some(root))?;
    let _profiles = location.lock()?;
    let (mut layout, image) = if let Some((layout, image)) = location.load()? {
        (layout, Some(image))
    } else if let Some((layout, _)) = Location::new(config, None)?.load()? {
        (layout, None)
    } else {
        let image = read(&config.join("rex.lua"))?.ok_or(BAD_PROFILE)?;
        let layout = std::str::from_utf8(&image.bytes).ok().and_then(Layout::parse).ok_or(BAD_PROFILE)?;
        (layout, None)
    };
    let write_width = width_changed && layout.width != after.width;
    let write_shares = shares_changed && layout.panes == after.panes && layout.geometry.shares != after.geometry.shares;
    if !write_width && !write_shares {
        return Ok(());
    }
    if write_width {
        layout.width.clone_from(&after.width);
    }
    if write_shares {
        layout.geometry.shares = after.geometry.shares;
    }
    let change = location.change(image, Some(&layout));
    files::install(change.as_slice(), || true)
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").expect("writing to a string");
    }
    result
}

fn unhex(text: &str) -> Option<OsString> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for i in (0..text.len()).step_by(2) {
        bytes.push(u8::from_str_radix(&text[i..i + 2], 16).ok()?);
    }
    Some(OsString::from_vec(bytes))
}

/// Internal, noninteractive entry point for the installed Rex actions. Paths are arguments, never Lua.
pub fn layout_profile(args: &[OsString]) -> i32 {
    fn run(args: &[OsString]) -> Result<(), &'static str> {
        let [action, rest @ ..] = args else { return Err("Missing layout-profile operation.") };
        match (action.to_str(), rest) {
            (Some("worktree"), [cwd]) => {
                worktree(Path::new(cwd)).ok_or(NO_WORKTREE)?;
            }
            (Some("read"), [cwd]) => {
                let config = crate::paths::config_dir().ok_or(BAD_PROFILE)?;
                if let Some((layout, _)) = selected(&config, worktree(Path::new(cwd)).as_deref())? {
                    println!("{}", layout.rail_code());
                }
            }
            (Some("resize"), [before, after, cwd, other_dirs @ ..]) => {
                let root = worktree(Path::new(cwd)).ok_or(NO_WORKTREE)?;
                if other_dirs.iter().any(|dir| worktree(Path::new(dir)).as_deref() != Some(root.as_path())) {
                    return Err("Milo panes belong to different Git worktrees.");
                }
                let config = crate::paths::config_dir().ok_or(BAD_PROFILE)?;
                let parse =
                    |value: &OsString| value.to_str().and_then(Layout::parse_rail).ok_or("Invalid Milo rail layout.");
                remember_resize(&config, &root, &parse(before)?, &parse(after)?)?;
            }
            (Some("save"), [scope, cwd, _]) | (Some("reset"), [scope, cwd]) => {
                let scope = match scope.to_str() {
                    Some("global") => Scope::Global,
                    Some("worktree") => Scope::Worktree,
                    _ => return Err("Choose global or worktree scope."),
                };
                let root =
                    if scope == Scope::Worktree { Some(worktree(Path::new(cwd)).ok_or(NO_WORKTREE)?) } else { None };
                let config = crate::paths::config_dir().ok_or("Could not find Milo's settings folder.")?;
                let layout = if let [_, _, code] = rest {
                    Some(code.to_str().and_then(Layout::parse_rail).ok_or("Invalid Milo rail layout.")?)
                } else {
                    None
                };
                update(&config, scope, root.as_deref(), layout.as_ref())?;
            }
            _ => return Err("Invalid layout-profile arguments."),
        }
        Ok(())
    }
    match run(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn store(fixture: &Fixture) -> Store {
        Store {
            config: fixture.root.join("config"),
            state: fixture.root.join("state"),
            claude: fixture.root.join("claude"),
            init: fixture.root.join("init"),
        }
    }

    #[test]
    fn preferences_resolve_worktree_before_global_and_reset_is_local() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let a = fixture.root.join("a");
        let b = fixture.root.join("b");
        let global = Layout { panes: [true, false, false], ..Layout::default() };
        let project = Layout::parse_rail("v1 r .231 .7 10 uho 011 0 417000 583000").unwrap();
        update(&store.config, Scope::Global, None, Some(&global)).unwrap();
        update(&store.config, Scope::Worktree, Some(&a), Some(&project)).unwrap();
        let a_layout = selected(&store.config, Some(&a)).unwrap().unwrap();
        assert!(a_layout.0 == project);
        assert_eq!(a_layout.1, Scope::Worktree);
        assert_eq!(selected(&store.config, Some(&b)).unwrap().unwrap().1, Scope::Global);
        store.use_global(&a).unwrap();
        assert_eq!(selected(&store.config, Some(&a)).unwrap().unwrap().1, Scope::Global);
    }

    #[test]
    fn divider_changes_only_its_axis_and_worktree() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let a = fixture.root.join("a");
        let b = fixture.root.join("b");
        let global = Layout::parse_rail("v1 r .321 .7 10 uho 111").unwrap();
        update(&store.config, Scope::Global, None, Some(&global)).unwrap();
        let before = Layout::parse_rail("v1 l .2 .7 10 ohu 111 540000 260000 200000").unwrap();
        let height = Layout::parse_rail("v1 l .2 .7 10 ohu 111 660000 320000 20000").unwrap();
        let width = Layout::parse_rail("v1 l .45 .7 10 ohu 111 540000 260000 200000").unwrap();

        remember_resize(&store.config, &a, &before, &before).unwrap();
        assert_eq!(selected(&store.config, Some(&a)).unwrap().unwrap().1, Scope::Global);
        remember_resize(&store.config, &a, &before, &height).unwrap();
        let (first, scope) = selected(&store.config, Some(&a)).unwrap().unwrap();
        assert_eq!(scope, Scope::Worktree);
        assert_eq!(first.width, global.width);
        assert_eq!(first.geometry.side, global.geometry.side);
        assert_eq!(first.geometry.order, global.geometry.order);
        assert_eq!(first.geometry.shares, height.geometry.shares);

        remember_resize(&store.config, &a, &before, &width).unwrap();
        let resized = selected(&store.config, Some(&a)).unwrap().unwrap().0;
        assert_eq!(resized.width, width.width);
        assert_eq!(resized.geometry.shares, height.geometry.shares);
        assert!(selected(&store.config, Some(&b)).unwrap().unwrap().0 == global);
        store.use_global(&a).unwrap();
        assert!(selected(&store.config, Some(&a)).unwrap().unwrap().0 == global);
    }

    #[test]
    fn a_partial_rail_resize_does_not_replace_configured_panes_or_height_policy() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let root = fixture.root.join("project");
        let default = Layout::default();
        std::fs::create_dir(&store.config).unwrap();
        std::fs::write(store.rex_file(), super::super::layout::rex_lua(&default, Path::new("/bin/milo"))).unwrap();
        let before = Layout::parse_rail("v1 l .2 .7 10 uho 101 800000 0 200000").unwrap();
        let after = Layout::parse_rail("v1 l .33 .7 10 uho 101 900000 0 100000").unwrap();

        remember_resize(&store.config, &root, &before, &after).unwrap();
        let (resized, scope) = selected(&store.config, Some(&root)).unwrap().unwrap();
        assert_eq!(scope, Scope::Worktree);
        assert_eq!(resized.width, after.width);
        assert_eq!(resized.panes, default.panes);
        assert_eq!(resized.geometry, default.geometry);
    }

    #[test]
    fn reload_failure_rolls_back_a_scoped_save_and_integration_files() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let first = Layout { panes: [true, false, false], ..Layout::default() };
        update(&store.config, Scope::Global, None, Some(&first)).unwrap();
        let replacement = Layout { panes: [false, true, true], ..Layout::default() };
        let result = store.save_for(&replacement, Scope::Global, None, None, &|args| {
            args.starts_with(&[OsStr::new("config"), OsStr::new("check")])
        });
        assert!(result.is_err());
        assert!(!store.init.exists() && !store.rex_file().exists());
        assert!(store.layout_for(None).unwrap().0 == first);
    }

    #[test]
    fn a_profile_with_invalid_ownership_is_never_replaced_or_reset() {
        let fixture = Fixture::folder();
        let store = store(&fixture);
        let profile = Location::new(&store.config, None).unwrap();
        let _lock = profile.lock().unwrap();
        let bytes = b"not Milo's settings\n";
        std::fs::write(&profile.path, bytes).unwrap();
        let mut changes = Vec::new();
        assert_eq!(profile.plan(None, &mut changes), Err(BAD_PROFILE));
        assert!(changes.is_empty());
    }
}
