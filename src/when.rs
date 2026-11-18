//! Gives the history panel local calendar days and short commit ages.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A day number for the local time zone, used to separate days in the timeline.
pub fn local_day(ts: i64) -> i64 {
    let Ok(t) = libc::time_t::try_from(i128::from(ts)) else { return ts.div_euclid(86400) };
    // SAFETY: Zero is valid for the integer fields and null is valid for the raw zone pointer.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `t` is readable and `tm` is writable for the call.
    let local = unsafe { libc::localtime_r(&t, &mut tm) };
    // Use UTC if the local time is unavailable.
    let offset: libc::c_long = if local.is_null() { 0 } else { tm.tm_gmtoff };
    #[cfg(not(target_pointer_width = "64"))]
    let offset = i64::from(offset);
    (ts + offset).div_euclid(86400)
}

/// Age to fit next to a commit subject. Uses the largest whole unit.
pub fn short_age(secs: i64) -> String {
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    match secs {
        ..MIN => "now".into(),
        MIN..HOUR => format!("{}m", secs / MIN),
        HOUR..DAY => format!("{}h", secs / HOUR),
        s if s < 7 * DAY => format!("{}d", s / DAY),
        s if s < 30 * DAY => format!("{}w", s / (7 * DAY)),
        s if s < 365 * DAY => format!("{}mo", s / (30 * DAY)),
        s => format!("{}y", s / (365 * DAY)),
    }
}

#[cfg(test)]
mod tests {
    use super::short_age;

    #[test]
    fn ages_use_the_largest_whole_unit() {
        let ages = [0, 59, 60, 70 * 60, 3 * 3600, 26 * 3600, 8 * 86400, 40 * 86400, 800 * 86400];
        let short: Vec<String> = ages.into_iter().map(short_age).collect();
        assert_eq!(short, ["now", "now", "1m", "1h", "3h", "1d", "1w", "1mo", "2y"]);
    }

    #[test]
    fn a_commit_dated_in_the_future_is_now() {
        assert_eq!(short_age(-500), "now");
    }
}
