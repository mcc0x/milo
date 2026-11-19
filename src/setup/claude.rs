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

fn status_command(path: &Path) -> Result<String, &'static str> {
    let path = path.to_str().ok_or("Milo's path is not valid UTF-8 for Claude Code settings.")?;
    Ok(format!("'{}' claude-status", path.replace('\'', "'\\''")))
}

fn current_command() -> Result<String, &'static str> {
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
    /// Show foreign settings as they are without offering to replace them.
    pub fn claude(&self) -> Claude {
        self.claude_files().map_or(Claude::Invalid, |settings| settings.status())
    }

    /// Read the ownership note alongside the settings before proposing edits.
    pub(super) fn claude_files(&self) -> Result<ClaudeFiles<'_>, &'static str> {
        let path = settings_path(&self.claude)?;
        let note_path = self.state.join(OWN_CLAUDE);
        Ok(ClaudeFiles { settings: read(&path)?, path, note: read(&note_path)?, note_path })
    }
}

/// The two files must agree before setup is allowed to remove a status line.
pub(super) struct ClaudeFiles<'a> {
    path: Cow<'a, Path>,
    settings: Option<Image>,
    note: Option<Image>,
    note_path: PathBuf,
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
        self.status_of(self.editor().as_ref())
    }

    fn status_of(&self, editor: Option<&Settings<'_>>) -> Claude {
        let ownership = self.ownership();
        match editor.map(|editor| editor.status(ownership.map(|(_, command)| command))) {
            Some(Status::Absent) => Claude::Off,
            Some(Status::Foreign) => Claude::Foreign,
            Some(Status::Milo) if ownership.is_some() => Claude::On,
            Some(Status::Milo) => Claude::NotOwned,
            None => Claude::Invalid,
        }
    }

    /// Recheck ownership against the files just read rather than the pane's old state.
    pub(super) fn plan(self, enabled: bool, changes: &mut Vec<Change>) -> Result<(), &'static str> {
        let json = self.editor();
        let status = self.status_of(json.as_ref());
        if !status.editable() {
            return Err("Claude Code settings changed. Reopen setup to see them.");
        }
        if self.note.is_some() && self.ownership().is_none() {
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
                let (origin, command) = self.ownership().expect("the status line is owned");
                let delete = origin == CLAUDE_CREATED
                    && matches!(self.path, Cow::Borrowed(_))
                    && self.settings.as_ref().is_some_and(|file| file.bytes == settings::new(command).as_bytes());
                let after = (!delete).then(|| json.remove(command).expect("the status line is milo's").into_bytes());
                changes.extend(Change::new(self.path.into_owned(), self.settings, after));
                changes.extend(Change::new(self.note_path, self.note, None));
            }
            (Claude::Off, false) => changes.extend(Change::new(self.note_path, self.note, None)),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
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
    fn a_changed_launcher_does_not_match_the_ownership_note() {
        let text = settings::new("'/a/milo' claude-status");
        let settings = Some(Image { bytes: text.into_bytes(), mode: 0o600 });
        let note = Some(Image { bytes: b"created\n'/other/milo' claude-status".to_vec(), mode: 0o600 });
        let files = ClaudeFiles {
            path: Cow::Borrowed(Path::new("settings.json")),
            settings,
            note,
            note_path: PathBuf::new(),
        };
        assert!(files.status() == Claude::Foreign);
        assert!(files.plan(false, &mut Vec::new()).is_err());
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
