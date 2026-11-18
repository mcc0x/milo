//! Gives panes local calendar days and compact wording for ages and durations.

use std::time::{SystemTime, UNIX_EPOCH};

pub const MINUTE: i64 = 60;
pub const HOUR: i64 = 60 * MINUTE;
pub const DAY: i64 = 24 * HOUR;

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A day number for the local time zone, used to separate days in the timeline.
pub fn local_day(ts: i64) -> i64 {
    let Ok(t) = libc::time_t::try_from(i128::from(ts)) else { return ts.div_euclid(DAY) };
    // SAFETY: Zero is valid for the integer fields and null is valid for the raw zone pointer.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `t` is readable and `tm` is writable for the call.
    let local = unsafe { libc::localtime_r(&t, &mut tm) };
    // Use UTC if the local time is unavailable.
    let offset: libc::c_long = if local.is_null() { 0 } else { tm.tm_gmtoff };
    #[cfg(not(target_pointer_width = "64"))]
    let offset = i64::from(offset);
    (ts + offset).div_euclid(DAY)
}

/// Age to fit next to a commit subject. Uses the largest whole unit.
pub fn short_age(secs: i64) -> String {
    match secs {
        ..MINUTE => "now".into(),
        MINUTE..HOUR => format!("{}m", secs / MINUTE),
        HOUR..DAY => format!("{}h", secs / HOUR),
        s if s < 7 * DAY => format!("{}d", s / DAY),
        s if s < 30 * DAY => format!("{}w", s / (7 * DAY)),
        s if s < 365 * DAY => format!("{}mo", s / (30 * DAY)),
        s => format!("{}y", s / (365 * DAY)),
    }
}

/// Round only when there is no room for a second unit.
#[derive(Clone, Copy)]
pub enum Detail {
    Exact,
    Rounded,
}

/// Durations for the usage strip are spaced so they can be read beside figures.
pub fn duration(seconds: i64, detail: Detail) -> String {
    let rounded = matches!(detail, Detail::Rounded);
    match seconds {
        ..MINUTE => "<1 m".into(),
        MINUTE..HOUR => format!("{} m", seconds / MINUTE),
        HOUR..DAY => {
            let minutes = seconds % HOUR / MINUTE;
            if rounded {
                format!("{} h", (seconds + HOUR / 2) / HOUR)
            } else if minutes == 0 {
                format!("{} h", seconds / HOUR)
            } else {
                format!("{} h {minutes} m", seconds / HOUR)
            }
        }
        _ => format!("{} d", (seconds + if rounded { DAY / 2 } else { 0 }) / DAY),
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
