//! Plans status-line edits only when milo can prove it owns the old entry.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use super::files::{Change, Image};
use super::install::{Store, read, settings_path};
use super::rex_files::executable;
use super::settings::{self, Settings, Status};

pub(super) const OWN_CLAUDE: &str = "claude-status-added";
const CLAUDE_CREATED: &str = "created";
const CLAUDE_UPDATED: &str = "updated";
pub(super) const WINDOWS: &str = "Milo's Claude Code status line is not supported on Windows yet.";

fn status_command(path: &Path) -> Result<String, &'static str> {
    let path = path.to_str().ok_or("Milo's path is not valid UTF-8 for Claude Code settings.")?;
    Ok(format!("'{}' claude-status", path.replace('\'', "'\\''")))
}

fn current_command() -> Result<String, &'static str> {
    if cfg!(windows) {
        return Err(WINDOWS);
    }
    status_command(&executable()?)
}

/// A valid Claude entry is editable only when it is absent or owned by setup.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Claude {
    Off,
    On,
    Foreign,
    NotOwned,
    Invalid,
}

impl Claude {
    pub fn editable(self) -> bool {
        matches!(self, Self::Off | Self::On)
    }
}

impl Store {
    pub(super) fn claude_state_dir(&self) -> &Path {
        self.claude_state.as_deref().unwrap_or(&self.state)
    }

    pub(super) fn claude_note(&self) -> PathBuf {
        self.claude_state_dir().join(OWN_CLAUDE)
    }

    /// Show foreign settings as they are without offering to replace them.
    pub fn claude(&self) -> Claude {
        self.claude_files().map_or(Claude::Invalid, |settings| settings.status())
    }

    /// Read the ownership note alongside the settings before proposing edits.
    pub(super) fn claude_files(&self) -> Result<ClaudeFiles<'_>, &'static str> {
        let path = settings_path(&self.claude)?;
        let note_path = self.claude_note();
        Ok(ClaudeFiles { settings: read(&path)?, path, note: read(&note_path)?, note_path })
    }

    pub(super) fn claude_snapshot(&self) -> Result<(Claude, ClaudeSnapshot), &'static str> {
        let files = self.claude_files()?;
        let status = files.status();
        Ok((status, ClaudeSnapshot { path: files.path.into_owned(), settings: files.settings, note: files.note }))
    }

    pub(super) fn save_claude_draft(&self, enabled: bool, before: &ClaudeSnapshot) -> Result<(), &'static str> {
        self.save_with(None, None, &|_| false, |changes| {
            let files = self.claude_files()?;
            if files.path.as_ref() != before.path.as_path()
                || files.settings != before.settings
                || files.note != before.note
            {
                return Err("Claude Code settings changed. Reopen Settings before saving again.");
            }
            files.plan(enabled, changes)
        })?;
        Ok(())
    }
}

/// The two files must agree before setup is allowed to remove a status line.
pub(super) struct ClaudeFiles<'a> {
    path: Cow<'a, Path>,
    settings: Option<Image>,
    note: Option<Image>,
    note_path: PathBuf,
}

pub(super) struct ClaudeSnapshot {
    path: PathBuf,
    settings: Option<Image>,
    note: Option<Image>,
}

