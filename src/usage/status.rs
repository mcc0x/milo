//! Captures the optional Claude Code limits supplied to its status line.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::fs_ops::{self, Temp};
use crate::json::Value;
use crate::width::char_width;

use super::files::{self, Budget, FileVersion};
use super::time;
use super::{BaseWindows, Limits, Reading, Window};

const MAX_INPUT: usize = 1024 * 1024;
const MAX_SAVED: u64 = 4096;
const MAX_CONFIG: u64 = 4 * 1024 * 1024;
pub(super) const INPUT_WAIT: Duration = Duration::from_millis(250);
const WINDOWS: [&str; 2] = ["five_hour", "seven_day"];

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
pub(super) fn saved(file: File, version: FileVersion, budget: &mut Budget) -> Option<Reading> {
    let bytes = files::read_small(file, version, MAX_SAVED, budget)?;
    let [saved_at, rate_limits] = Value::parse(&bytes)?.fields(["saved_at", "rate_limits"]);
    let at = saved_at?.integer()?;
    let base = windows(rate_limits?, |value, minutes| window(value, minutes, Observed::Saved(at)));
    base.iter().any(Option::is_some).then_some(Reading { at: time::Timestamp::seconds(at), windows: Limits::new(base) })
}

/// Only inspect normal configurations up to 4 MiB; the cache is a top-level field.
pub(super) fn native(file: File, version: FileVersion, budget: &mut Budget, now: i64) -> Option<Reading> {
    let bytes = files::read_small(file, version, MAX_CONFIG, budget)?;
    let [cache, current] = Value::parse(&bytes)?.fields(["cachedUsageUtilization", "oauthAccount"]);
    let [at, usage, account] = cache?.fields(["fetchedAtMs", "utilization", "accountUuid"]);
    if let Some(account) = account {
        let [current] = current?.fields(["accountUuid"]);
        (account.string(128)? == current?.string(128)?).then_some(())?;
    }
    let at = time::Timestamp::millis(at?)?;
    (at.seconds <= now.saturating_add(super::CLOCK_SKEW)).then_some(())?;
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
pub(super) fn run_fd(input: impl AsFd, output: impl Write, path: Option<&Path>, now: i64, wait: Duration) -> i32 {
    let input = files::read_pipe(input, MAX_INPUT, Instant::now() + wait);
    run(input.as_deref().unwrap_or_default(), output, path, now)
}

/// Even malformed input gets an empty, successful status line.
pub(super) fn run(input: &[u8], mut output: impl Write, path: Option<&Path>, now: i64) -> i32 {
    let line = capture(input, path, now).unwrap_or_default();
    let _ = writeln!(output, "{line}");
    0
}

/// Prefer usage figures for the line and cache; otherwise show the model name.
fn capture(input: &[u8], path: Option<&Path>, now: i64) -> Option<String> {
    if input.len() > MAX_INPUT {
        return None;
    }
    let [limits, model] = Value::parse(input)?.fields(["rate_limits", "model"]);
    if let Some(limits) = limits.filter(|v| v.object()) {
        let windows = windows(limits, |value, minutes| window(value, minutes, Observed::Now(now)));
        let [five, week] = windows;
        let label = |w: Window| if w.is_reset(now) { "reset" } else { w.number() };
        if (five.is_some() || week.is_some())
            && let Some(path) = path
        {
            let _ = save(path, now, windows);
        }
        match (five, week) {
            (Some(a), Some(b)) => return Some(format!("5h {} · week {}", label(a), label(b))),
            (Some(a), None) => return Some(format!("5h {}", label(a))),
            (None, Some(b)) => return Some(format!("week {}", label(b))),
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

fn save(path: &Path, now: i64, windows: BaseWindows) -> io::Result<()> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    // The file is replaced on each save, so take the lock on its directory.
    let lock = fs_ops::open_dir(dir)?;
    lock.lock()?;
    let done = merge_save(path, dir, now, windows);
    done.and(lock.unlock())
}

/// Merge under the directory lock so two status invocations cannot lose a window.
fn merge_save(path: &Path, dir: &Path, now: i64, windows: BaseWindows) -> io::Result<()> {
    let previous = fs_ops::open_read(path)
        .ok()
        .and_then(|file| {
            let version = FileVersion::opened(&file)?;
            saved(file, version, &mut Budget::new(MAX_SAVED + 1))
        })
        .map(|reading| reading.windows.base)
        .unwrap_or_default();
    let merged = std::array::from_fn(|i| newer(previous[i], windows[i]));
    if merged == previous {
        return Ok(());
    }
    write_limits(path, dir, now, merged)
}

/// An older or smaller report must not roll back a known nondecreasing window.
fn newer(previous: Option<Window>, incoming: Option<Window>) -> Option<Window> {
    if let (Some(old), Some(new)) = (previous, incoming)
        && (old.resets > new.resets || old.resets == new.resets && old.percent >= new.percent)
    {
        return previous;
    }
    incoming.or(previous)
}

/// A replacement makes the next read see both windows from the same save.
fn write_limits(path: &Path, dir: &Path, now: i64, windows: BaseWindows) -> io::Result<()> {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    let temporary =
        dir.join(format!(".claude-limits-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
    let (temp, mut file) = Temp::create(temporary)?;
    write!(file, "{{\"saved_at\":{now},\"rate_limits\":{{")?;
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
