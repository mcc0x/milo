//! Delivers opt-in lifecycle notices separately from the Rex alert worker.
//!
//! A provider has at most one pending notice. Changes replace or discard it
//! under the settings lock; dispatch claims it before attempting delivery.
//! Curl itself owns the cancellation lock, so disconnecting a destination
//! stops its request even if the detached launch helper was killed.

use std::ffi::CStr;
use std::fmt;
use std::fs;
use std::hash::{BuildHasher as _, DefaultHasher, Hasher as _, RandomState};
use std::io::{self, Read as _};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{Alerts, AlertsEdit, Destination, Settings, send};
use crate::{fs_ops, preferences, setup::lock};

/// A per-opt-in, opaque comparison tag for the exact recipient credential.
/// This is deliberately not a secret-store format, so legacy pairings work
/// unchanged even when multiple settings directories share a Keychain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Binding {
    salt: u128,
    tag: [u64; 2],
}

impl Binding {
    pub(super) fn new(settings: &Settings, destination: Destination, secret: &str) -> io::Result<Self> {
        let mut salt = [0; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut salt)?;
        let salt = u128::from_le_bytes(salt);
        Ok(Self { salt, tag: Self::tag(salt, settings, destination, secret) })
    }

    pub(super) fn parse(encoded: &str) -> Option<Self> {
        let hex = encoded.strip_prefix("v1:")?;
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self {
            salt: u128::from_str_radix(&hex[..32], 16).ok()?,
            tag: [u64::from_str_radix(&hex[32..48], 16).ok()?, u64::from_str_radix(&hex[48..], 16).ok()?],
        })
    }

    pub(super) fn matches(self, settings: &Settings, destination: Destination, secret: &str) -> bool {
        self.tag == Self::tag(self.salt, settings, destination, secret)
    }

    // DefaultHasher's algorithm is not a Rust compatibility promise. If a
    // toolchain changes it, old tags fail closed until notices are reapproved.
    // Two domains give a 128-bit comparison; the salt avoids correlating
    // tags across separate approvals of the same destination.
    fn tag(salt: u128, settings: &Settings, destination: Destination, secret: &str) -> [u64; 2] {
        let chat = if destination == Destination::Telegram { settings.telegram_chat.as_str() } else { "" };
        [0, 1].map(|domain| {
            let mut hash = DefaultHasher::new();
            hash.write(b"milo lifecycle consent v1\0");
            hash.write_u8(domain);
            hash.write(&salt.to_le_bytes());
            hash.write(destination.name().as_bytes());
            hash.write_u8(0);
            hash.write(chat.as_bytes());
            hash.write_u8(0);
            hash.write(secret.as_bytes());
            hash.finish()
        })
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [first, second] = self.tag;
        write!(out, "v1:{:032x}{first:016x}{second:016x}", self.salt)
    }
}

#[derive(Clone, Copy)]
pub(super) enum Notice {
    Enabled,
    Paused,
    Recovered,
}

impl Notice {
    pub(super) const ALL: [Self; 3] = [Self::Enabled, Self::Paused, Self::Recovered];

    fn name(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Paused => "paused",
            Self::Recovered => "recovered",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|notice| notice.name() == name)
    }

    pub(super) fn text(self) -> &'static str {
        match self {
            Self::Enabled => "Milo: Rex alert forwarding was enabled.",
            Self::Paused => "Milo: Rex alert forwarding was paused.",
            Self::Recovered => "Milo: Rex alert forwarding recovered after an interruption.",
        }
    }

    fn allowed(self, settings: &Settings, destination: Destination) -> bool {
        settings.notice_binding(destination).is_some() && settings.active(destination) != matches!(self, Self::Paused)
    }
}

fn pending(alerts: &Alerts, destination: Destination) -> PathBuf {
    alerts.state_dir().join(match destination {
        Destination::Telegram => "alerts-telegram.notice",
        Destination::Discord => "alerts-discord.notice",
    })
}

#[derive(Clone, Copy)]
struct Pending {
    id: u64,
    notice: Notice,
    binding: Binding,
}

