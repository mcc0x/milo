//! Carries chosen pane preferences and layouts from one host to another as a
//! text bundle, and takes the last import back.
//!
//! A *bundle* holds only the groups someone chose to export: appearance,
//! workflow, usage display and layout, for the global defaults and for one
//! project. It never holds a token, an alert destination, an account choice,
//! an integration switch or anything from the state folder, and an import
//! applies none of those even when a bundle names them.
//!
//! A project is known across hosts by where its history starts, not by its
//! path. An import applies a project's section to a worktree here only when
//! that matches or when the person names the worktree.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Component, Path, PathBuf};

use super::files::{self, Change, Image};
use super::install::Store;
use super::layout::{Layout, Side, TabPane};
use super::lock;
use super::profiles::profile_edit;
use crate::preferences::{self, Preferences};
use crate::rex::json as quoted;
use crate::{git, paths};

/// A bundle is a few dozen short lines. Anything much longer is not one.
pub(crate) const BUNDLE_LIMIT: usize = 32 * 1024;
const HEADER: &str = "milo-settings";
const VERSION: u32 = 1;
const MAX_PROJECTS: usize = 16;
const JOURNAL: &str = "settings-import-undo";
const JOURNAL_HEADER: &str = "milo-settings-undo 1";
const NOT_A_BUNDLE: &str = "This is not a Milo settings bundle. Export one with milo settings export.";
const NEWER_BUNDLE: &str = "This bundle was written by a newer Milo. Update Milo on this host, then import it again.";
pub(crate) const STALE: &str = "Settings on this host changed after the preview. Preview the import again.";
const NOTHING_TO_UNDO: &str = "There is no import to take back on this host.";
const CHANGED_SINCE_IMPORT: &str =
    "A setting was changed after the import, so the import was not taken back. Nothing was changed.";
pub(crate) const HELP: &str = "Usage: milo settings export GROUPS [--scope global|worktree|all] [--output FILE]\n       milo settings import FILE [--map NAME=DIR]... [--apply]\n       milo settings undo\n\nCopy chosen pane settings to another host as a text file.\n\nGROUPS is a comma-separated choice of appearance, workflow, usage and layout, or all. --scope picks the global defaults, this worktree's own settings, or both (the default is global). The file goes to standard output unless --output names a new file.\n\nimport shows what FILE would change here and changes nothing; add --apply to make the changes. A project in the file is applied to a worktree here only when both start from the same commit, or when you name the worktree with --map NAME=DIR. FILE may be - for standard input.\n\nundo takes back the last import, unless a setting it changed has been changed again.\n\nA file never holds tokens, alert destinations, accounts, integration switches or state. Set those up on each host.";

/// A group of choices someone can pick to export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Group {
    Appearance,
    Workflow,
    Usage,
    Layout,
}

impl Group {
    pub(crate) const ALL: [Self; 4] = [Self::Appearance, Self::Workflow, Self::Usage, Self::Layout];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Workflow => "workflow",
            Self::Usage => "usage",
            Self::Layout => "layout",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|group| group.name() == name)
    }

    /// The preference keys of the group. The layout is one line of its own.
    const fn keys(self) -> &'static [&'static str] {
        match self {
            Self::Appearance => &["icons", "static_marks", "text_git_marks"],
            Self::Workflow => &["project_mode", "follow_activity", "changes_only", "history_follow_new"],
            Self::Usage => &["usage_large_figures", "usage_codex", "usage_claude", "usage_opencode"],
            Self::Layout => &[],
        }
    }
}

const LAYOUT_KEY: &str = "layout";

/// The key as a bundle and a preferences file write it, if it may travel.
fn travelling_key(key: &str) -> Option<&'static str> {
    Group::ALL.into_iter().flat_map(|group| group.keys()).copied().find(|known| *known == key)
}

fn label(key: &str) -> &'static str {
    match key {
        "icons" => "Icons",
        "static_marks" => "Static activity marks",
        "text_git_marks" => "Git text marks",
        "project_mode" => "Project",
        "follow_activity" => "Follow writes",
        "changes_only" => "Changed files only",
        "history_follow_new" => "Follow new commits",
        "usage_large_figures" => "Large usage figures",
        "usage_codex" => "Codex in Usage",
        "usage_claude" => "Claude Code in Usage",
        "usage_opencode" => "OpenCode in Usage",
        _ => "Layout",
    }
}

