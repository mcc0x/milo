//! Calendar labels for the timeline, in the local time zone.

use std::time::{SystemTime, UNIX_EPOCH};

const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The local calendar day `ts` falls on, as a day count, with its broken-down time.
fn local(ts: i64) -> (i64, libc::tm) {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    ((ts + tm.tm_gmtoff as i64).div_euclid(86400), tm)
}

/// "Today", "Yesterday", a weekday within the last week, otherwise a date.
pub fn day_label(ts: i64, now: i64) -> String {
    let (day, tm) = local(ts);
    let (today, now_tm) = local(now);
    let month = MONTHS[tm.tm_mon.clamp(0, 11) as usize];
    match today - day {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => WEEKDAYS[tm.tm_wday.clamp(0, 6) as usize].into(),
        _ if tm.tm_year == now_tm.tm_year => format!("{} {month}", tm.tm_mday),
        _ => format!("{} {month} {}", tm.tm_mday, tm.tm_year + 1900),
    }
}