fn read_pending(alerts: &Alerts, destination: Destination) -> Option<Pending> {
    let text = preferences::read(&pending(alerts, destination), 100).ok().flatten()?;
    let (id, details) = text.split_once(' ')?;
    let (name, binding) = details.split_once(' ')?;
    Some(Pending {
        id: u64::from_str_radix(id, 16).ok()?,
        notice: Notice::parse(name)?,
        binding: Binding::parse(binding)?,
    })
}

/// Recheck the exact current credential before a pending notice can use it.
fn authorize(
    edit: &AlertsEdit<'_>,
    destination: Destination,
    Pending { notice, binding, .. }: Pending,
) -> Result<Option<(Settings, String)>, String> {
    let alerts = edit.alerts;
    if known_refused(alerts, destination) {
        return Ok(None);
    }
    let settings = edit.editable()?;
    if !notice.allowed(&settings, destination) || settings.notice_binding(destination) != Some(binding) {
        return Ok(None);
    }
    let secret = alerts.target_secret(&settings, destination)?;
    Ok(binding.matches(&settings, destination, &secret).then_some((settings, secret)))
}

fn refusal(alerts: &Alerts, destination: Destination) -> PathBuf {
    alerts.state_dir().join(match destination {
        Destination::Telegram => "alerts-telegram.refused",
        Destination::Discord => "alerts-discord.refused",
    })
}

