//! Reads provider timestamps without rounding the order of snapshots.

use crate::json::Value;
use crate::when::{DAY, HOUR, MINUTE};

/// Subseconds break ties between reports that arrived in the same second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Timestamp {
    pub seconds: i64,
    nanos: u32,
}

impl Timestamp {
    pub(super) const fn seconds(seconds: i64) -> Self {
        Self { seconds, nanos: 0 }
    }

    /// Keep the millisecond order even when two provider updates share a second.
    pub(super) fn millis(value: Value<'_>) -> Option<Self> {
        let ms = value.integer()?;
        let nanos = u32::try_from(ms % 1000).ok()? * 1_000_000;
        (ms >= 0).then_some(Self { seconds: ms / 1000, nanos })
    }

    /// Transcripts order readings by their full timestamp, including fractional seconds.
    pub(crate) fn iso(value: Value<'_>) -> Option<Self> {
        let string = value.string(96)?;
        let b = string.as_bytes();
        let [_, _, _, _, b'-', _, _, b'-', _, _, b'T' | b't', _, _, b':', _, _, b':', _, _, ..] = b else {
            return None;
        };
        let (year, month, day) = (digits(&b[..4])?, digits(&b[5..7])?, digits(&b[8..10])?);
        let (hour, min, sec) = (digits(&b[11..13])?, digits(&b[14..16])?, digits(&b[17..19])?);
        if hour > 23 || min > 59 || sec > 59 {
            return None;
        }
        let (fraction, zone) = if let Some(rest) = b[19..].strip_prefix(b".") {
            let n = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            if n == 0 {
                return None;
            }
            (&rest[..n.min(9)], &rest[n..])
        } else {
            (&b[19..19], &b[19..])
        };
        let nanos = digits(fraction)? * 10_u32.pow(9 - u32::try_from(fraction.len()).ok()?);
        let seconds =
            calendar_days(year, month, day)? * DAY + i64::from(hour) * HOUR + i64::from(min) * MINUTE + i64::from(sec)
                - offset(zone)?;
        Some(Self { seconds, nanos })
    }
}

/// Disk caches specify their reset units; keep the floor rather than guessing units or rounding up.
pub(super) fn cache_reset(stamp: Timestamp, observed: i64) -> Option<i64> {
    (0..=observed.saturating_add(366 * DAY)).contains(&stamp.seconds).then_some(stamp.seconds)
}

/// Reports use more than one wire format; reject implausibly distant resets.
pub(super) fn reset(value: Value<'_>, observed: i64) -> Option<i64> {
    const YEAR: i64 = 366 * DAY;
    let latest = observed.saturating_add(YEAR);
    if let Some(stamp) = Timestamp::iso(value) {
        let seconds = stamp.seconds.checked_add(i64::from(stamp.nanos > 0))?;
        return (0..=latest).contains(&seconds).then_some(seconds);
    }
    let stamp = value.integer().or_else(|| value.string(24)?.parse().ok())?;
    if stamp <= latest {
        return (stamp >= 0).then_some(stamp);
    }
    // A near-future reset sent in milliseconds is about a thousand times too large.
    let seconds = stamp / 1000 + i64::from(stamp % 1000 > 0);
    (observed.saturating_sub(YEAR).max(0)..=latest).contains(&seconds).then_some(seconds)
}

fn digits(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |n, &b| b.is_ascii_digit().then(|| n * 10 + u32::from(b - b'0')))
}

/// HTTP recipients accept IMF-fixdate and the two obsolete HTTP-date formats (RFC 9110 §5.6.7).
pub(super) fn http_date(value: &str, now: i64) -> Option<i64> {
    const SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const LONG: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (weekday, tail) = value.split_once(' ')?;
    let (weekday, day, month, year, clock, short_year) = if let Some(weekday) = weekday.strip_suffix(',') {
        let tail = tail.strip_suffix(" GMT")?;
        if let Some(index) = SHORT.iter().position(|&w| w == weekday) {
            let (day, tail) = tail.split_once(' ')?;
            let (month, tail) = tail.split_once(' ')?;
            let (year, clock) = tail.split_once(' ')?;
            (index, day, month, year, clock, false)
        } else {
            let index = LONG.iter().position(|&w| w == weekday)?;
            let (date, clock) = tail.split_once(' ')?;
            let (day, date) = date.split_once('-')?;
            let (month, year) = date.split_once('-')?;
            (index, day, month, year, clock, true)
        }
    } else {
        let index = SHORT.iter().position(|&w| w == weekday)?;
        let (month, tail) = tail.split_once(' ')?;
        let (day, tail) = tail.split_at_checked(2)?;
        let (clock, year) = tail.strip_prefix(' ')?.split_once(' ')?;
        (index, day.trim_start_matches(' '), month, year, clock, false)
    };
    let month = u32::try_from(MONTHS.iter().position(|&m| m == month)? + 1).ok()?;
    if year.len() != if short_year { 2 } else { 4 } || day.is_empty() || day.len() > 2 {
        return None;
    }
    let day = digits(day.as_bytes())?;
    let [h, h1, b':', m, m1, b':', s, s1] = *clock.as_bytes() else { return None };
    let (h, m, s) = (digits(&[h, h1])?, digits(&[m, m1])?, digits(&[s, s1])?);
    if h >= 24 || m >= 60 || s > 60 {
        return None;
    }
    let time = i64::from(h * 3600 + m * 60 + s);
    let mut year = digits(year.as_bytes())?;
    if short_year {
        year = (19..=99).rev().map(|century| century * 100 + year).find(|&candidate| {
            calendar_days(candidate - 50, month, 1)
                .is_some_and(|first| (first + i64::from(day) - 1) * DAY + time <= now)
        })?;
    }
    let days = calendar_days(year, month, day)?;
    ((days + 4).rem_euclid(7) == i64::try_from(weekday).ok()?).then_some(days * DAY + time)
}

fn offset(zone: &[u8]) -> Option<i64> {
    match zone {
        [b'Z' | b'z'] => Some(0),
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let (h, m) = (digits(&[*h1, *h2])?, digits(&[*m1, *m2])?);
            let sign = if *sign == b'+' { 1 } else { -1 };
            (h < 24 && m < 60).then(|| sign * i64::from(h * 3600 + m * 60))
        }
        _ => None,
    }
}

/// Validate the day before converting so impossible dates do not become later months.
fn calendar_days(year: u32, month: u32, day: u32) -> Option<i64> {
    let leap = |y: u32| y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let days = [31, u32::from(leap(year)) + 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let month = usize::try_from(month.checked_sub(1)?).ok()?;
    if year == 0 || day == 0 || day > *days.get(month)? {
        return None;
    }
    let previous = year - 1;
    let before_year = 365 * previous + previous / 4 - previous / 100 + previous / 400;
    let before_month: u32 = days[..month].iter().sum();
    Some(i64::from(before_year + before_month + day - 1) - 719_162)
}
