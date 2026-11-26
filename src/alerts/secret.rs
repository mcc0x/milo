//! Keeps a destination's secret: a Telegram bot token or relay capability,
//! or the `ID/TOKEN` of a Discord webhook link. Each allows posting.
//!
//! On macOS the secret goes in the user's Keychain, through `/usr/bin/security`.
//! Elsewhere it goes in a file only the user can read, beside Milo's settings.
//! A secret is never put on a command line, where other programs could see it.

use std::borrow::Cow;
use std::fs;
use std::io::{self, ErrorKind, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{Destination, relay};
use crate::fs_ops;

const SECURITY: &str = "/usr/bin/security";
const SERVICE: &str = "net.usemilo.alerts";
/// What `security` exits with when the Keychain has no such item.
const NOT_FOUND: i32 = 44;
const SECRET_LIMIT: u64 = 512;
/// Names a keychain file of a test's own, so that tests leave the user's
/// alone. A release build does not read it.
const TEST_KEYCHAIN: &str = "MILO_ALERTS_KEYCHAIN";

pub(super) fn test_keychain() -> Option<PathBuf> {
    cfg!(debug_assertions)
        .then(|| std::env::var_os(TEST_KEYCHAIN))
        .flatten()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// A hosted Telegram entry holds the capability and its verified recipient.
/// Old entries held only the capability; they can be revoked but cannot send.
pub(super) fn telegram_binding(entry: &str) -> Option<(&str, Option<&str>)> {
    let (capability, chat) = entry.split_once(':').map_or((entry, None), |(capability, chat)| (capability, Some(chat)));
    relay::valid_capability(capability).then_some((capability, chat))
}

pub(crate) enum Store {
    /// Each config profile has its own service in the selected Keychain.
    Keychain {
        path: Option<PathBuf>,
        service: Cow<'static, str>,
    },
    File(PathBuf),
}

impl Store {
    /// On macOS the Keychain. Elsewhere a file in `state`, which, unlike the
    /// settings folder, is not meant to be copied to another machine.
    pub(super) fn real(state: &Path, keychain: Option<PathBuf>, scope: Option<&str>) -> Self {
        if cfg!(target_os = "macos") {
            let service = scope.map_or(Cow::Borrowed(SERVICE), |scope| Cow::Owned(format!("{SERVICE}.{scope}")));
            Self::Keychain { path: keychain, service }
        } else {
            Self::File(state.join("alerts-secrets"))
        }
    }

    /// Where secrets are kept, to tell the user.
    pub(crate) fn place(&self) -> &'static str {
        match self {
            Self::Keychain { .. } => "your macOS Keychain",
            Self::File(_) => "a file only you can read (alerts-secrets, in Milo's state folder)",
        }
    }

    pub(crate) fn get(&self, destination: Destination) -> io::Result<Option<String>> {
        match self {
            Self::Keychain { path, service } => {
                let mut find = Command::new(SECURITY);
                find.args(["find-generic-password", "-s", service.as_ref(), "-a", destination.name(), "-w"]);
                find.args(path).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
                let output = fs_ops::spawn(&mut find)?.wait_with_output()?;
                match output.status.code() {
                    Some(0) => String::from_utf8(output.stdout)
                        .map(|secret| Some(secret.trim_end().to_owned()))
                        .map_err(|_| ErrorKind::InvalidData.into()),
                    Some(NOT_FOUND) => Ok(None),
                    _ => Err(io::Error::other("the Keychain did not answer")),
                }
            }
            Self::File(path) => {
                Ok(read_file(path)?.into_iter().find(|(name, _)| name == destination.name()).map(|(_, secret)| secret))
            }
        }
    }

    pub(crate) fn set(&self, destination: Destination, secret: &str) -> io::Result<()> {
        // Both stores hold one secret per line or command, unquoted.
        if secret.is_empty() || !secret.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-:/".contains(&c)) {
            return Err(ErrorKind::InvalidInput.into());
        }
        match self {
            Self::Keychain { path, service } => {
                let keychain = match path {
                    Some(path) => format!(" \"{}\"", quotable(path)?),
                    None => String::new(),
                };
                // Commands read from standard input keep the secret off the command line.
                let add = format!(
                    "add-generic-password -U -s {service} -a {} -l \"Milo alert mirroring\" -w {secret}{keychain}\n",
                    destination.name()
                );
                let mut security = Command::new(SECURITY);
                security.arg("-i").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
                let mut security = fs_ops::spawn(&mut security)?;
                let written = security.stdin.take().map(|mut input| input.write_all(add.as_bytes()));
                security.wait()?;
                written.transpose()?;
                // `security -i` succeeds whatever its commands did.
                if self.get(destination)?.as_deref() == Some(secret) {
                    Ok(())
                } else {
                    Err(io::Error::other("the Keychain did not keep the secret"))
                }
            }
            Self::File(path) => {
                let mut secrets = read_file(path)?;
                secrets.retain(|(name, _)| name != destination.name());
                secrets.push((destination.name().to_owned(), secret.to_owned()));
                write_file(path, &secrets)
            }
        }
    }

    pub(crate) fn remove(&self, destination: Destination) -> io::Result<()> {
        match self {
            Self::Keychain { path, service } => {
                let mut delete = Command::new(SECURITY);
                delete.args(["delete-generic-password", "-s", service.as_ref(), "-a", destination.name()]);
                delete.args(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                let status = fs_ops::spawn(&mut delete)?.wait()?;
                match status.code() {
                    Some(0 | NOT_FOUND) => Ok(()),
                    _ => Err(io::Error::other("the Keychain did not answer")),
                }
            }
            Self::File(path) => {
                let mut secrets = read_file(path)?;
                secrets.retain(|(name, _)| name != destination.name());
                if secrets.is_empty() {
                    match fs::remove_file(path) {
                        Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
                        _ => Ok(()),
                    }
                } else {
                    write_file(path, &secrets)
                }
            }
        }
    }
}

/// A keychain path that can sit between double quotes as it is.
fn quotable(path: &Path) -> io::Result<&str> {
    path.to_str().filter(|path| !path.contains(['"', '\\', '\n'])).ok_or_else(|| ErrorKind::InvalidInput.into())
}

/// The file's `name = secret` lines. A file others can read is refused.
fn read_file(path: &Path) -> io::Result<Vec<(String, String)>> {
    let file = match fs_ops::open_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let info = file.metadata()?;
    if !private(&info) {
        return Err(ErrorKind::PermissionDenied.into());
    }
    let mut text = String::new();
    file.take(SECRET_LIMIT * 4).read_to_string(&mut text)?;
    Ok(text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, secret)| (name.trim().to_owned(), secret.trim().to_owned()))
        .collect())
}

/// Whether only the user can read the file. Windows access lists are not checked yet, so no file qualifies there.
fn private(info: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        info.is_file() && info.permissions().mode() & 0o077 == 0
    }
    #[cfg(windows)]
    {
        let _ = info;
        false
    }
}

/// Replaces the file with one made private before anything is written to it.
fn write_file(path: &Path, secrets: &[(String, String)]) -> io::Result<()> {
    if cfg!(windows) {
        return Err(ErrorKind::Unsupported.into());
    }
    // AlertsEdit already holds the transaction lock, even if config and state share a directory.
    let dir = path.parent().ok_or(ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    let _directory = fs_ops::open_dir(dir)?;
    let (temp, mut file) = fs_ops::Temp::create(dir.join(format!(".milo-alerts-{}", std::process::id())))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    for (name, secret) in secrets {
        writeln!(file, "{name} = {secret}")?;
    }
    drop(file);
    temp.replace(path)
}
