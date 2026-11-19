//! Plans status-line edits only when milo can prove it owns the old entry.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use super::files::{Change, Image};
use super::install::{Store, read, settings_path};
use super::settings::{self, Settings, Status};

pub(super) const OWN_CLAUDE: &str = "claude-status-added";
const CLAUDE_CREATED: &[u8] = b"created\n";
const CLAUDE_UPDATED: &[u8] = b"updated\n";

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

    fn owned(&self) -> bool {
        self.note.as_ref().is_some_and(|file| matches!(file.bytes.as_slice(), CLAUDE_CREATED | CLAUDE_UPDATED))
    }

    /// A matching command without a note may have been added by the user.
    fn status(&self) -> Claude {
        self.status_of(self.editor().as_ref())
    }

    fn status_of(&self, editor: Option<&Settings<'_>>) -> Claude {
        match editor.map(Settings::status) {
            Some(Status::Absent) => Claude::Off,
            Some(Status::Foreign) => Claude::Foreign,
            Some(Status::Milo) if self.owned() => Claude::On,
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
        if self.note.is_some() && !self.owned() {
            return Err("Milo's Claude Code note was changed. It was left alone.");
        }
        let json = json.expect("editable settings are valid");
        match (status, enabled) {
            (Claude::Off, true) => {
                let created = self.settings.is_none();
                let text = if created { settings::NEW.into() } else { json.add().expect("the status line is absent") };
                let recorded = if created { CLAUDE_CREATED } else { CLAUDE_UPDATED };
                // If milo is interrupted, any status line it wrote is already owned.
                changes.extend(Change::new(self.note_path, self.note, Some(recorded.to_vec())));
                changes.extend(Change::new(self.path.into_owned(), self.settings, Some(text.into_bytes())));
            }
            (Claude::On, false) => {
                let created = self.note.as_ref().is_some_and(|note| note.bytes == CLAUDE_CREATED);
                let untouched = self.settings.as_ref().is_some_and(|file| file.bytes == settings::NEW.as_bytes());
                let delete = created && untouched && matches!(self.path, Cow::Borrowed(_));
                let after = (!delete).then(|| json.remove().expect("the status line is milo's").into_bytes());
                changes.extend(Change::new(self.path.into_owned(), self.settings, after));
                changes.extend(Change::new(self.note_path, self.note, None));
            }
            (Claude::Off, false) => changes.extend(Change::new(self.note_path, self.note, None)),
            _ => {}
        }
        Ok(())
    }
}
