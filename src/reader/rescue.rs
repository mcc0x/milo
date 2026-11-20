//! Keeps unsaved edits when a reader is closed from outside, and lets other
//! panes see that a floating reader holds some.
//!
//! Both live in milo's state folder, never in the project. A kept draft is
//! named after the file's path and offered the next time that file is edited.

use std::fs::{self, DirBuilder, File};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::content::LIMIT;
use crate::fs_ops::{self, Temp};
use crate::paths;

/// What to tell the user once the terminal is theirs again.
static NOTE: Mutex<Option<String>> = Mutex::new(None);

/// The line about a draft kept by a reader that has just closed, once.
pub fn take_note() -> Option<String> {
    NOTE.lock().ok()?.take()
}

/// Leaves a line for the user to read once the terminal is theirs again.
pub(super) fn tell(note: String) {
    if let Ok(mut slot) = NOTE.lock() {
        *slot = Some(note);
    }
}

/// Where drafts and marks are kept. Tests name their own folder.
pub(super) fn state_dir() -> Option<PathBuf> {
    if cfg!(test) { None } else { paths::state_dir() }
}

/// A private folder under the state folder, made if it is missing.
fn private_dir(state: &Path, name: &str) -> io::Result<PathBuf> {
    let dir = state.join(name);
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    // Refuse a link left where the folder should be.
    fs_ops::open_dir(&dir)?;
    Ok(dir)
}

/// A name no other path shares in practice: the 64-bit FNV-1a of the path.
fn hashed(text: &[u8]) -> String {
    let hash = text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    format!("{hash:016x}")
}

/// Where an interrupted edit of one file is kept.
pub(super) struct Draft {
    state: PathBuf,
    /// The file's full path, which names the draft and is shown to the user.
    file: PathBuf,
}

impl Draft {
    pub(super) fn new(state: PathBuf, file: PathBuf) -> Self {
        Self { state, file }
    }

    fn path(&self) -> PathBuf {
        let name = hashed(self.file.as_os_str().as_encoded_bytes());
        self.state.join("drafts").join(format!("{name}.draft"))
    }

    /// Writes the unsaved text beside any other drafts, replacing an older one
    /// for this file. Returns what to tell the user about it.
    pub(super) fn keep(&self, bytes: &[u8]) -> String {
        let write = || {
            let dir = private_dir(&self.state, "drafts")?;
            let path = self.path();
            let (temp, mut file) = Temp::create(dir.join(format!(".draft-{}", std::process::id())))?;
            file.write_all(bytes)?;
            file.sync_all()?;
            temp.replace(&path)
        };
        match write() {
            Ok(()) => format!(
                "unsaved edits to {} were kept. Press e on the file in milo to get them back.",
                self.file.display()
            ),
            Err(error) => format!("unsaved edits to {} could not be kept: {error}", self.file.display()),
        }
    }

    pub(super) fn exists(&self) -> bool {
        fs::symlink_metadata(self.path()).is_ok_and(|metadata| metadata.is_file())
    }

    /// The kept text, if it is still text milo can edit.
    pub(super) fn read(&self) -> Option<String> {
        let file = fs_ops::open_read(&self.path()).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
        let text = String::from_utf8(bytes).ok()?;
        (text.len() as u64 <= LIMIT && !text.contains('\0')).then_some(text)
    }

    pub(super) fn forget(&self) {
        let _ = fs::remove_file(self.path());
    }
}

fn mark_path(state: &Path, block: &str) -> PathBuf {
    state.join("editing").join(hashed(block.as_bytes()))
}

/// Says, while it lives, that the reader in a Rex block holds unsaved edits.
pub(super) struct Mark(PathBuf);

impl Mark {
    pub(super) fn set(state: &Path, block: &str) -> Option<Self> {
        private_dir(state, "editing").ok()?;
        let path = mark_path(state, block);
        fs::write(&path, std::process::id().to_string()).ok()?;
        Some(Self(path))
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Whether a running reader in `block` holds unsaved edits. A mark left by a
/// reader that has since died is removed.
pub(crate) fn block_has_draft(state: &Path, block: &str) -> bool {
    let path = mark_path(state, block);
    let Some(pid) = read_pid(&path) else { return false };
    // SAFETY: Signal 0 only asks whether the process exists; nothing is sent.
    let alive = unsafe { libc::kill(pid, 0) } == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !alive {
        let _ = fs::remove_file(path);
    }
    alive
}

fn read_pid(path: &Path) -> Option<libc::pid_t> {
    let mut text = String::new();
    File::open(path).ok()?.take(16).read_to_string(&mut text).ok()?;
    text.trim().parse().ok().filter(|&pid| pid > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn a_kept_draft_is_private_and_found_only_for_its_own_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let f = Fixture::folder();
        let state = f.root.join("state");
        let draft = Draft::new(state.clone(), f.root.join("a.txt"));
        assert!(!draft.exists() && draft.read().is_none());
        let note = draft.keep(b"kept \xc3\xa9\r\n");
        assert!(note.contains("a.txt") && note.contains("were kept"), "{note}");
        assert_eq!(draft.read().as_deref(), Some("kept é\r\n"));
        assert_eq!(fs::metadata(draft.path()).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(state.join("drafts")).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(!Draft::new(state.clone(), f.root.join("b.txt")).exists());
        // A later draft of the same file takes the older one's place.
        assert!(draft.keep(b"newer").contains("were kept"));
        assert_eq!(draft.read().as_deref(), Some("newer"));
        draft.forget();
        assert!(!draft.exists());
    }

    #[test]
    fn a_mark_counts_only_while_its_reader_is_alive() {
        let f = Fixture::folder();
        let block = "block:11111111-1111-1111-1111-111111111111";
        assert!(!block_has_draft(&f.root, block));
        let mark = Mark::set(&f.root, block).unwrap();
        assert!(block_has_draft(&f.root, block));
        assert!(!block_has_draft(&f.root, "block:22222222-2222-2222-2222-222222222222"));
        drop(mark);
        assert!(!block_has_draft(&f.root, block));

        // A reader that was killed leaves its mark behind; no process has this ID.
        let stale = mark_path(&f.root, block);
        fs::write(&stale, i32::MAX.to_string()).unwrap();
        assert!(!block_has_draft(&f.root, block));
        assert!(!stale.exists());
    }
}
