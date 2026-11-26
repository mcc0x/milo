//! Anonymous counts of interactive launches. Claim the period before starting a single
//! best-effort request. A lost report costs a count, never a retry or an identifier.

#[cfg(unix)]
#[path = "telemetry/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "telemetry/windows.rs"]
mod platform;
#[cfg(test)]
mod tests;

use std::fs::{self, File};
use std::io::{self, ErrorKind, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::Child;

use crate::{fs_ops, paths, preferences};

const PREFERENCE: &str = "anonymous-counts.conf";
const HIGH_WATER: &str = "anonymous-counts.state";
const FIRST_SEEN: &str = "first-seen\n";

pub(crate) fn configured() -> bool {
    endpoint().is_some()
}

fn endpoint() -> Option<&'static str> {
    // Evaluate this gate at compile time so an ordinary binary does not embed a
    // production URL even when the packaging environment accidentally sets one.
    const BUILD_URL: Option<&str> = match option_env!("MILO_COUNTS_ENDPOINT") {
        Some(url) if cfg!(milo_distribution_counts) || matches!(url.as_bytes(), [b'h', b't', b't', b'p', b':', ..]) => {
            Some(url)
        }
        _ => None,
    };
    BUILD_URL.filter(|url| crate::usage::counts_protocol(url).is_some())
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
        // for the entire lifetime of the first eligible interactive session.
        let guard = platform::lock_file(&state, c"anonymous-counts-session.lock")?;
        guard.try_lock()?;
        Ok(Self { config, state, _guard: Held(guard) })
    }

    fn settings_when_enabled(&self) -> io::Result<Held> {
        let lock = platform::lock_file(&self.config, c"anonymous-counts-settings.lock")?;
        lock.try_lock()?;
        let settings = Held(lock);
        if !enabled(&self.config)? {
            return Err(ErrorKind::PermissionDenied.into());
        }
        Ok(settings)
    }

    fn previous_or_mark_first(&self) -> io::Result<Option<String>> {
        let previous = preferences::read(&self.state.join(HIGH_WATER), 24)?;
        if previous.is_none() {
            let _settings = self.settings_when_enabled()?;
            save(&self.state, HIGH_WATER, FIRST_SEEN)?;
        }
        Ok(previous)
    }

    fn report(&self, last: &str, now: i64, url: &str) -> Option<Child> {
        let day = now.checked_div(crate::when::DAY).filter(|_| now >= 0)?;
        let previous = match last {
            FIRST_SEEN | "noticed\n" => None,
            text => Some(saved_day(text)?),
        };
        if previous.is_some_and(|previous| previous >= day) {
            return None;
        }
        let date = utc_date(day)?;
        let monthly = match previous {
            None => true,
            Some(previous) => utc_date(previous)?[..7] != date[..7],
        };
        let _settings = self.settings_when_enabled().ok()?;
        save(&self.state, HIGH_WATER, &format!("{day}\n")).ok()?;
        crate::usage::post_anonymous_counts(url, &date, previous.is_none(), monthly)
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

/// Keep the returned lock until the pane and its handoffs close, so that a concurrent
/// launch cannot mistake the new first-seen marker for a completed first session.
pub(crate) fn start() -> Option<impl Drop> {
    if suppressed() {
        return None;
    }
    let url = endpoint()?;
    let visit = Visit::enter(paths::config_dir()?, paths::state_dir()?).ok()?;
    let Ok(Some(last)) = visit.previous_or_mark_first() else { return Some(visit._guard) };
    let _ = std::thread::Builder::new().spawn(move || {
        let child = visit.report(&last, crate::when::now(), url);
        drop(visit);
        if let Some(mut child) = child {
            let _ = child.wait();
        }
    });
    None
}