/// Which saved settings an export takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scopes {
    Global,
    Worktree,
    All,
}

impl Scopes {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "global" => Self::Global,
            "worktree" => Self::Worktree,
            "all" => Self::All,
            _ => return None,
        })
    }
}

/// The settings of this host: where they are kept and which worktree is open.
pub(crate) struct Place<'a> {
    pub config: &'a Path,
    pub state: &'a Path,
    pub home: Option<&'a Path>,
    /// The checkout root for export, or the directory to match against for import.
    pub worktree: Option<&'a Path>,
}

/// How a bundle names a project, without its path on the host it came from.
#[derive(Debug, Default, PartialEq, Eq)]
struct Project {
    name: String,
    /// The folder below the home folder, where the project was under one.
    home: Option<String>,
    /// The commit the project's history starts from.
    repository: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Section {
    project: Option<Project>,
    choices: Vec<(&'static str, String)>,
    layout: Option<String>,
    /// Lines an import leaves out, in the words shown for them.
    skipped: Vec<String>,
}

/// One printable line, or nothing: a name from another host is never shown with controls in it.
fn plain(text: &str, max: usize) -> Option<&str> {
    (!text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)).then_some(text)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string");
    }
    out
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    let digit = |byte: u8| char::from(byte).to_digit(16).and_then(|digit| u8::try_from(digit).ok());
    let (pairs, odd) = text.as_bytes().as_chunks::<2>();
    odd.is_empty().then(|| pairs.iter().map(|&[high, low]| Some(digit(high)? << 4 | digit(low)?)).collect()).flatten()
}

fn oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A path that stays below the folder it is joined to.
fn relative(text: &str) -> bool {
    plain(text, 1024).is_some() && Path::new(text).components().all(|part| matches!(part, Component::Normal(_)))
}

fn describe(layout: &Layout) -> String {
    let panes: Vec<&str> = layout
        .geometry
        .order
        .into_iter()
        .filter(|pane| layout.chosen(*pane))
        .map(|pane| match pane {
            TabPane::Outline => "Outline",
            TabPane::History => "History",
            TabPane::Usage => "Usage",
        })
        .collect();
    if panes.is_empty() {
        return "no panes".into();
    }
    let side = if layout.geometry.side == Side::Left { "left" } else { "right" };
    let width =
        layout.width.parse::<f64>().map_or_else(|_| layout.width.clone(), |width| format!("{:.0}%", width * 100.0));
    format!("{} on the {side}, {width} wide", panes.join(", "))
}

fn shown(value: &str) -> &str {
    match value {
        "true" => "on",
        "false" => "off",
        other => other,
    }
}

