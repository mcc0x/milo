//! Captures the optional Claude Code limits supplied to its status line.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use super::files::{self, Budget, Mark};
use super::json::Value;
use super::time::{self, Stamp};
use super::{Snapshot, Window};

const MAX_INPUT: u64 = 1024 * 1024;
const MAX_SAVED: u64 = 4096;

fn windows(object: Value<'_>) -> [Option<Window>; 2] {
    let [five_hour, seven_day] = object.fields(["five_hour", "seven_day"]);
    [window(five_hour, 300), window(seven_day, 10_080)]
}

fn window(value: Option<Value<'_>>, minutes: u64) -> Option<Window> {
    let [percent, resets] = value?.fields(["used_percentage", "resets_at"]);
    Some(Window { minutes, percent: Window::percent(percent?)?, resets: resets.and_then(time::reset) })
}

pub(super) fn saved(file: File, mark: Mark, budget: &mut Budget) -> Option<Snapshot> {
    if mark.len > MAX_SAVED || budget.remaining <= mark.len {
        return None;
    }
    let mut bytes = Vec::new();
    let result = file.take((MAX_SAVED + 1).min(budget.remaining)).read_to_end(&mut bytes);
    budget.debit(bytes.len());
    result.ok()?;
    if u64::try_from(bytes.len()).ok()? > MAX_SAVED {
        return None;
    }
    let [saved_at, rate_limits] = Value::parse(&bytes)?.fields(["saved_at", "rate_limits"]);
    Snapshot::new(Stamp::at(saved_at?.integer()?), windows(rate_limits?))
}

pub(super) fn run(input: impl Read, mut output: impl Write, path: Option<&Path>, now: i64) -> i32 {
    let line = capture(input, path, now).unwrap_or_default();
    let _ = writeln!(output, "{line}");
    0
}

fn capture(input: impl Read, path: Option<&Path>, now: i64) -> Option<String> {
    let mut bytes = Vec::new();
    input.take(MAX_INPUT + 1).read_to_end(&mut bytes).ok()?;
    if u64::try_from(bytes.len()).ok()? > MAX_INPUT {
        return None;
    }
    let [limits, model] = Value::parse(&bytes)?.fields(["rate_limits", "model"]);
    if let Some(limits) = limits.filter(|v| v.object()) {
        let windows = windows(limits);
        save(path?, now, windows).ok()?;
        let [five, week] = windows;
        let label = |w: Window| if w.is_reset(now) { "reset".into() } else { w.number() };
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

fn save(path: &Path, now: i64, windows: [Option<Window>; 2]) -> io::Result<()> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
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
                write!(file, "{{\"used_percentage\":{percent}")?;
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

pub(super) fn read(path: &Path, previous: &mut Option<Mark>, snapshot: &mut Option<Snapshot>, budget: &mut Budget) {
    let Some(mark) = fs::symlink_metadata(path).ok().filter(|m| m.is_file()) else {
        *previous = None;
        *snapshot = None;
        return;
    };
    let mark = Mark::metadata(&mark);
    if *previous == Some(mark) {
        return;
    }
    *snapshot = files::open_file(path).ok().and_then(|file| {
        let mark = Mark::opened(&file)?;
        *previous = Some(mark);
        saved(file, mark, budget)
    });
}
