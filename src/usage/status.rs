//! Captures the optional Claude Code limits supplied to its status line.

use std::fs::{self, File, TryLockError};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::fs_ops::{self, Temp};
use crate::json::Value;
use crate::when::MINUTE;
use crate::width::char_width;

use super::files::{self, Budget, FileVersion};
use super::time;
use super::{BaseWindows, Limits, Reading, Window};

const MAX_INPUT: usize = 1024 * 1024;
const MAX_SAVED: u64 = 4096;
const MAX_CONFIG: u64 = 4 * 1024 * 1024;
pub(super) const INPUT_WAIT: Duration = Duration::from_millis(250);
/// Another status call holds the lock only while it saves its own report.
const LOCK_WAIT: Duration = Duration::from_millis(100);
const WINDOWS: [&str; 2] = ["five_hour", "seven_day"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct AccountId(u128);

impl AccountId {
    fn uuid(value: Value<'_>) -> Option<Self> {
        let uuid = value.string(36)?;
        let bytes = uuid.as_bytes();
        if bytes.len() != 36 {
            return None;
        }
        let mut id = 0_u128;
        for (i, &byte) in bytes.iter().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                (byte == b'-').then_some(())?;
            } else {
                id = (id << 4) | u128::from(char::from(byte).to_digit(16)?);
            }
        }
        Some(Self(id))
    }
}

/// Claude's status input does not name its account. Only the active profile's
/// OAuth account can identify it, not the session, model or reset times.
fn active_account(oauth: Option<Value<'_>>) -> Option<AccountId> {
    let [uuid] = oauth?.fields(["accountUuid"]);
    AccountId::uuid(uuid?)
}

pub(super) struct Saved {
    reading: Reading,
    account: Option<AccountId>,
}

impl Saved {
    pub(super) fn for_account(&self, current: Option<AccountId>) -> Option<&Reading> {
        (self.account == current).then_some(&self.reading)
    }
}

pub(super) struct Native {
    pub(super) reading: Option<Reading>,
    pub(super) account: Option<AccountId>,
}

pub(super) struct SaveTo<'a> {
    pub(super) path: &'a Path,
    pub(super) config: Option<&'a Path>,
}

/// A saved window can be older than the file if a later report omitted it.
#[derive(Clone, Copy)]
enum Observed {
    Now(i64),
    Saved(i64),
}

fn windows<'a>(object: Value<'a>, mut read: impl FnMut(Option<Value<'a>>, u64) -> Option<Window>) -> BaseWindows {
    let [five, week] = object.fields(WINDOWS);
    [read(five, 300), read(week, 10_080)]
}

fn window(value: Option<Value<'_>>, minutes: u64, observed: Observed) -> Option<Window> {
    let [percent, resets, at] = value?.fields(["used_percentage", "resets_at", "observed_at"]);
    let observed = match observed {
        Observed::Now(now) => now,
        Observed::Saved(saved) => at.and_then(Value::integer).unwrap_or(saved),
    };
    Some(Window {
        minutes,
        percent: Window::percent(percent?)?,
        resets: resets.and_then(|value| time::reset(value, observed)),
        observed,
    })
}

/// The status helper's own cache can preserve windows from different reports.
pub(super) fn saved(file: File, version: FileVersion, budget: &mut Budget) -> Option<Saved> {
    let bytes = files::read_small(file, version, MAX_SAVED, budget)?;
    let [saved_at, rate_limits, account] = Value::parse(&bytes)?.fields(["saved_at", "rate_limits", "account"]);
    let account = match account {
        Some(account) => Some(AccountId(u128::from_str_radix(&account.string(32)?, 16).ok()?)),
        None => None,
    };
    let at = saved_at?.integer()?;
    let base = windows(rate_limits?, |value, minutes| window(value, minutes, Observed::Saved(at)));
    base.iter()
        .any(Option::is_some)
        .then_some(Saved { reading: Reading { at: time::Timestamp::seconds(at), windows: Limits::new(base) }, account })
}

