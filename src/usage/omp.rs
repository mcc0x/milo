//! Reads OMP's saved account limits without invoking the harness or its login.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::fs_ops;
use crate::json::Value;

use super::files;
use super::time::{self, Timestamp};
use super::{CLOCK_SKEW, Limits, ModelWeek, Reading, Window};

const INTERVAL: Duration = Duration::from_secs(20);
const QUERY_WAIT: Duration = Duration::from_secs(1);
const MAX_OUTPUT: usize = 64 * 1024;

/// Project only usage fields. An older history row must not restore a window
/// that is absent from the newest report, and a newer row replaces its limit id.
pub(super) const SQL: &str = r"
PRAGMA query_only=ON;
WITH cache_json AS (
    SELECT CASE WHEN json_valid(value) THEN value END AS body
    FROM cache WHERE key GLOB 'usage_cache:report:*'
), reports AS (
    SELECT json_extract(body,'$.value.provider') AS provider,
           json_extract(body,'$.value.fetchedAt') AS observed_ms,
           CASE WHEN json_type(body,'$.value.limits')='array' THEN json_extract(body,'$.value.limits') END AS limits
    FROM cache_json
), picked AS (
    SELECT *, row_number() OVER (PARTITION BY provider ORDER BY observed_ms DESC) AS rank
    FROM reports WHERE provider IN ('anthropic','openai-codex') AND typeof(observed_ms)='integer'
), history AS (
    SELECT provider, recorded_at, limit_id, used_fraction, resets_at, window_label,
           row_number() OVER (PARTITION BY provider,limit_id ORDER BY recorded_at DESC,id DESC) AS rank
    FROM usage_history WHERE provider IN ('anthropic','openai-codex')
)
SELECT r.provider, r.observed_ms, json_extract(l.value,'$.id') AS limit_id,
       json_extract(l.value,'$.amount.usedFraction') AS used_fraction,
       json_extract(l.value,'$.window.resetsAt') AS reset_ms,
       json_extract(l.value,'$.window.durationMs') AS duration_ms,
       json_extract(l.value,'$.scope.tier') AS tier, NULL AS window_label, 0 AS history_row
FROM picked r LEFT JOIN json_each(r.limits) l ON l.type='object' AND NOT EXISTS (
    SELECT 1 FROM history h WHERE h.rank=1 AND h.provider=r.provider
       AND h.limit_id=CASE l.type WHEN 'object' THEN json_extract(l.value,'$.id') END
       AND h.recorded_at>r.observed_ms
) WHERE r.rank=1
UNION ALL
SELECT h.provider, h.recorded_at, h.limit_id, h.used_fraction, h.resets_at,
       NULL, NULL, h.window_label, 1
FROM history h LEFT JOIN picked r ON r.rank=1 AND r.provider=h.provider
WHERE h.rank=1 AND (r.observed_ms IS NULL OR h.recorded_at>r.observed_ms)
ORDER BY observed_ms,limit_id;
";

/// Honour the harness's directory override without invoking the harness.
pub(super) fn database(home: Option<&Path>) -> Option<PathBuf> {
    let dir = std::env::var_os("PI_CODING_AGENT_DIR").filter(|s| !s.is_empty()).map(PathBuf::from);
    database_in(home, dir.as_deref())
}

/// SQLite receives an absolute path, even for a relative directory override.
pub(super) fn database_in(home: Option<&Path>, dir: Option<&Path>) -> Option<PathBuf> {
    let path = match dir {
        Some(dir) => dir.join("agent.db"),
        None => home?.join(".omp/agent/agent.db"),
    };
    std::path::absolute(path).ok()
}

/// OMP keeps separate account snapshots for the two providers.
pub(super) struct Reports {
    pub claude: Option<Reading>,
    pub codex: Option<Reading>,
}

/// A slow database read runs off the pane's event loop.
#[derive(Default)]
pub(super) struct Poll {
    path: Option<PathBuf>,
    last: Option<Instant>,
    job: Option<JoinHandle<Option<Reports>>>,
}

impl Poll {
    pub(super) fn new(path: Option<PathBuf>) -> Self {
        Self { path, ..Self::default() }
    }

    /// Keep checking quickly enough to collect a worker that has not finished.
    pub(super) fn pending(&self) -> bool {
        self.job.is_some()
    }

    /// At most one worker; even a busy database does not hold up a frame.
    pub(super) fn check(&mut self, now: i64, clock: Instant) -> Option<Reports> {
        if let Some(job) = &self.job {
            if !job.is_finished() {
                return None;
            }
            return self.job.take()?.join().ok().flatten();
        }
        if self.last.is_some_and(|last| clock.saturating_duration_since(last) < INTERVAL) {
            return None;
        }
        let path = self.path.clone()?;
        self.last = Some(clock);
        self.job = thread::Builder::new().spawn(move || read(&path, now)).ok();
        None
    }
}

/// Prefer the system binary over an executable found in the user's path.
pub(super) fn sqlite() -> &'static OsStr {
    let system = Path::new("/usr/bin/sqlite3");
    if system.is_file() { system.as_os_str() } else { OsStr::new("sqlite3") }
}

pub(super) fn read(path: &Path, now: i64) -> Option<Reports> {
    let bytes = query(sqlite(), path, QUERY_WAIT)?;
    parse(&bytes, now)
}