impl ClaudeFiles<'_> {
    /// Treat missing settings as empty while rejecting malformed existing files.
    fn editor(&self) -> Option<Settings<'_>> {
        let text = self.settings.as_ref().map_or(Some("{\n}\n"), |file| std::str::from_utf8(&file.bytes).ok())?;
        Settings::parse(text)
    }

    /// Older notes recorded just the file's origin and belonged to the bare command.
    fn ownership(&self) -> Option<(&str, &str)> {
        let (origin, command) = std::str::from_utf8(&self.note.as_ref()?.bytes).ok()?.split_once('\n')?;
        if !matches!(origin, CLAUDE_CREATED | CLAUDE_UPDATED) {
            return None;
        }
        if command.is_empty() {
            Some((origin, settings::LEGACY_COMMAND))
        } else {
            settings::is_launcher(command).then_some((origin, command))
        }
    }

    pub(super) fn launcher_matches(&self) -> bool {
        self.ownership().zip(current_command().ok()).is_some_and(|((_, owned), current)| owned == current)
    }

    /// A matching command without a note may have been added by the user.
    pub(super) fn status(&self) -> Claude {
        self.status_with_ownership().0
    }

    /// The note can still be valid after its status line is gone or replaced.
    pub(super) fn status_with_ownership(&self) -> (Claude, bool) {
        let command = self.ownership().map(|(_, command)| command);
        (Self::status_of(self.editor().as_ref(), command), command.is_some())
    }

    fn status_of(editor: Option<&Settings<'_>>, command: Option<&str>) -> Claude {
        match editor.map(|editor| editor.status(command)) {
            Some(Status::Absent) => Claude::Off,
            Some(Status::Foreign) => Claude::Foreign,
            Some(Status::Milo) if command.is_some() => Claude::On,
            Some(Status::Milo) => Claude::NotOwned,
            None => Claude::Invalid,
        }
    }

    /// Recheck ownership against the files just read rather than the pane's old state.
    pub(super) fn plan(self, enabled: bool, changes: &mut Vec<Change>) -> Result<(), &'static str> {
        let json = self.editor();
        let ownership = self.ownership();
        let status = Self::status_of(json.as_ref(), ownership.map(|(_, command)| command));
        let clear_stale_note = !enabled && status == Claude::Foreign && ownership.is_some();
        if !status.editable() && !clear_stale_note {
            return Err("Claude Code settings changed. Reopen setup to see them.");
        }
        if self.note.is_some() && ownership.is_none() {
            return Err("Milo's Claude Code note was changed. It was left alone.");
        }
        let json = json.expect("editable settings are valid");
        match (status, enabled) {
            (Claude::Off, true) => {
                let command = current_command()?;
                let created = self.settings.is_none();
                let text = if created {
                    settings::new(&command)
                } else {
                    json.add(&command).expect("the status line is absent")
                };
                let origin = if created { CLAUDE_CREATED } else { CLAUDE_UPDATED };
                let recorded = format!("{origin}\n{command}");
                // If milo is interrupted, any status line it wrote is already owned.
                changes.extend(Change::new(self.note_path, self.note, Some(recorded.into_bytes())));
                changes.extend(Change::new(self.path.into_owned(), self.settings, Some(text.into_bytes())));
            }
            (Claude::On, false) => {
                let (origin, command) = ownership.expect("the status line is owned");
                let delete = origin == CLAUDE_CREATED
                    && matches!(self.path, Cow::Borrowed(_))
                    && self.settings.as_ref().is_some_and(|file| file.bytes == settings::new(command).as_bytes());
                let after = (!delete).then(|| json.remove(command).expect("the status line is milo's").into_bytes());
                changes.extend(Change::new(self.path.into_owned(), self.settings, after));
                changes.extend(Change::new(self.note_path, self.note, None));
            }
            // A missing or replaced status line no longer needs its ownership note.
            (Claude::Off | Claude::Foreign, false) => changes.extend(Change::new(self.note_path, self.note, None)),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    #[test]
    fn claude_launcher_runs_without_milo_on_path_and_quotes_its_name() {
        let fixture = crate::fixture::Fixture::folder();
        let path = fixture.root.join("milo $here'\"\\bin");
        fs::write(&path, b"#!/bin/sh\nprintf '%s' \"$1\"\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let command = status_command(&path).unwrap();
        assert!(settings::is_launcher(&command));
        let output = Command::new("/bin/sh").env("PATH", "/no-milo").args(["-c", &command]).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"claude-status");
    }

    #[test]
    fn a_replacement_launcher_is_left_alone_when_clearing_the_stale_note() {
        let text = settings::new("'/a/milo' claude-status");
        let settings = Some(Image { bytes: text.into_bytes(), mode: 0o600 });
        let note = Some(Image { bytes: b"created\n'/other/milo' claude-status".to_vec(), mode: 0o600 });
        let path = Cow::Borrowed(Path::new("settings.json"));
        let files = ClaudeFiles { path, settings, note, note_path: OWN_CLAUDE.into() };
        assert!(files.status_with_ownership() == (Claude::Foreign, true));
        let mut changes = Vec::new();
        files.plan(false, &mut changes).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path(), Path::new(OWN_CLAUDE));
    }

    #[test]
    fn legacy_bare_launcher_can_still_be_removed() {
        let fixture = crate::fixture::Fixture::folder();
        let path = fixture.root.join("settings.json");
        let note_path = fixture.root.join(OWN_CLAUDE);
        fs::write(&path, settings::new(settings::LEGACY_COMMAND)).unwrap();
        fs::write(&note_path, b"created\n").unwrap();
        let files = ClaudeFiles {
            path: Cow::Borrowed(&path),
            settings: read(&path).unwrap(),
            note: read(&note_path).unwrap(),
            note_path: note_path.clone(),
        };
        assert!(files.status() == Claude::On);
        assert!(!files.launcher_matches());
        let mut changes = Vec::new();
        files.plan(false, &mut changes).unwrap();
        super::super::files::install(&changes, || true).unwrap();
        assert!(!path.exists() && !note_path.exists());
    }
}