/// Only inspect normal configurations up to 4 MiB; the cache is a top-level field.
pub(super) fn native(file: File, version: FileVersion, budget: &mut Budget) -> Option<Native> {
    let bytes = files::read_small(file, version, MAX_CONFIG, budget)?;
    let [cache, oauth] = Value::parse(&bytes)?.fields(["cachedUsageUtilization", "oauthAccount"]);
    let account = active_account(oauth);
    Some(Native { reading: cache.and_then(|cache| native_reading(cache, account)), account })
}

fn native_reading(cache: Value<'_>, current: Option<AccountId>) -> Option<Reading> {
    let [at, usage, account] = cache.fields(["fetchedAtMs", "utilization", "accountUuid"]);
    match account {
        Some(uuid) => (Some(AccountId::uuid(uuid)?) == current).then_some(())?,
        None => current.is_none().then_some(())?,
    }
    let at = time::Timestamp::millis(at?)?;
    let usage = usage.filter(|value| value.object() || value.null())?;
    let base = windows(usage, |value, minutes| {
        let [percent, reset] = value?.fields(["utilization", "resets_at"]);
        Some(Window {
            minutes,
            percent: Window::percent(percent?)?,
            resets: reset.and_then(time::Timestamp::iso).and_then(|stamp| time::cache_reset(stamp, at.seconds)),
            observed: at.seconds,
        })
    });
    Some(Reading { at, windows: Limits::new(base) })
}

/// A stalled stdin must not leave Claude waiting for its status line.
pub(super) fn run_fd(
    input: impl AsFd,
    output: impl Write,
    target: Option<SaveTo<'_>>,
    now: i64,
    wait: Duration,
) -> i32 {
    let input = files::read_pipe(input, MAX_INPUT, Instant::now() + wait);
    render(input.as_deref().unwrap_or_default(), output, target, now)
}

#[cfg(test)]
pub(super) fn run(input: &[u8], output: impl Write, path: Option<&Path>, now: i64) -> i32 {
    render(input, output, path.map(|path| SaveTo { path, config: None }), now)
}

/// Even malformed input gets an empty, successful status line.
pub(super) fn render(input: &[u8], mut output: impl Write, target: Option<SaveTo<'_>>, now: i64) -> i32 {
    let line = capture(input, target, now).unwrap_or_default();
    let _ = writeln!(output, "{line}");
    0
}

/// Prefer usage figures for the line and cache; otherwise show the model name.
fn capture(input: &[u8], target: Option<SaveTo<'_>>, now: i64) -> Option<String> {
    if input.len() > MAX_INPUT {
        return None;
    }
    let [limits, model] = Value::parse(input)?.fields(["rate_limits", "model"]);
    if let Some(limits) = limits.filter(|v| v.object()) {
        let windows = windows(limits, |value, minutes| window(value, minutes, Observed::Now(now)));
        let [five, week] = windows;
        let label = |w: Window| if w.is_reset(now) { ("reset elapsed", "") } else { (w.number(), " left") };
        if (five.is_some() || week.is_some())
            && let Some(target) = target
        {
            let _ = save(target, now, windows);
        }
        match (five, week) {
            (Some(a), Some(b)) if a.is_reset(now) || b.is_reset(now) => {
                let ((a, a_end), (b, b_end)) = (label(a), label(b));
                return Some(format!("5h {a}{a_end} · week {b}{b_end}"));
            }
            (Some(a), Some(b)) => return Some(format!("5h {} · week {} left", a.number(), b.number())),
            (Some(w), None) | (None, Some(w)) => {
                let name = if five.is_some() { "5h" } else { "week" };
                let (value, end) = label(w);
                return Some(format!("{name} {value}{end}"));
            }
            (None, None) => {}
        }
    }
    model_name(model?)
}

/// The helper prints to Claude's terminal, so leave out controls and long names.
fn model_name(model: Value<'_>) -> Option<String> {
    let [name, id] = model.fields(["display_name", "id"]);
    let value = name.or(id).unwrap_or(model).string(128)?;
    let mut safe: String = value
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_' | '\u{0300}'..='\u{036f}'))
        .skip_while(|&c| c == ' ' || char_width(c) == 0)
        .scan(0, |columns, c| {
            *columns += char_width(c);
            (*columns <= 40).then_some(c)
        })
        .collect();
    safe.truncate(safe.trim_end().len());
    Some(safe)
}

