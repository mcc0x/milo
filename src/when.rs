//! Small clock helpers for the history panel.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The local calendar day `ts` falls on, as a day count: equal for two
/// moments on the same day here, whatever the time zone.
pub fn local_day(ts: i64) -> i64 {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    (ts + tm.tm_gmtoff as i64).div_euclid(86400)
}

/// How long ago, in the largest whole unit: "now", "50m", "1h", "3d", "6w",
/// "4mo", "2y". Seventy minutes is "1h", never "70m".
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
