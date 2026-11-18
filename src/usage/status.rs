//! Captures the optional Claude Code limits supplied to its status line.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::files::{self, Budget, Mark};
use super::json::Value;
use super::time;
use super::{BaseWindows, Limits, Reading, Window};

const MAX_INPUT: usize = 1024 * 1024;
const MAX_SAVED: u64 = 4096;
const MAX_CONFIG: u64 = 4 * 1024 * 1024;
const INPUT_WAIT: Duration = Duration::from_millis(250);

/// A saved window can be older than the file if a later report omitted it.
#[derive(Clone, Copy)]
enum Observed {
    Now(i64),
    Saved(i64),
}

fn windows(object: Value<'_>, observed: Observed) -> BaseWindows {
    let [five_hour, seven_day] = object.fields(["five_hour", "seven_day"]);
    [window(five_hour, 300, observed), window(seven_day, 10_080, observed)]
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

pub(super) fn saved(file: File, mark: Mark, budget: &mut Budget) -> Option<Reading> {
    let bytes = files::read_small(file, mark, MAX_SAVED, budget)?;
    let [saved_at, rate_limits] = Value::parse(&bytes)?.fields(["saved_at", "rate_limits"]);
    let at = saved_at?.integer()?;
    let base = windows(rate_limits?, Observed::Saved(at));
    base.iter().any(Option::is_some).then_some(Reading { at: time::Stamp::seconds(at), windows: Limits::new(base) })
}

/// Only inspect normal configurations up to 4 MiB; the cache is a top-level field.
pub(super) fn native(file: File, mark: Mark, budget: &mut Budget, now: i64) -> Option<Reading> {
    let bytes = files::read_small(file, mark, MAX_CONFIG, budget)?;
    let [cache, current] = Value::parse(&bytes)?.fields(["cachedUsageUtilization", "oauthAccount"]);
    let [at, usage, account] = cache?.fields(["fetchedAtMs", "utilization", "accountUuid"]);
    if let Some(account) = account {
        let [current] = current?.fields(["accountUuid"]);
        (account.string(128)? == current?.string(128)?).then_some(())?;
    }
    let at = time::Stamp::millis(at?)?;
    (at.seconds <= now.saturating_add(super::CLOCK_SKEW)).then_some(())?;
    let [five, week] = usage.filter(|object| object.object())?.fields(["five_hour", "seven_day"]);
    let window = |value: Option<Value<'_>>, minutes| {
        let [percent, reset] = value?.fields(["utilization", "resets_at"]);
        Some(Window {
            minutes,
            percent: Window::percent(percent?)?,
            resets: reset.and_then(time::Stamp::iso).and_then(|stamp| time::cache_reset(stamp, at.seconds)),
            observed: at.seconds,
        })
    };
    let base = [window(five, 300), window(week, 10_080)];
    Some(Reading { at, windows: Limits::new(base) })
}

pub(super) fn run_fd(input: impl AsFd, output: impl Write, path: Option<&Path>, now: i64) -> i32 {
    let input = files::read_pipe(input, MAX_INPUT, Instant::now() + INPUT_WAIT);
    run(input.as_deref().unwrap_or_default(), output, path, now)
}

pub(super) fn run(input: &[u8], mut output: impl Write, path: Option<&Path>, now: i64) -> i32 {
    let line = capture(input, path, now).unwrap_or_default();
    let _ = writeln!(output, "{line}");
    0
}

fn capture(input: &[u8], path: Option<&Path>, now: i64) -> Option<String> {
    if input.len() > MAX_INPUT {
        return None;
    }
    let [limits, model] = Value::parse(input)?.fields(["rate_limits", "model"]);
    if let Some(limits) = limits.filter(|v| v.object()) {
        let windows = windows(limits, Observed::Now(now));
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

fn model_name(model: Value<'_>) -> Option<String> {
    let [name, id] = model.fields(["display_name", "id"]);
    let value = name.or(id).unwrap_or(model).string(128)?;
    let safe: String =
        value.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_')).take(40).collect();
    Some(safe.trim().to_owned())
}

fn save(path: &Path, now: i64, windows: BaseWindows) -> io::Result<()> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    // The file is replaced on each save, so take the lock on its directory.
    let lock = files::open_directory(dir)?;
    lock.lock()?;
    let done = merge_save(path, dir, now, windows);
    done.and(lock.unlock())
}

fn merge_save(path: &Path, dir: &Path, now: i64, windows: BaseWindows) -> io::Result<()> {
    let previous = files::open_file(path)
        .ok()
        .and_then(|file| {
            let mark = Mark::opened(&file)?;
            saved(file, mark, &mut Budget::new(MAX_SAVED + 1))
        })
        .map(|reading| reading.windows.base)
        .unwrap_or_default();
    let merged = std::array::from_fn(|i| newer(previous[i], windows[i]));
    if merged == previous {
        return Ok(());
    }
    write_limits(path, dir, now, merged)
}

fn newer(previous: Option<Window>, incoming: Option<Window>) -> Option<Window> {
    if let (Some(old), Some(new)) = (previous, incoming)
        && (old.resets > new.resets || old.resets == new.resets && old.percent >= new.percent)
    {
        return previous;
    }
    incoming.or(previous)
}

fn write_limits(path: &Path, dir: &Path, now: i64, windows: BaseWindows) -> io::Result<()> {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    let temporary =
        dir.join(format!(".claude-limits-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
    let done = (|| {
        write!(file, "{{\"saved_at\":{now},\"rate_limits\":{{")?;
        for (i, (key, window)) in ["five_hour", "seven_day"].into_iter().zip(windows).enumerate() {
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
        fs::rename(&temporary, path)
    })();
    if done.is_err() {
        let _ = fs::remove_file(temporary);
    }
    done
}