/// A read-only SQLite connection can create missing WAL sidecars on a writable directory.
/// Look at the journal versions in the database header before opening the connection.
fn readable(path: &Path) -> bool {
    let mut header = [0; 20];
    if !path.is_absolute() || !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    if fs_ops::open_read(path).and_then(|mut file| file.read_exact(&mut header)).is_err()
        || &header[..16] != b"SQLite format 3\0"
    {
        return false;
    }
    match (header[18], header[19]) {
        (1, 1) => true,
        (1 | 2, 2) | (2, 1) => ["-wal", "-shm"].iter().all(|suffix| {
            let mut sidecar = path.as_os_str().to_owned();
            sidecar.push(suffix);
            std::fs::symlink_metadata(sidecar).is_ok_and(|m| m.is_file())
        }),
        _ => false,
    }
}

/// Bound both child output and wall time so a busy database cannot keep a worker alive.
pub(super) fn query(executable: &OsStr, path: &Path, wait: Duration) -> Option<Vec<u8>> {
    if !readable(path) {
        return None;
    }
    let deadline = Instant::now() + wait;
    let mut child = Command::new(executable)
        .args(["-batch", "-readonly", "-json", "-init", "/dev/null"])
        .arg(path)
        .arg(SQL)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let captured = child.stdout.take().and_then(|pipe| files::read_pipe(pipe, MAX_OUTPUT, deadline));
    if let Some(bytes) = captured {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success().then_some(bytes),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
                _ => break,
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// Reconstruct each newest snapshot without bringing back obsolete windows.
pub(super) fn parse(bytes: &[u8], now: i64) -> Option<Reports> {
    let mut reports = Reports { claude: None, codex: None };
    // sqlite3 prints nothing for a select that returns no rows.
    if bytes.is_empty() {
        return Some(reports);
    }
    for row in Value::parse(bytes)?.elements()? {
        let [provider, at, id, fraction, reset, duration, tier, label, history] = row.fields([
            "provider", "observed_ms", "limit_id", "used_fraction", "reset_ms", "duration_ms", "tier", "window_label",
            "history_row",
        ]);
        let Some(at) = at.and_then(Timestamp::millis).filter(|at| at.seconds <= now.saturating_add(CLOCK_SKEW)) else {
            continue;
        };
        let (report, claude) = match provider.and_then(|v| v.string(20)).as_deref() {
            Some("anthropic") => (&mut reports.claude, true),
            Some("openai-codex") => (&mut reports.codex, false),
            _ => continue,
        };
        let id = id.and_then(|v| v.string(128));
        let scoped = tier.filter(|v| !v.null());
        let tier = scoped.and_then(|v| v.string(32));
        let (minutes, model, scoped) = if history.and_then(Value::integer) == Some(1) {
            let Some((minutes, model)) = history_window(claude, id.as_deref(), label) else { continue };
            (Some(minutes), model, model.is_some())
        } else {
            let minutes = match duration.and_then(Window::minutes) {
                Some(18_000_000) => Some(300),
                Some(604_800_000) => Some(10_080),
                _ => None,
            };
            (minutes, tier.as_deref(), scoped.is_some())
        };
        let reading = report.get_or_insert(Reading { at, windows: Limits::default() });
        reading.at = reading.at.max(at);
        let Some(minutes) = minutes else { continue };
        if !claude && !scoped && !matches!(id.as_deref(), Some("openai-codex:primary" | "openai-codex:secondary")) {
            continue;
        }
        let window = fraction.and_then(Value::number).filter(|n| (0.0..=1.0).contains(n)).map(|fraction| Window {
            minutes,
            percent: fraction.abs() * 100.0,
            resets: reset.and_then(Timestamp::millis).and_then(|stamp| time::cache_reset(stamp, at.seconds)),
            observed: at.seconds,
        });
        match (scoped, model) {
            (false, _) => reading.windows.base[usize::from(minutes == 10_080)] = window,
            (true, Some(tier)) if minutes == 10_080 => {
                let Some(model) = window.and_then(|window| ModelWeek::new(tier, window)) else { continue };
                if reading.windows.model_week.is_none_or(|old| {
                    model.window.percent > old.window.percent
                        || (model.window.percent == old.window.percent && model.label() < old.label())
                }) {
                    reading.windows.model_week = Some(model);
                }
            }
            _ => {}
        }
    }
    Some(reports)
}

/// History stores a display label rather than a duration or scope object.
fn history_window<'a>(claude: bool, id: Option<&'a str>, label: Option<Value<'_>>) -> Option<(u64, Option<&'a str>)> {
    let id = id?;
    if claude {
        return match id {
            "anthropic:5h" => Some((300, None)),
            "anthropic:7d" => Some((10_080, None)),
            _ => Some((10_080, Some(id.strip_prefix("anthropic:7d:")?))),
        };
    }
    if !matches!(id, "openai-codex:primary" | "openai-codex:secondary") {
        return None;
    }
    let label = label?.string(20)?.to_ascii_lowercase();
    match label.as_str() {
        "5 hour" | "5 hours" | "5h" => Some((300, None)),
        "7 day" | "7 days" | "7d" => Some((10_080, None)),
        _ => None,
    }
}
