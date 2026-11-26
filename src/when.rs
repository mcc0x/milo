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
    (ts + local_offset(ts)).div_euclid(DAY)
}

#[cfg(unix)]
fn local_offset(ts: i64) -> i64 {
    let Ok(t) = libc::time_t::try_from(i128::from(ts)) else { return 0 };
    // SAFETY: Zero is valid for the integer fields and null is valid for the raw zone pointer.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `t` is readable and `tm` is writable for the call.
    let local = unsafe { libc::localtime_r(&t, &mut tm) };
    // Use UTC if the local time is unavailable.
    let offset: libc::c_long = if local.is_null() { 0 } else { tm.tm_gmtoff };
    #[cfg(not(target_pointer_width = "64"))]
    let offset = i64::from(offset);
    offset
}

#[cfg(windows)]
mod win {
    #[repr(C)]
    pub struct SystemTime {
        fields: [u16; 8],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn FileTimeToSystemTime(file_time: *const u64, system_time: *mut SystemTime) -> i32;
        pub fn SystemTimeToFileTime(system_time: *const SystemTime, file_time: *mut u64) -> i32;
        pub fn SystemTimeToTzSpecificLocalTime(zone: *const u8, utc: *const SystemTime, local: *mut SystemTime) -> i32;
    }

    pub const fn zero() -> SystemTime {
        SystemTime { fields: [0; 8] }
    }
}

/// Seconds the local zone is ahead of UTC at `ts`, or zero when Windows cannot say.
#[cfg(windows)]
fn local_offset(ts: i64) -> i64 {
    const UNIX_TO_FILETIME: i64 = 11_644_473_600;
    const TICKS_PER_SECOND: i64 = 10_000_000;
    let Some(ticks) = ts.checked_add(UNIX_TO_FILETIME).and_then(|secs| secs.checked_mul(TICKS_PER_SECOND)) else {
        return 0;
    };
    let Ok(ticks) = u64::try_from(ticks) else { return 0 };
    let (mut utc, mut local) = (win::zero(), win::zero());
    let (mut utc_ticks, mut local_ticks) = (0u64, 0u64);
    // SAFETY: `ticks` is readable and `utc` is writable for the call.
    if unsafe { win::FileTimeToSystemTime(&ticks, &mut utc) } == 0 {
        return 0;
    }
    // SAFETY: A null zone means the current one, and `utc` and `local` are valid for the call.
    if unsafe { win::SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return 0;
    }
    // SAFETY: `utc` is readable and `utc_ticks` is writable for the call.
    if unsafe { win::SystemTimeToFileTime(&utc, &mut utc_ticks) } == 0 {
        return 0;
    }
    // SAFETY: `local` is readable and `local_ticks` is writable for the call.
    if unsafe { win::SystemTimeToFileTime(&local, &mut local_ticks) } == 0 {
        return 0;
    }
    let (Ok(local_ticks), Ok(utc_ticks)) = (i64::try_from(local_ticks), i64::try_from(utc_ticks)) else { return 0 };
    (local_ticks - utc_ticks) / TICKS_PER_SECOND
}

/// Age to fit next to a commit subject. Uses the largest whole unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Age {
    pub count: Option<u64>,
    pub suffix: &'static str,
}

impl Age {
    pub fn width(self) -> usize {
        self.suffix.len() + self.count.map_or(0, |count| count.ilog10() as usize + 1)
    }
}

pub fn short_age(secs: i64) -> Age {
    let (unit, suffix) = match secs {
        ..MINUTE => return Age { count: None, suffix: "now" },
        MINUTE..HOUR => (MINUTE, "m"),
        HOUR..DAY => (HOUR, "h"),
        s if s < 7 * DAY => (DAY, "d"),
        s if s < 30 * DAY => (7 * DAY, "w"),
        s if s < 365 * DAY => (30 * DAY, "mo"),
        _ => (365 * DAY, "y"),
    };
    Age { count: Some(u64::try_from(secs / unit).expect("past age is positive")), suffix }
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
        _ => {
            let days = seconds / DAY + i64::from(rounded && seconds % DAY >= DAY / 2);
            format!("{days} d")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Age, Detail, duration, short_age};

    #[test]
    fn ages_use_the_largest_whole_unit() {
        let ages = [0, 59, 60, 70 * 60, 3 * 3600, 26 * 3600, 8 * 86400, 40 * 86400, 800 * 86400];
        let short: Vec<String> = ages
            .into_iter()
            .map(|age| {
                let Age { count, suffix } = short_age(age);
                count.map_or_else(|| suffix.to_string(), |n| format!("{n}{suffix}"))
            })
            .collect();
        assert_eq!(short, ["now", "now", "1m", "1h", "3h", "1d", "1w", "1mo", "2y"]);
    }

    #[test]
    fn a_commit_dated_in_the_future_is_now() {
        assert_eq!(short_age(-500), Age { count: None, suffix: "now" });
    }

    #[test]
    fn rounded_duration_formats_a_saturated_age_without_overflow() {
        assert_eq!(duration(i64::MAX, Detail::Rounded), "106751991167301 d");
    }
}
