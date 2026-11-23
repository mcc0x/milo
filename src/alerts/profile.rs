//! Keep alerts for distinct config directories in distinct state and secret stores.

use std::ffi::{CStr, OsStr};
use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use super::{SETTINGS_LIMIT, Settings, secret};
use crate::{paths, preferences};

pub(super) struct Profile {
    pub(super) config: PathBuf,
    pub(super) state: PathBuf,
    pub(super) secrets: secret::Store,
    pub(super) isolated: bool,
}

impl Profile {
    pub(super) fn real() -> Option<Self> {
        let keychain = secret::test_keychain();
        // A disposable Keychain's own directory acts as that test account's
        // home. Changing HOME while sharing the file must not claim its slot.
        let home = keychain.as_deref().and_then(Path::parent).map(Path::to_owned).or_else(account_home);
        Self::locate(&paths::config_dir()?, &paths::state_dir()?, home.as_deref(), keychain).ok()
    }

    fn locate(config: &Path, state: &Path, home: Option<&Path>, keychain: Option<PathBuf>) -> io::Result<Self> {
        let config = canonical_path(config)?;
        let root = canonical_path(state)?;
        let is_default = |path: &Path, relative: &str| {
            home.and_then(|home| canonical_path(&home.join(relative)).ok()).is_some_and(|default| default == path)
        };
        let legacy = is_default(&config, ".config/milo");
        let key = key_for(&config);
        let scoped = root.join("alert-profiles").join(&key);
        let use_root = if is_default(&root, ".local/state/milo") {
            legacy
        } else {
            owns_custom_state(&config, &root, &key, legacy)?
        };
        let state = if use_root { root } else { scoped.clone() };
        let storage = if legacy { &state } else { &scoped };
        let secrets = secret::Store::real(storage, keychain, (!legacy).then_some(key.as_str()));
        Ok(Self { config, state, secrets, isolated: !legacy })
    }
}

// A pre-upgrade installation with a separate XDG_STATE_HOME keeps its old
// worker lock. Reserve that root to the same config on later invocations.
// If a fresh config points at an old state root, it uses its own subdirectory.
fn owns_custom_state(config: &Path, root: &Path, key: &str, legacy: bool) -> io::Result<bool> {
    let file = root.join("alerts.profile");
    if let Some(owner) = preferences::read(&file, 64)? {
        return Ok(owner == key);
    }
    if !legacy {
        let has_old_state = ["alerts.lock", "alerts-supervisor.lock", "alerts.status", "alerts-secrets"]
            .into_iter()
            .any(|name| root.join(name).is_file());
        let had_pairing = has_old_state
            && preferences::read(&config.join("alerts.conf"), SETTINGS_LIMIT)?
                .and_then(|text| Settings::parse(&text).ok())
                .is_some_and(|settings| {
                    settings.connection_revision == [0; 2]
                        && (settings.destination.is_some() || settings.targets().contains(&true))
                });
        if !had_pairing {
            return Ok(false);
        }
    }
    preferences::locked(&file, |dir| match preferences::read(&file, 64)? {
        Some(owner) => Ok(owner == key),
        None => {
            preferences::replace(dir, &file, key)?;
            Ok(true)
        }
    })
}

// Canonicalize the existing prefix; a fresh profile need not have made its
// config or state directory yet. Reject ambiguous missing parent components.
fn canonical_path(path: &Path) -> io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let Some((parent, name)) = path.parent().zip(path.file_name()) else { return Err(error) };
            if parent.as_os_str().is_empty() || fs::symlink_metadata(path).is_ok() {
                return Err(error);
            }
            Ok(canonical_path(parent)?.join(name))
        }
        result => result,
    }
}

fn key_for(path: &Path) -> String {
    let bytes = path.as_os_str().as_bytes();
    let [first, second] = [0xcbf2_9ce4_8422_2325_u64, 0xb6e1_9938_391d_7567]
        .map(|seed| bytes.iter().fold(seed, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)));
    format!("{first:016x}{second:016x}")
}

/// Use the OS account record, not the caller's HOME override, to decide
/// which settings directory can read the old unscoped credential slots.
fn account_home() -> Option<PathBuf> {
    let mut entry = MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buf = [0; 16 * 1024];
    // SAFETY: Reading the process's own UID has no preconditions.
    let uid = unsafe { libc::getuid() };
    // SAFETY: The buffer and passwd/result slots are writable and stay live
    // through the call; getpwuid_r puts its returned strings into the buffer.
    let status = unsafe { libc::getpwuid_r(uid, entry.as_mut_ptr(), buf.as_mut_ptr(), buf.len(), &mut result) };
    if status != 0 || result.is_null() {
        return None;
    }
    // SAFETY: A successful lookup initializes the record.
    let entry = unsafe { entry.assume_init() };
    if entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: The returned home string is NUL-terminated in the live buffer.
    let directory = unsafe { CStr::from_ptr(entry.pw_dir) };
    let path = PathBuf::from(OsStr::from_bytes(directory.to_bytes()));
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn an_existing_custom_state_keeps_its_worker_lock_and_cannot_be_claimed_twice() {
        let fixture = Fixture::folder();
        let root = fixture.root.canonicalize().unwrap();
        let home = root.join("home");
        let config = root.join("existing/milo");
        let state = root.join("custom-state/milo");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(config.join("alerts.conf"), "enabled = true\ndestination = discord\n").unwrap();
        fs::write(state.join("alerts.lock"), "").unwrap();

        let opened = Profile::locate(&config, &state, Some(&home), None).unwrap();
        assert!(opened.isolated);
        assert_eq!(opened.state, state);
        fs::write(config.join("alerts.conf"), "enabled = false\ndiscord_connection_revision = 5\n").unwrap();
        assert_eq!(Profile::locate(&config, &state, Some(&home), None).unwrap().state, state);

        let other = root.join("other/milo");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("alerts.conf"), "enabled = true\ndestination = discord\n").unwrap();
        assert_ne!(Profile::locate(&other, &state, Some(&home), None).unwrap().state, state);
    }
}