/// Writes the chosen groups of this host's saved settings as a bundle.
pub(crate) fn export(place: &Place<'_>, groups: &[Group], scopes: Scopes) -> Result<String, String> {
    if groups.is_empty() {
        return Err("Choose what to export: appearance, workflow, usage, layout, or all.".into());
    }
    let mut out = format!(
        "{HEADER} {VERSION}\n# Milo {} pane settings: {}.\n# No tokens, alert destinations, accounts, integrations or state are in this file.\n",
        env!("CARGO_PKG_VERSION"),
        groups.iter().map(|group| group.name()).collect::<Vec<_>>().join(", "),
    );
    let layout_line = |root: Option<&Path>| -> Result<Option<String>, String> {
        if !groups.contains(&Group::Layout) {
            return Ok(None);
        }
        let (saved, _) = profile_edit(place.config, root, None)?;
        Ok(saved.map(|layout| layout.rail_code()))
    };
    let mut sections = 0;
    if scopes != Scopes::Worktree {
        let (prefs, _) = Preferences::resolve_in(place.config, None)
            .map_err(|_| "Could not read the global preferences. Run milo doctor.".to_owned())?;
        out.push_str("\n[global]\n");
        for key in groups.iter().flat_map(|group| group.keys()) {
            writeln!(out, "{key} = {}", prefs.value_of(key).expect("a travelling key is a preference")).unwrap();
        }
        if let Some(code) = layout_line(None)? {
            writeln!(out, "{LAYOUT_KEY} = {code}").unwrap();
        }
        sections += 1;
    }
    if scopes != Scopes::Global {
        let root = place.worktree.ok_or("Run this in a Git worktree to export that worktree's settings.")?;
        let layer = preferences::read(&preferences::layer_file(place.config, Some(root)), BUNDLE_LIMIT as u64)
            .map_err(|_| "Could not read this worktree's preferences. Run milo doctor.".to_owned())?
            .unwrap_or_default();
        let mut body = String::new();
        // A worktree holds only what it overrides, and only that is carried.
        for key in groups.iter().flat_map(|group| group.keys()) {
            if let Some(value) =
                preferences::layer_value(&layer, key).filter(|value| preferences::valid_choice(key, value))
            {
                writeln!(body, "{key} = {value}").unwrap();
            }
        }
        if let Some(code) = layout_line(Some(root))? {
            writeln!(body, "{LAYOUT_KEY} = {code}").unwrap();
        }
        if !body.is_empty() {
            let name = root.file_name().and_then(OsStr::to_str).and_then(|name| plain(name, 128)).unwrap_or("project");
            writeln!(out, "\n[project]\nname = {name}").unwrap();
            let below_home = place.home.and_then(|home| root.strip_prefix(home).ok()).and_then(Path::to_str);
            if let Some(home) = below_home.filter(|path| relative(path)) {
                writeln!(out, "home = {home}").unwrap();
            }
            if let Some(commit) = git::root_commit(root) {
                writeln!(out, "repository = {commit}").unwrap();
            }
            out.push_str(&body);
            sections += 1;
        } else if scopes == Scopes::Worktree {
            return Err(
                "This worktree has no settings of its own in the chosen groups. It uses the global defaults.".into()
            );
        }
    }
    if sections == 0 { Err("There is nothing to export.".into()) } else { Ok(out) }
}

/// Reads a bundle. Only a line it knows and may carry becomes a change; every other line is named as left out.
fn parse(text: &str) -> Result<Vec<Section>, &'static str> {
    if text.len() > BUNDLE_LIMIT || text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        return Err(NOT_A_BUNDLE);
    }
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#'));
    let version =
        lines.next().and_then(|line| line.strip_prefix(HEADER)).and_then(|rest| rest.trim().parse::<u32>().ok());
    match version.ok_or(NOT_A_BUNDLE)? {
        VERSION => {}
        0 => return Err(NOT_A_BUNDLE),
        _ => return Err(NEWER_BUNDLE),
    }
    let mut sections: Vec<Section> = Vec::new();
    for line in lines {
        match line {
            "[global]" if sections.iter().all(|section| section.project.is_some()) => sections.push(Section::default()),
            "[project]" if sections.iter().filter(|section| section.project.is_some()).count() < MAX_PROJECTS => {
                sections.push(Section { project: Some(Project::default()), ..Section::default() });
            }
            "[global]" | "[project]" => return Err(NOT_A_BUNDLE),
            _ => {
                let (key, value) = line.split_once('=').ok_or(NOT_A_BUNDLE)?;
                let (key, value) = (key.trim(), value.trim());
                let section = sections.last_mut().ok_or(NOT_A_BUNDLE)?;
                read_line(section, plain(key, 64).ok_or(NOT_A_BUNDLE)?, value)?;
            }
        }
    }
    for section in &mut sections {
        if let Some(project) = &mut section.project
            && project.name.is_empty()
        {
            project.name.push_str("project");
        }
    }
    if sections.is_empty() { Err(NOT_A_BUNDLE) } else { Ok(sections) }
}

fn read_line(section: &mut Section, key: &str, value: &str) -> Result<(), &'static str> {
    if let Some(project) = &mut section.project {
        match key {
            "name" => {
                let name = plain(value, 128).filter(|name| !name.contains('/')).ok_or(NOT_A_BUNDLE)?;
                project.name = name.to_owned();
                return Ok(());
            }
            "home" => {
                project.home = Some(value).filter(|path| relative(path)).map(str::to_owned);
                return Ok(());
            }
            "repository" => {
                project.repository = Some(value).filter(|commit| oid(commit)).map(str::to_owned);
                return Ok(());
            }
            _ => {}
        }
    }
    if key == LAYOUT_KEY {
        match Layout::parse_rail(value) {
            Some(layout) => section.layout = Some(layout.rail_code()),
            None => section.skipped.push("layout (written in a form this Milo does not know)".into()),
        }
    } else if let Some(known) = travelling_key(key) {
        if preferences::valid_choice(known, value) {
            section.choices.retain(|(earlier, _)| *earlier != known);
            section.choices.push((known, value.to_owned()));
        } else {
            section.skipped.push(format!("{} (a choice this Milo does not know)", label(known)));
        }
    } else if Preferences::default().value_of(key).is_some() {
        // An account or an integration switch is turned on where it is used.
        section.skipped.push(format!("{key} (set on each host, never imported)"));
    } else {
        section.skipped.push(format!("{key} (not a setting this Milo knows)"));
    }
    Ok(())
}

