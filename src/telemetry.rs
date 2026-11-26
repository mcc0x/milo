//! Anonymous counts of interactive launches. Claim the period before starting a single
//! best-effort request. A lost report costs a count, never a retry or an identifier.

mod notice;
#[cfg(unix)]
#[path = "telemetry/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "telemetry/windows.rs"]
mod platform;
#[cfg(test)]
mod tests;
mod transport;

use std::cell::Cell;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::Child;

use crate::{fs_ops, paths, preferences};

pub(crate) use notice::Notice;

const PREFERENCE: &str = "anonymous-counts.conf";
const HIGH_WATER: &str = "anonymous-counts.state";
const NOTICED: &str = "noticed\n";

pub(crate) fn configured() -> bool {
    endpoint().is_some()
}

fn endpoint() -> Option<&'static str> {
    option_env!("MILO_COUNTS_ENDPOINT").filter(|url| transport::protocol(url).is_some())
}

pub(crate) fn suppressed() -> bool {
    std::env::var_os("MILO_TELEMETRY").is_some_and(|value| value == "0")
        || [
            "CI", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE", "TF_BUILD", "JENKINS_URL", "CODEX_CI", "CODEX_THREAD_ID",
            "CLAUDECODE",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some())
}

fn absolute(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if !matches!(path.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(ErrorKind::InvalidInput.into());
    }
    if path.is_absolute() && !path.components().any(|part| part == Component::ParentDir) {
        Ok(())
    } else {
        Err(ErrorKind::InvalidInput.into())
    }
}

pub(crate) fn enabled(config: &Path) -> io::Result<bool> {
    absolute(config)?;
    match fs_ops::open_dir(config) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    }
    match preferences::read(&config.join(PREFERENCE), 8)?.as_deref() {
        None | Some("on\n") => Ok(true),
        Some("off\n") => Ok(false),
        _ => Err(ErrorKind::InvalidData.into()),
    }
}

pub(crate) fn set_enabled(config: &Path, value: bool) -> io::Result<()> {
    let file = platform::lock_file(config, c"anonymous-counts-settings.lock")?;
    #[cfg(unix)]
    file.lock()?;
    #[cfg(windows)]
    fs_ops::lock_dir(&file)?;
    let _lock = Held(file);
    save(config, PREFERENCE, if value { "on\n" } else { "off\n" })
}

pub(crate) fn notice_shown(state: &Path) -> io::Result<bool> {
    absolute(state)?;
    match preferences::read(&state.join(HIGH_WATER), 24)? {
        None => Ok(false),
        Some(text) if text == NOTICED || saved_day(&text).is_some() => Ok(true),
        _ => Err(ErrorKind::InvalidData.into()),
    }
}

fn saved_day(text: &str) -> Option<i64> {
    let value = text.strip_suffix('\n')?;
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let day = value.parse::<i64>().ok()?;
    utc_date(day).map(|_| day)
}

struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        // A concurrent fork can still hold our open file description before its exec
        // closes CLOEXEC descriptors. Release the lock even while that copy exists.
        let _ = self.0.unlock();
    }
}

fn save(dir: &Path, name: &str, value: &str) -> io::Result<()> {
    let staged = dir.join(format!(".{name}.tmp"));
    match fs::remove_file(&staged) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let (temp, mut file) = fs_ops::Temp::create(staged)?;
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    drop(file);
    platform::publish(temp, dir, name)
}

struct Visit {
    config: PathBuf,
    state: PathBuf,
    _guard: Held,
}

impl Visit {
    fn enter(config: PathBuf, state: PathBuf) -> io::Result<Self> {
        if !enabled(&config)? {
            return Err(ErrorKind::PermissionDenied.into());
        }
        // Never wait for another startup. In particular, skip every other launch
        // for the entire lifetime of the interactive process that presents the notice.
        let guard = platform::lock_file(&state, c"anonymous-counts-session.lock")?;
        guard.try_lock()?;
        Ok(Self { config, state, _guard: Held(guard) })
    }

    fn last(&self) -> io::Result<Option<String>> {
        preferences::read(&self.state.join(HIGH_WATER), 24)
    }

    fn acknowledge(&self, keep_on: bool) -> io::Result<()> {
        if !keep_on {
            set_enabled(&self.config, false)?;
        }
        save(&self.state, HIGH_WATER, NOTICED)
    }

    fn report(&self, last: &str, now: i64, url: &str) -> Option<Child> {
        let day = now.checked_div(crate::when::DAY).filter(|_| now >= 0)?;
        let previous = if last == NOTICED { None } else { Some(saved_day(last)?) };
        if previous.is_some_and(|previous| previous >= day) {
            return None;
        }
        let date = utc_date(day)?;
        let monthly = match previous {
            None => true,
            Some(previous) => utc_date(previous)?[..7] != date[..7],
        };
        let lock = platform::lock_file(&self.config, c"anonymous-counts-settings.lock").ok()?;
        lock.try_lock().ok()?;
        let _settings = Held(lock);
        if !enabled(&self.config).ok()? {
            return None;
        }
        save(&self.state, HIGH_WATER, &format!("{day}\n")).ok()?;
        transport::start(url, &date, previous.is_none(), monthly)
    }
}

fn utc_date(day: i64) -> Option<String> {
    // From the Unix epoch through 9999-12-31, including the Gregorian century rule.
    if !(0..=2_932_896).contains(&day) {
        return None;
    }
    let mut year = 1970 + 400 * (day / 146_097);
    let mut ordinal = day % 146_097;
    let leap = |y| i64::from(y % 4 == 0 && (y % 100 != 0 || y % 400 == 0));
    while ordinal >= 365 + leap(year) {
        ordinal -= 365 + leap(year);
        year += 1;
    }
    let lengths = [31, 28 + leap(year), 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for (i, length) in lengths.into_iter().enumerate() {
        if ordinal < length {
            return Some(format!("{year:04}-{:02}-{:02}", i + 1, ordinal + 1));
        }
        ordinal -= length;
    }
    None
}

/// Dropping this releases the first-session lock; retain it across the original pane and its handoffs.
pub(crate) struct First {
    visit: Visit,
    pub(crate) proceed: Cell<bool>,
}

impl First {
    pub(crate) fn start() -> Option<Self> {
        let url = endpoint()?;
        if suppressed() {
            return None;
        }
        let visit = Visit::enter(paths::config_dir()?, paths::state_dir()?).ok()?;
        let Some(last) = visit.last().ok()? else { return Some(Self { visit, proceed: Cell::new(false) }) };
        let _ = std::thread::Builder::new().spawn(move || {
            let child = visit.report(&last, crate::when::now(), url);
            drop(visit);
            if let Some(mut child) = child {
                let _ = child.wait();
            }
        });
        None
    }
}
