//! Keeps unsaved edits when a reader is closed from outside, and lets other
//! panes see that a floating reader holds some.
//!
//! Both live in milo's state folder, never in the project. A kept draft is
//! named after the file's path and offered one at a time when that file is edited.

use std::fs::{self, DirBuilder, File};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use super::content::LIMIT;
use crate::fs_ops::{self, Temp};
use crate::paths;

/// What to tell the user once the terminal is theirs again: a line for each draft.
static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The lines about drafts kept by a reader that has just closed, once.
pub fn take_notes() -> Vec<String> {
    NOTES.lock().map(|mut notes| std::mem::take(&mut *notes)).unwrap_or_default()
}

/// Leaves a line for the user to read once the terminal is theirs again.
pub(super) fn tell(note: String) {
    if let Ok(mut notes) = NOTES.lock() {
        notes.push(note);
    }
}

/// Where drafts and marks are kept. Tests name their own folder.
pub(super) fn state_dir() -> Option<PathBuf> {
    if cfg!(test) { None } else { paths::state_dir() }
}

/// A private folder under the state folder, made if it is missing.
pub(super) fn private_dir(state: &Path, name: &str) -> io::Result<PathBuf> {
    let dir = state.join(name);
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    // Refuse a link left where the folder should be.
    fs_ops::open_dir(&dir)?;
    Ok(dir)
}

/// A name no other path shares in practice: the 64-bit FNV-1a of the path.
pub(super) fn hashed(text: &[u8]) -> String {
    let hash = text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    format!("{hash:016x}")
}

/// Where an interrupted edit of one file is kept.
pub(super) struct Drafts {
    state: PathBuf,
    /// The file's full path, which names the draft and is shown to the user.
    file: PathBuf,
}

impl Drafts {
    pub(super) fn new(state: PathBuf, file: PathBuf) -> Self {
        Self { state, file }
    }

    fn dir(&self) -> PathBuf {
        self.state.join("drafts")
    }

    fn name(&self) -> String {
        hashed(self.file.as_os_str().as_encoded_bytes())
    }

    pub(super) fn first(&self) -> Option<Draft> {
        let dir = self.dir();
        let name = self.name();
        let first = dir.join(format!("{name}.draft"));
        if fs::symlink_metadata(&first).is_ok_and(|metadata| metadata.is_file()) {
            return Some(Draft { path: first, base: None });
        }
        fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let filename = entry.file_name();
                let tail = filename.to_str()?.strip_prefix(&name)?.strip_prefix('-')?.strip_suffix(".draft")?;
                // A draft kept before bases were recorded has only its slot.
                let (slot, base) = tail.split_once('-').map_or((tail, None), |(slot, base)| (slot, Some(base)));
                let slot = slot.parse::<u128>().ok()?;
                let draft = Draft { path: entry.path(), base: base.map(str::to_owned) };
                entry.file_type().ok()?.is_file().then_some((slot, draft))
            })
            .min_by_key(|(slot, _)| *slot)
            .map(|(_, draft)| draft)
    }

    /// Updates the restored copy, or adds an independent draft without replacing other copies.
    /// `base` is the [`base`] of the file text the draft was written over, where that is known.
    /// Returns what to tell the user about it.
    pub(super) fn keep(&self, bytes: &[u8], base: Option<&str>, restored: Option<&Draft>) -> String {
        let write = || {
            let dir = private_dir(&self.state, "drafts")?;
            let name = self.name();
            let staged = dir.join(format!(".draft-{}", std::process::id()));
            // A process of the same number may have died while writing one.
            let _ = fs::remove_file(&staged);
            let (temp, mut file) = Temp::create(staged)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            // Publish under a new name before giving up the restored copy. Other readers can
            // have restored it too, and their independently kept copies must not overwrite ours.
            let mut slot = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
            loop {
                let filename = match base {
                    Some(base) => format!("{name}-{slot:039}-{base}.draft"),
                    None => format!("{name}-{slot:039}.draft"),
                };
                match fs::hard_link(temp.path(), dir.join(filename)) {
                    Ok(()) => {
                        if let Some(restored) = restored {
                            restored.forget();
                        }
                        return Ok(());
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => slot += 1,
                    Err(error) => return Err(error),
                }
            }
        };
        match write() {
            Ok(()) => format!(
                "unsaved edits to {} were kept. Press e on the file in milo to get them back.",
                self.file.display()
            ),
            Err(error) => format!("unsaved edits to {} could not be kept: {error}", self.file.display()),
        }
    }
}

/// Names one version of a file's text, to tell later whether the file still holds it.
pub(super) fn base(text: &str) -> String {
    format!("{}-{}", text.len(), hashed(text.as_bytes()))
}

pub(super) struct Draft {
    path: PathBuf,
    /// The [`base`] of the file text this draft was written over, unless it was kept without one.
    base: Option<String>,
}

impl Draft {
    /// Whether this draft is known to have been written over `text`.
    pub(super) fn written_over(&self, text: &str) -> bool {
        self.base.as_deref() == Some(&base(text))
    }

    pub(super) fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }

    /// The kept text, if it is still text milo can edit.
    pub(super) fn read(&self) -> Option<String> {
        let file = fs_ops::open_read(&self.path).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
        let text = String::from_utf8(bytes).ok()?;
        (text.len() as u64 <= LIMIT && !text.contains('\0')).then_some(text)
    }

    pub(super) fn forget(&self) {
        let _ = fs::remove_file(&self.path);
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
        let drafts = Drafts::new(state.clone(), f.root.join("a.txt"));
        assert!(drafts.first().is_none());
        // Recognize the original single-draft file name as well.
        private_dir(&state, "drafts").unwrap();
        let (temp, mut file) = Temp::create(drafts.dir().join(".legacy")).unwrap();
        file.write_all(b"kept \xc3\xa9\r\n").unwrap();
        temp.replace(&drafts.dir().join(format!("{}.draft", drafts.name()))).unwrap();
        let note = drafts.keep(b"another copy", Some(&base("its file")), None);
        assert!(note.contains("a.txt") && note.contains("were kept"), "{note}");
        let draft = drafts.first().unwrap();
        assert_eq!(draft.read().as_deref(), Some("kept é\r\n"));
        // A draft from before bases were recorded is known to be written over nothing.
        assert!(!draft.written_over("its file"));
        assert_eq!(fs::metadata(&draft.path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(state.join("drafts")).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(Drafts::new(state, f.root.join("b.txt")).first().is_none());
        // Saving a successor to the restored copy removes only its predecessor.
        assert!(drafts.keep(b"newer", None, Some(&draft)).contains("were kept"));
        assert!(draft.read().is_none());
        let other = drafts.first().unwrap();
        assert_eq!(other.read().as_deref(), Some("another copy"));
        assert!(other.written_over("its file") && !other.written_over("its file, changed"));
        other.forget();
        let newer = drafts.first().unwrap();
        assert_eq!(newer.read().as_deref(), Some("newer"));
        newer.forget();
        assert!(drafts.first().is_none());
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