/// Where a section of a bundle would be saved here.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Destination {
    Global,
    Worktree {
        root: PathBuf,
        matched: WorktreeMatch,
    },
    /// A project that no worktree here was matched to.
    Unmapped,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WorktreeMatch {
    SameRepository,
    Chosen { device: u64, inode: u64, first_commit: Option<String> },
}

impl WorktreeMatch {
    const fn reason(&self) -> &'static str {
        match self {
            Self::SameRepository => "same repository",
            Self::Chosen { .. } => "chosen by you",
        }
    }
}

/// One setting an import would change, in the words shown for it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub label: &'static str,
    pub from: String,
    pub to: String,
}

#[derive(Debug)]
pub(crate) struct SectionPlan {
    /// The project's name in the bundle, or `None` for the global defaults.
    pub project: Option<String>,
    pub destination: Destination,
    pub rows: Vec<Row>,
    /// How many of the section's settings already read the same here.
    pub unchanged: usize,
    pub skipped: Vec<String>,
}

/// What an import would do, worked out against the files as they are now.
pub(crate) struct Plan {
    pub sections: Vec<SectionPlan>,
    changes: Vec<Change>,
}

impl Plan {
    pub(crate) fn changes(&self) -> usize {
        self.sections.iter().map(|section| section.rows.len()).sum()
    }