pub(super) fn known_refused(alerts: &Alerts, destination: Destination) -> bool {
    !matches!(fs::symlink_metadata(refusal(alerts, destination)), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

fn lock_name(destination: Destination) -> &'static CStr {
    match destination {
        Destination::Telegram => c"alerts-telegram-notice.lock",
        Destination::Discord => c"alerts-discord-notice.lock",
    }
}

fn clear(path: PathBuf) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Under the settings lock, invalidate queued work and stop a request in
/// flight before the destination's credential can be replaced or revoked.
pub(super) fn invalidate(edit: &AlertsEdit<'_>, destination: Destination) -> Result<(), String> {
    let removed = clear(pending(edit.alerts, destination));
    lock::stop_holder(edit.alerts.state_dir(), lock_name(destination))
        .and(removed)
        .map_err(|_| format!("Could not cancel the pending {} lifecycle notice.", destination.title()))
}

/// Only a new forwarding approval or an explicit re-save may retry a
/// destination whose binding was rejected.
pub(super) fn reapprove(edit: &AlertsEdit<'_>, destination: Destination) -> Result<(), String> {
    if known_refused(edit.alerts, destination) {
        invalidate(edit, destination)?;
        clear_refusal(edit.alerts, destination)?;
    }
    Ok(())
}

pub(super) fn clear_refusal(alerts: &Alerts, destination: Destination) -> Result<(), String> {
    clear(refusal(alerts, destination)).map_err(|_| format!("Could not clear {} delivery status.", destination.title()))
}

fn mark_refused(alerts: &Alerts, destination: Destination) {
    let state = alerts.state_dir();
    let _ =
        fs::create_dir_all(state).and_then(|()| preferences::replace(state, &refusal(alerts, destination), "refused"));
}

/// Called under the settings lock when a normal send confirms revoked or
/// otherwise rejected credentials. Cancel without waiting for network I/O.
pub(super) fn record_refusal(alerts: &Alerts, destination: Destination) {
    mark_refused(alerts, destination);
    let _ = clear(pending(alerts, destination));
    let _ = lock::signal_holder(alerts.state_dir(), lock_name(destination));
}

pub(super) fn provider_refused(alerts: &Alerts, before: &Settings, destination: Destination, secret: &str) {
    let _ = alerts.locked(|edit| {
        let settings = edit.editable()?;
        let index = destination as usize;
        if before.connection_revision[index] == settings.connection_revision[index]
            && (destination != Destination::Telegram || before.telegram_chat == settings.telegram_chat)
            && edit.saved_secret(destination).ok().flatten().as_deref() == Some(secret)
        {
            record_refusal(alerts, destination);
        }
        Ok(())
    });
}

/// Only a change in forwarding intent sends an enable or explicit pause notice.
pub(super) fn changed(edit: &AlertsEdit<'_>, settings: &Settings, was_active: [bool; 2], explicit_pause: bool) {
    for destination in Destination::ALL {
        let active = settings.active(destination);
        if settings.notice_binding(destination).is_none() || was_active[destination as usize] == active {
            continue;
        }
        let _ = lock::signal_holder(edit.alerts.state_dir(), lock_name(destination));
        let notice = if active { Notice::Enabled } else { Notice::Paused };
        if active || explicit_pause {
            queue(edit, settings, destination, notice);
        } else {
            let _ = clear(pending(edit.alerts, destination));
        }
    }
}

/// Send only to destinations that kept their forwarding generation through
/// the interruption; the claimed notice still verifies the approved binding.
pub(super) fn recovered(alerts: &Alerts, interrupted: [Option<u64>; 2]) {
    let _ = alerts.locked(|edit| {
        let settings = edit.editable()?;
        for destination in Destination::ALL {
            let Some(revision) = interrupted[destination as usize] else { continue };
            if settings.active_revision(destination) == Some(revision)
                && Notice::Recovered.allowed(&settings, destination)
            {
                queue(edit, &settings, destination, Notice::Recovered);
            }
        }
        Ok(())
    });
}

fn queue(edit: &AlertsEdit<'_>, settings: &Settings, destination: Destination, notice: Notice) {
    let alerts = edit.alerts;
    let Some(binding) = settings.notice_binding(destination) else { return };
    if known_refused(alerts, destination) {
        return;
    }
    let path = pending(alerts, destination);
    let state = alerts.state_dir();
    let id = format!("{:016x}", RandomState::new().hash_one(0_u8));
    let record = format!("{id} {} {binding}", notice.name());
    if fs::create_dir_all(state).and_then(|()| preferences::replace(state, &path, &record)).is_err() {
        return;
    }
    if cfg!(test) {
        return;
    }
    let spawned = std::env::current_exe().and_then(|milo| {
        let mut notice = Command::new(milo);
        notice
            .args(["alerts", "send-notice", destination.name(), &id])
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        fs_ops::spawn(&mut notice)
    });
    if let Ok(mut child) = spawned {
        // A long-lived caller (for example, Settings) must also reap helpers.
        std::thread::spawn(move || child.wait());
    } else {
        let _ = clear(pending(alerts, destination));
    }
}

/// Try briefly to acquire the transport lease; once we launch a request,
/// never retry. Waiting for a cancelled curl happens outside the settings lock.
pub(super) fn dispatch(alerts: &Alerts, destination: Destination, id: u64) {
    let lease = (alerts.state_dir(), lock_name(destination));
    for remaining in (0..=5).rev() {
        let mut busy = false;
        let launched = alerts.locked(|edit| {
            let Some(job) = read_pending(alerts, destination).filter(|job| job.id == id) else {
                return Ok(None);
            };
            let path = pending(alerts, destination);
            let result = if lock::held(lease.0, lease.1).unwrap_or(true) {
                Err(io::ErrorKind::WouldBlock.into())
            } else if let Some((settings, secret)) = authorize(edit, destination, job).ok().flatten()
                && let Some(request) =
                    alerts.message_request(destination, &secret, &settings.telegram_chat, job.notice.text(), None)
            {
                // Register curl before unlocking settings, without sending any request bytes.
                send::launch(request, Some(lease)).map(|transport| (transport, settings, secret))
            } else {
                let _ = clear(path);
                return Ok(None);
            };
            if result.as_ref().is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock) && remaining > 0 {
                busy = true;
                return Ok(None);
            }
            // Only claim this job after curl has the lease (or abandon it if no attempt is possible).
            Ok(clear(path).ok().and(result.ok()))
        });
        if let Ok(Some((transport, settings, secret))) = launched {
            if transport.post().refused_credentials() {
                provider_refused(alerts, &settings, destination, &secret);
            }
            return;
        }
        if !busy {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn queues_only_opted_in_transitions_until_the_destination_is_rejected() {
        let fixture = Fixture::folder();
        let alerts = Alerts::at(&fixture.root.join("config"), &fixture.root.join("state"));
        let destination = Destination::Discord;
        alerts.set_discord("https://discord.com/api/webhooks/1234567890/SYNTHETIC_synthetic-SYNTHETIC0").unwrap();

        let take = || {
            let path = pending(&alerts, destination);
            let item = read_pending(&alerts, destination).map(|job| job.notice.name());
            clear(path).unwrap();
            item
        };
        alerts.enable().unwrap();
        assert_eq!(take(), None);
        alerts.locked(|edit| edit.set_notices(destination, true)).unwrap();
        assert_eq!(take(), None);
        alerts.disable().unwrap();
        assert_eq!(take(), Some("paused"));
        alerts.disable().unwrap();
        assert_eq!(take(), None);
        alerts.enable().unwrap();
        assert_eq!(take(), Some("enabled"));
        let interrupted = alerts.settings().unwrap().active_revisions();
        alerts.enable().unwrap();
        assert_eq!(take(), None);
        recovered(&alerts, interrupted);
        assert_eq!(take(), Some("recovered"));

        alerts
            .locked(|_| {
                record_refusal(&alerts, destination);
                Ok(())
            })
            .unwrap();
        alerts.enable().unwrap();
        recovered(&alerts, alerts.settings().unwrap().active_revisions());
        assert_eq!(take(), None);
        assert!(known_refused(&alerts, destination));
        alerts.disable().unwrap();
        assert_eq!(take(), None);
        alerts.enable().unwrap();
        assert_eq!(take(), Some("enabled"));
        assert!(!known_refused(&alerts, destination));
        alerts.forget(destination).unwrap();
        assert!(!known_refused(&alerts, destination));
        assert_eq!(alerts.settings().unwrap().notices, [false; 2]);
    }

    #[test]
    fn a_shared_secret_replacement_can_never_receive_another_profiles_notice() {
        let fixture = Fixture::folder();
        let a = Alerts::at(&fixture.root.join("a-config"), &fixture.root.join("a-state"));
        let mut b = Alerts::at(&fixture.root.join("b-config"), &fixture.root.join("b-state"));
        b.secrets = super::super::secret::Store::File(a.state_dir().join("alerts-secrets"));
        let destination = Destination::Discord;
        const ORIGINAL: &str = "1234567890/SYNTHETIC_synthetic-SYNTHETIC0";
        const REPLACEMENT: &str = "1234567891/SYNTHETIC_synthetic-SYNTHETIC1";
        a.set_discord(&format!("https://discord.com/api/webhooks/{ORIGINAL}")).unwrap();
        a.enable().unwrap();
        a.locked(|edit| edit.set_notices(destination, true)).unwrap();

        a.disable().unwrap();
        let take = || {
            a.locked(|edit| {
                let Some(job) = read_pending(&a, destination) else { return Ok(None) };
                clear(pending(&a, destination)).unwrap();
                Ok(authorize(edit, destination, job)?.map(|(settings, secret)| (settings, job.notice, secret)))
            })
            .unwrap()
        };
        let (_, notice, credential) = take().unwrap();
        assert_eq!((notice.name(), credential.as_str()), ("paused", ORIGINAL));

        a.enable().unwrap();
        a.disable().unwrap();
        assert!(read_pending(&a, destination).is_some());
        b.set_discord(&format!("https://discord.com/api/webhooks/{REPLACEMENT}")).unwrap();
        assert_eq!(a.secrets.get(destination).unwrap().as_deref(), Some(REPLACEMENT));
        assert!(take().is_none()); // A's already queued pause is bound to A's old credential.

        a.enable().unwrap();
        a.disable().unwrap();
        assert!(take().is_none()); // A's next pause still has no consent for B's credential.
        assert!(!a.summary().unwrap().settings.notices[destination as usize]);

        a.locked(|edit| edit.set_notices(destination, true)).unwrap();
        a.enable().unwrap();
        a.disable().unwrap();
        let (_, notice, credential) = take().unwrap();
        assert_eq!((notice.name(), credential.as_str()), ("paused", REPLACEMENT));
    }
}