fn save(target: SaveTo<'_>, now: i64, windows: BaseWindows) -> io::Result<()> {
    let dir = target.path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    // The file is replaced on each save, so take the lock on its directory.
    let lock = fs_ops::open_dir(dir)?;
    let deadline = Instant::now() + LOCK_WAIT;
    // A holder that has stalled must not keep Claude waiting for its status
    // line. Skipping this save loses nothing the next report will not bring.
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
    let done = merge_save(target, dir, now, windows);
    done.and(lock.unlock())
}

fn configured_account(path: &Path) -> io::Result<Option<AccountId>> {
    let file = match fs_ops::open_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let invalid = io::ErrorKind::InvalidData;
    let version = FileVersion::opened(&file).ok_or(invalid)?;
    let bytes = files::read_small(file, version, MAX_CONFIG, &mut Budget::new(MAX_CONFIG + 1)).ok_or(invalid)?;
    let [oauth] = Value::parse(&bytes).filter(|value| value.object()).ok_or(invalid)?.fields(["oauthAccount"]);
    Ok(active_account(oauth))
}

/// Merge under the directory lock so two status invocations cannot lose a window.
/// A save for another account or with an invalid date contributes no windows.
fn merge_save(target: SaveTo<'_>, dir: &Path, now: i64, windows: BaseWindows) -> io::Result<()> {
    let account = target.config.map(configured_account).transpose()?.flatten();
    let previous = fs_ops::open_read(target.path).ok().and_then(|file| {
        let version = FileVersion::opened(&file)?;
        saved(file, version, &mut Budget::new(MAX_SAVED + 1))
    });
    let previous =
        previous.as_ref().and_then(|saved| saved.for_account(account)).filter(|reading| reading.visible_at() <= now);
    let base = previous.map_or([None; 2], |reading| reading.windows.base);
    let merged = std::array::from_fn(|i| newer(base[i], windows[i]));
    if merged == base {
        return Ok(());
    }
    let at = previous.map_or(now, |reading| now.max(reading.at.seconds));
    write_limits(target.path, dir, at, merged, account)
}

/// An older or smaller report must not roll back a known nondecreasing window.
fn newer(previous: Option<Window>, incoming: Option<Window>) -> Option<Window> {
    let (Some(old), Some(mut new)) = (previous, incoming) else {
        return incoming.or(previous);
    };
    // An omitted reset is not evidence of a new window. Keep the known
    // deadline while accepting an otherwise valid update to this window.
    new.resets = new.resets.or(old.resets);
    if new.observed < old.observed || new.resets < old.resets {
        return previous;
    }
    if new.resets == old.resets {
        match new.percent.total_cmp(&old.percent) {
            std::cmp::Ordering::Less => return previous,
            // Identical reports confirm the reading without replacing the file on every status call.
            std::cmp::Ordering::Equal
                if new.is_reset(new.observed) || new.observed.saturating_sub(old.observed) < MINUTE =>
            {
                return previous;
            }
            _ => {}
        }
    }
    Some(new)
}

/// A replacement makes the next read see both windows from the same save.
fn write_limits(path: &Path, dir: &Path, now: i64, windows: BaseWindows, account: Option<AccountId>) -> io::Result<()> {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    let temporary =
        dir.join(format!(".claude-limits-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
    let (temp, mut file) = Temp::create(temporary)?;
    write!(file, "{{\"saved_at\":{now}")?;
    if let Some(AccountId(id)) = account {
        write!(file, ",\"account\":\"{id:032x}\"")?;
    }
    write!(file, ",\"rate_limits\":{{")?;
    for (i, (key, window)) in WINDOWS.into_iter().zip(windows).enumerate() {
        if i > 0 {
            write!(file, ",")?;
        }
        write!(file, "\"{key}\":")?;
        if let Some(window) = window {
            let percent = window.percent;
            write!(file, "{{\"used_percentage\":{percent},\"observed_at\":{}", window.observed)?;
            if let Some(resets) = window.resets {
                write!(file, ",\"resets_at\":{resets}")?;
            }
            write!(file, "}}")?;
        } else {
            write!(file, "null")?;
        }
    }
    writeln!(file, "}}}}")?;
    drop(file);
    temp.replace(path)
}