    /// Names the exact files this plan was worked out against, what it would
    /// write, and which checkout the person explicitly chose.
    pub(crate) fn basis(&self) -> String {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut feed = |bytes: &[u8]| {
            for &byte in bytes.iter().chain(&[0xff]) {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        };
        for change in &self.changes {
            let (before, after) = change.images();
            feed(change.path().as_os_str().as_bytes());
            feed(before.map_or(&[][..], |image| &image.bytes));
            feed(&[u8::from(before.is_some())]);
            feed(after.map_or(&[][..], |image| &image.bytes));
        }
        for section in &self.sections {
            if let Destination::Worktree { root, matched: WorktreeMatch::Chosen { device, inode, first_commit } } =
                &section.destination
            {
                feed(root.as_os_str().as_bytes());
                feed(&device.to_le_bytes());
                feed(&inode.to_le_bytes());
                feed(first_commit.as_deref().unwrap_or("").as_bytes());
            }
        }
        format!("{hash:016x}")
    }

    /// The plan as someone reads it in a terminal.
    pub(crate) fn report(&self, file: &str) -> String {
        let mut out = String::new();
        for section in &self.sections {
            match (&section.project, &section.destination) {
                (None, _) => out.push_str("Global defaults\n"),
                (Some(name), Destination::Worktree { root, matched }) => {
                    writeln!(out, "Project {name} → {} ({})", root.display(), matched.reason()).unwrap()
                }
                (Some(name), _) => {
                    writeln!(
                        out,
                        "Project {name}: not applied. No worktree here starts from the same commit.\n  To choose one: milo settings import {file} --map {name}=DIR"
                    )
                    .unwrap();
                }
            }
            if section.destination != Destination::Unmapped {
                for row in &section.rows {
                    writeln!(out, "  {}: {} → {}", row.label, row.from, row.to).unwrap();
                }
                if section.unchanged > 0 {
                    let (count, verb) = (
                        section.unchanged,
                        if section.unchanged == 1 { "setting already matches" } else { "settings already match" },
                    );
                    writeln!(out, "  {count} {verb}").unwrap();
                }
            }
            for skipped in &section.skipped {
                writeln!(out, "  Left out: {skipped}").unwrap();
            }
        }
        out
    }

    /// The plan as the browser editor shows it.
    pub(crate) fn json(&self) -> String {
        let mut out = format!("{{\"basis\":\"{}\",\"changes\":{},\"sections\":[", self.basis(), self.changes());
        for (index, section) in self.sections.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            let (kind, path, why) = match &section.destination {
                Destination::Global => ("global", None, None),
                Destination::Worktree { root, matched } => {
                    ("worktree", Some(root.to_string_lossy()), Some(matched.reason()))
                }
                Destination::Unmapped => ("unmapped", None, None),
            };
            let optional = |text: Option<&str>| text.map_or_else(|| "null".to_owned(), quoted);
            write!(
                out,
                "{{\"kind\":\"{kind}\",\"project\":{},\"path\":{},\"why\":{},\"unchanged\":{},\"rows\":[",
                optional(section.project.as_deref()),
                optional(path.as_deref()),
                optional(why),
                section.unchanged,
            )
            .unwrap();
            for (index, row) in section.rows.iter().enumerate() {
                let comma = if index == 0 { "" } else { "," };
                write!(
                    out,
                    "{comma}{{\"label\":\"{}\",\"from\":{},\"to\":{}}}",
                    row.label,
                    quoted(&row.from),
                    quoted(&row.to)
                )
                .unwrap();
            }
            out.push_str("],\"skipped\":[");
            for (index, skipped) in section.skipped.iter().enumerate() {
                let comma = if index == 0 { "" } else { "," };
                write!(out, "{comma}{}", quoted(skipped)).unwrap();
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }
}

/// The worktree here for a project of a bundle: the one named for it, or one
/// whose history starts from the same commit. A name or a path alone never matches.
fn destination(place: &Place<'_>, project: &Project, maps: &[(String, PathBuf)]) -> Result<Destination, String> {
    if let Some((_, dir)) = maps.iter().find(|(name, _)| *name == project.name) {
        let root =
            git::worktree(dir).ok_or_else(|| format!("{} is not in a Git worktree on this host.", dir.display()))?;
        // The path alone could now name a different checkout. History also
        // catches a repository replaced in place; when no root commit is
        // available (unborn or shallow), a manual choice still works.
        let folder = fs::metadata(&root).map_err(|_| format!("Could not read the worktree at {}.", root.display()))?;
        let matched =
            WorktreeMatch::Chosen { device: folder.dev(), inode: folder.ino(), first_commit: git::root_commit(&root) };
        return Ok(Destination::Worktree { root, matched });
    }
    let Some(commit) = &project.repository else { return Ok(Destination::Unmapped) };
    let below_home = place.home.zip(project.home.as_deref()).map(|(home, path)| home.join(path));
    let candidates = place.worktree.map(Path::to_path_buf).into_iter().chain(below_home);
    for candidate in candidates {
        if let Some(root) = git::worktree(&candidate)
            && git::root_commit(&root).as_ref() == Some(commit)
        {
            return Ok(Destination::Worktree { root, matched: WorktreeMatch::SameRepository });
        }
    }
    Ok(Destination::Unmapped)
}

/// Works out what importing `bundle` would change here. It writes nothing.
pub(crate) fn plan(place: &Place<'_>, bundle: &str, maps: &[(String, PathBuf)]) -> Result<Plan, String> {
    let sections = parse(bundle)?;
    if let Some((name, _)) = maps.iter().find(|(name, _)| {
        !sections.iter().any(|section| section.project.as_ref().is_some_and(|project| project.name == *name))
    }) {
        return Err(format!("The bundle has no project named {name}."));
    }
    let mut plan = Plan { sections: Vec::new(), changes: Vec::new() };
    let mut roots: Vec<PathBuf> = Vec::new();
    for section in sections {
        let destination = match &section.project {
            None => Destination::Global,
            Some(project) => destination(place, project, maps)?,
        };
        let root = match &destination {
            Destination::Global => None,
            Destination::Worktree { root, .. } if roots.contains(root) => {
                return Err(format!(
                    "Two projects in the bundle would be saved to {}. Map each to its own worktree.",
                    root.display()
                ));
            }
            Destination::Worktree { root, .. } => {
                roots.push(root.clone());
                Some(root.as_path())
            }
            Destination::Unmapped => {
                plan.sections.push(SectionPlan {
                    project: section.project.map(|project| project.name),
                    destination,
                    rows: Vec::new(),
                    unchanged: 0,
                    skipped: section.skipped,
                });
                continue;
            }
        };
        let (rows, unchanged) = plan_section(place, &section, root, &mut plan.changes)?;
        plan.sections.push(SectionPlan {
            project: section.project.map(|project| project.name),
            destination,
            rows,
            unchanged,
            skipped: section.skipped,
        });
    }
    Ok(plan)
}

fn plan_section(
    place: &Place<'_>,
    section: &Section,
    root: Option<&Path>,
    changes: &mut Vec<Change>,
) -> Result<(Vec<Row>, usize), String> {
    const UNREADABLE: &str = "Could not read the preferences saved on this host. Run milo doctor, then import again.";
    let mut rows = Vec::new();
    let mut unchanged = 0;
    let path = preferences::layer_file(place.config, root);
    let image = Image::read(&path).map_err(|_| UNREADABLE)?;
    let old = match &image {
        Some(image) => std::str::from_utf8(&image.bytes).map_err(|_| UNREADABLE)?,
        None => "",
    };
    // The global layer is compared as it takes effect; a worktree by what it overrides.
    let global = if root.is_none() {
        Some(Preferences::resolve_in(place.config, None).map_err(|_| UNREADABLE)?.0)
    } else {
        None
    };
    let mut edits = Vec::new();
    for (key, value) in &section.choices {
        let current = match &global {
            Some(prefs) => prefs.value_of(key),
            None => preferences::layer_value(old, key),
        };
        if current == Some(value.as_str()) {
            unchanged += 1;
        } else {
            let from = current.map_or("inherited", shown).to_owned();
            rows.push(Row { label: label(key), from, to: shown(value).to_owned() });
            edits.push((*key, Some(value.as_str())));
        }
    }
    if !edits.is_empty() {
        let text = preferences::edited(old, &edits).map_err(|_| UNREADABLE)?;
        changes.extend(Change::new(path, image, Some(text.into_bytes())));
    }
    if let Some(code) = &section.layout {
        let layout = Layout::parse_rail(code).expect("a parsed bundle holds a valid layout");
        let (current, change) = profile_edit(place.config, root, Some(&layout))?;
        match change {
            Some(change) => {
                let from = current.as_ref().map_or_else(|| "not saved".to_owned(), describe);
                rows.push(Row { label: label(LAYOUT_KEY), from, to: describe(&layout) });
                changes.push(change);
            }
            None => unchanged += 1,
        }
    }
    Ok((rows, unchanged))
}

fn journal(changes: &[Change]) -> Vec<u8> {
    let mut out = String::from(JOURNAL_HEADER);
    out.push('\n');
    let image = |image: Option<&Image>| {
        image.map_or_else(|| "-".to_owned(), |image| format!("{:o}:{}", image.mode, hex(&image.bytes)))
    };
    for change in changes {
        let (before, after) = change.images();
        writeln!(out, "{}\t{}\t{}", hex(change.path().as_os_str().as_bytes()), image(after), image(before)).unwrap();
    }
    out.into_bytes()
}

/// The edits that take a journal's import back. Each applies only to a file
/// that still reads as the import left it, and only below the settings folder.
fn reversal(place: &Place<'_>, text: &str) -> Option<Vec<Change>> {
    let image = |field: &str| -> Option<Option<Image>> {
        if field == "-" {
            return Some(None);
        }
        let (mode, bytes) = field.split_once(':')?;
        Some(Some(Image {
            bytes: unhex(bytes)?,
            mode: u32::from_str_radix(mode, 8).ok().filter(|mode| *mode <= 0o7777)?,
        }))
    };
    let mut lines = text.lines();
    if lines.next()? != JOURNAL_HEADER {
        return None;
    }
    let mut changes = Vec::new();
    for line in lines {
        let mut fields = line.split('\t');
        let (path, written, previous) = (fields.next()?, fields.next()?, fields.next()?);
        let path = PathBuf::from(OsString::from_vec(unhex(path)?));
        let inside = path
            .strip_prefix(place.config)
            .is_ok_and(|rest| rest.components().all(|part| matches!(part, Component::Normal(_))));
        if fields.next().is_some() || !inside {
            return None;
        }
        changes.extend(Change::between(path, image(written)?, image(previous)?));
    }
    Some(changes)
}

/// Makes the plan's changes, all or none, and keeps what they replaced for one undo.
/// It fails without writing if a file is no longer what the plan was worked out against.
pub(crate) fn apply(place: &Place<'_>, plan: Plan) -> Result<usize, String> {
    let count = plan.changes();
    if plan.changes.is_empty() {
        return Ok(0);
    }
    let _lock = lock::setup(place.state)?;
    let path = place.state.join(JOURNAL);
    let earlier = Image::read(&path).map_err(|_| "Could not read Milo's state folder. Nothing was changed.")?;
    let mut changes: Vec<Change> = Change::new(path, earlier, Some(journal(&plan.changes))).into_iter().collect();
    changes.extend(plan.changes);
    files::install(&changes, || true)
        .map_err(|why| if why.starts_with("A settings file changed") { STALE.to_owned() } else { why.to_owned() })?;
    Ok(count)
}

/// Whether an import is kept that `undo` could take back.
pub(crate) fn undo_available(state: &Path) -> bool {
    state.join(JOURNAL).is_file()
}

/// Puts back what the last import replaced, all or none.
pub(crate) fn undo(place: &Place<'_>) -> Result<usize, String> {
    let _lock = lock::setup(place.state)?;
    let path = place.state.join(JOURNAL);
    let kept = Image::read(&path).map_err(|_| NOTHING_TO_UNDO)?.ok_or(NOTHING_TO_UNDO)?;
    let mut changes = std::str::from_utf8(&kept.bytes)
        .ok()
        .and_then(|text| reversal(place, text))
        .ok_or("Milo's record of the last import could not be read. Nothing was changed.")?;
    let count = changes.len();
    changes.extend(Change::between(path, Some(kept), None));
    files::install(&changes, || true).map_err(|why| {
        if why.starts_with("A settings file changed") { CHANGED_SINCE_IMPORT.to_owned() } else { why.to_owned() }
    })?;
    Ok(count)
}

/// What `milo settings` was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Request<'a> {
    Export { groups: Vec<Group>, scopes: Scopes, output: Option<&'a OsStr> },
    Import { file: &'a OsStr, maps: Vec<(String, PathBuf)>, apply: bool },
    Undo,
}

/// The groups in a comma-separated choice, each once, in the order bundles are written in.
pub(crate) fn groups(choice: &str) -> Option<Vec<Group>> {
    if choice == "all" {
        return Some(Group::ALL.to_vec());
    }
    let named: Vec<Group> = choice.split(',').map(|name| Group::parse(name.trim())).collect::<Option<_>>()?;
    Some(Group::ALL.into_iter().filter(|group| named.contains(group)).collect())
}

impl<'a> Request<'a> {
    pub(crate) fn parse(action: Option<&'a OsStr>, rest: &'a [OsString]) -> Result<Self, String> {
        const EXPECTED: &str = "expected milo settings export GROUPS | import FILE | undo; see milo settings --help";
        let mut words = rest.iter().map(OsString::as_os_str);
        match action.and_then(OsStr::to_str) {
            Some("export") => {
                let groups = words.next().and_then(OsStr::to_str).and_then(groups).filter(|groups| !groups.is_empty());
                let groups = groups
                    .ok_or("choose what to export: appearance, workflow, usage, layout (comma-separated), or all")?;
                let (mut scopes, mut output) = (None, None);
                while let Some(flag) = words.next() {
                    match flag.to_str() {
                        Some("--scope") if scopes.is_none() => {
                            let scope = words.next().and_then(OsStr::to_str).and_then(Scopes::parse);
                            scopes = Some(scope.ok_or("--scope takes global, worktree or all")?);
                        }
                        Some("--output") if output.is_none() => {
                            output = Some(
                                words.next().filter(|file| !file.is_empty()).ok_or("--output requires a file name")?,
                            );
                        }
                        _ => return Err(EXPECTED.into()),
                    }
                }
                Ok(Self::Export { groups, scopes: scopes.unwrap_or(Scopes::Global), output })
            }
            Some("import") => {
                let file = words
                    .next()
                    .filter(|file| !file.is_empty())
                    .ok_or("import requires a file, or - for standard input")?;
                let (mut maps, mut apply) = (Vec::new(), false);
                while let Some(flag) = words.next() {
                    match flag.to_str() {
                        Some("--apply") if !apply => apply = true,
                        Some("--map") => {
                            let pair = words.next().map(OsStr::as_bytes);
                            let split = pair.and_then(|pair| {
                                pair.iter().position(|&byte| byte == b'=').map(|at| pair.split_at(at))
                            });
                            let (name, dir) = split.ok_or("--map takes NAME=DIR")?;
                            let name = std::str::from_utf8(name)
                                .ok()
                                .filter(|name| !name.is_empty())
                                .ok_or("--map takes NAME=DIR")?;
                            maps.push((name.to_owned(), PathBuf::from(OsStr::from_bytes(&dir[1..]))));
                        }
                        _ => return Err(EXPECTED.into()),
                    }
                }
                Ok(Self::Import { file, maps, apply })
            }
            Some("undo") if rest.is_empty() => Ok(Self::Undo),
            _ => Err(EXPECTED.into()),
        }
    }
}

fn read_bundle(file: &OsStr) -> Result<String, String> {
    let mut text = String::new();
    let limit = BUNDLE_LIMIT as u64 + 1;
    let read = if file == "-" {
        std::io::stdin().lock().take(limit).read_to_string(&mut text)
    } else {
        crate::fs_ops::open_read(Path::new(file)).and_then(|opened| opened.take(limit).read_to_string(&mut text))
    };
    match read {
        Ok(_) if text.len() <= BUNDLE_LIMIT => Ok(text),
        Ok(_) => Err(NOT_A_BUNDLE.into()),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Err(NOT_A_BUNDLE.into()),
        Err(error) => Err(format!("Could not read the bundle: {error}")),
    }
}

/// Runs `milo settings` against this host's own settings folders.
pub(crate) fn command(request: Request<'_>) -> Result<String, String> {
    let store = Store::real().ok_or("Could not locate your settings folders. Check HOME and XDG_CONFIG_HOME.")?;
    let home = paths::home();
    let cwd = Path::new(".");
    let root;
    let worktree = match request {
        Request::Export { scopes: Scopes::Worktree | Scopes::All, .. } => {
            root = git::worktree(cwd);
            root.as_deref()
        }
        // Project matching resolves the directory itself, and only when the bundle needs it.
        Request::Import { .. } => Some(cwd),
        Request::Export { scopes: Scopes::Global, .. } | Request::Undo => None,
    };
    let place = Place { config: &store.config, state: &store.state, home: home.as_deref(), worktree };
    match request {
        Request::Export { groups, scopes, output } => {
            let bundle = export(&place, &groups, scopes)?;
            let Some(output) = output else { return Ok(bundle.trim_end().to_owned()) };
            // The bundle is the person's own choices; another account has no use for it.
            let mut file =
                OpenOptions::new().write(true).create_new(true).mode(0o600).open(output).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        "That file exists already. Choose a new name; an export never replaces a file.".to_owned()
                    } else {
                        format!("Could not write the bundle: {error}")
                    }
                })?;
            file.write_all(bundle.as_bytes()).map_err(|error| format!("Could not write the bundle: {error}"))?;
            Ok(format!(
                "Exported {} to {}. On the other host, preview it with: milo settings import FILE",
                groups.iter().map(|group| group.name()).collect::<Vec<_>>().join(", "),
                Path::new(output).display()
            ))
        }
        Request::Import { file, maps, apply: make } => {
            let plan = plan(&place, &read_bundle(file)?, &maps)?;
            let name = if file == "-" { "FILE".into() } else { file.to_string_lossy() };
            let mut report = plan.report(&name);
            if plan.changes() == 0 {
                report.push_str("Nothing to change: this host already has these settings.");
            } else if make {
                let count = apply(&place, plan)?;
                let settings = if count == 1 { "1 setting" } else { &format!("{count} settings") };
                write!(
                    report,
                    "Changed {settings}. Reopen Milo panes to see them. To take this back: milo settings undo"
                )
                .unwrap();
            } else {
                write!(report, "Nothing was changed. To make these changes: milo settings import {name} --apply")
                    .unwrap();
            }
            Ok(report)
        }
        Request::Undo => {
            let count = undo(&place)?;
            let files = if count == 1 { "1 file" } else { &format!("{count} files") };
            Ok(format!("Took back the last import: {files} restored. Reopen Milo panes to see them."))
        }
    }
}

#[cfg(test)]
mod tests;
