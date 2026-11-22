//! Reads OMP's saved account limits without invoking the harness or its login.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::json::Value;

use super::sqlite::{self, Outcome, Poll};
use super::time::{self, Timestamp};
use super::{CLOCK_SKEW, Limits, ModelWeek, Reading, Window};

/// Project only usage fields. Rank eligible observations, and keep history
/// from the same account as the cached report (or one account if uncached).
pub(super) fn sql(now: i64) -> String {
    let latest = now.saturating_add(CLOCK_SKEW).saturating_mul(1000).saturating_add(999);
    format!(
        r"
PRAGMA query_only=ON;
PRAGMA trusted_schema=OFF;
WITH cache_json AS (
    SELECT key, CASE WHEN json_valid(value) THEN value END AS body
    FROM cache WHERE key GLOB 'usage_cache:report:*'
), reports AS (
    SELECT key, json_extract(body,'$.value.provider') AS provider,
           json_extract(body,'$.value.fetchedAt') AS observed_ms,
           CASE WHEN json_type(body,'$.value.limits')='array' THEN json_extract(body,'$.value.limits') END AS limits
    FROM cache_json
), picked AS (
    SELECT *, row_number() OVER (PARTITION BY provider ORDER BY observed_ms DESC,key) AS rank
    FROM reports WHERE provider IN ('anthropic','openai-codex') AND typeof(observed_ms)='integer'
      AND observed_ms BETWEEN 0 AND {latest}
), ranked_history AS (
    SELECT h.id, h.provider, h.recorded_at, h.limit_id, h.used_fraction, h.resets_at, h.window_label, h.account_key,
           row_number() OVER (
               PARTITION BY h.provider,h.account_key,h.limit_id ORDER BY h.recorded_at DESC,h.id DESC
           ) AS rank
    FROM usage_history h LEFT JOIN picked r ON r.provider=h.provider AND r.rank=1
    WHERE h.provider IN ('anthropic','openai-codex') AND typeof(h.recorded_at)='integer'
      AND h.recorded_at BETWEEN 0 AND {latest}
      AND (r.observed_ms IS NULL OR (
          h.recorded_at>r.observed_ms AND substr(r.key,-length(h.account_key)-1)=':'||h.account_key
      ))
), history_heads AS (
    -- The newest row per provider is also the newest row in its own account and limit.
    SELECT provider, recorded_at, limit_id, used_fraction, resets_at, window_label, account_key,
           first_value(account_key) OVER (PARTITION BY provider ORDER BY recorded_at DESC,id DESC) AS newest
    FROM ranked_history WHERE rank=1
), history AS (
    SELECT provider, recorded_at, limit_id, used_fraction, resets_at, window_label
    FROM history_heads WHERE account_key=newest
)
SELECT r.provider, r.observed_ms, json_extract(l.value,'$.id') AS limit_id,
       json_extract(l.value,'$.amount.usedFraction') AS used_fraction,
       json_extract(l.value,'$.window.resetsAt') AS reset_ms,
       json_extract(l.value,'$.window.durationMs') AS duration_ms,
       json_extract(l.value,'$.scope.tier') AS tier, NULL AS window_label, 0 AS history_row
FROM picked r LEFT JOIN json_each(r.limits) l ON l.type='object' AND NOT EXISTS (
    SELECT 1 FROM history h WHERE h.provider=r.provider
       AND h.limit_id=CASE l.type WHEN 'object' THEN json_extract(l.value,'$.id') END
) WHERE r.rank=1
UNION ALL
SELECT h.provider, h.recorded_at, h.limit_id, h.used_fraction, h.resets_at,
       NULL, NULL, h.window_label, 1
FROM history h
UNION ALL
SELECT 'milo:ahead', at, NULL, NULL, NULL, NULL, NULL, NULL, 2
FROM (SELECT min(at) AS at FROM (
    SELECT observed_ms AS at FROM reports WHERE typeof(observed_ms)='integer' AND observed_ms>{latest}
    UNION ALL
    SELECT recorded_at FROM usage_history WHERE typeof(recorded_at)='integer' AND recorded_at>{latest}
)) WHERE at IS NOT NULL
ORDER BY observed_ms,limit_id;
"
    )
}

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
#[derive(Default)]
pub(super) struct Reports {
    pub claude: Option<Reading>,
    pub codex: Option<Reading>,
    /// The earliest saved observation dated too far ahead of the clock to
    /// count yet, in milliseconds. The reports change when it comes due.
    pub ahead: Option<i64>,
}

impl Reports {
    /// The first second at which the same database gives other reports.
    fn stand_until(&self) -> i64 {
        self.ahead.map_or(i64::MAX, |ms| ms.div_euclid(1000).saturating_sub(CLOCK_SKEW))
    }
}

/// A missing database clears the saved reports; any other failure keeps them.
/// These are limits, so only a database without a write-ahead log is ever
/// left unread: with one, what a reader sees can change without its files changing.
pub(super) fn poll(path: Option<PathBuf>, wait: Duration) -> Poll<Reports> {
    let read: sqlite::Reader<Reports> = |path, now, wait| {
        if let Some(reports) = read(path, now, wait) {
            let stands_until = Some(reports.stand_until());
            return Some(Outcome { value: reports, stands_until });
        }
        let missing = std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == io::ErrorKind::NotFound);
        missing.then(|| Outcome { value: Reports::default(), stands_until: None })
    };
    Poll::new(path, wait, read).never_settled_with_a_log()
}

pub(super) fn read(path: &Path, now: i64, wait: Duration) -> Option<Reports> {
    let bytes = sqlite::query(sqlite::executable(), path, &sql(now), wait)?;
    parse(&bytes, now)
}

/// Reconstruct each newest snapshot without bringing back obsolete windows.
pub(super) fn parse(bytes: &[u8], now: i64) -> Option<Reports> {
    let mut reports = Reports::default();
    // sqlite3 prints nothing for a select that returns no rows.
    if bytes.is_empty() {
        return Some(reports);
    }
    for row in Value::parse(bytes)?.elements()? {
        let [provider, at, id, fraction, reset, duration, tier, label, history] = row.fields([
            "provider", "observed_ms", "limit_id", "used_fraction", "reset_ms", "duration_ms", "tier", "window_label",
            "history_row",
        ]);
        if provider.and_then(|v| v.string(20)).as_deref() == Some("milo:ahead") {
            reports.ahead = at.and_then(Value::integer);
            continue;
        }
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
