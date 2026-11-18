//! Reads provider timestamps without rounding the order of snapshots.

use super::json::Value;

pub(super) const MINUTE: i64 = 60;
pub(super) const HOUR: i64 = 60 * MINUTE;
pub(super) const DAY: i64 = 24 * HOUR;
pub(super) const WEEK: i64 = 7 * DAY;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Stamp {
    pub seconds: i64,
    nanos: u32,
}

impl Stamp {
    pub(super) fn at(seconds: i64) -> Self {
        Self { seconds, nanos: 0 }
    }

    pub(super) fn iso(value: Value<'_>) -> Option<Self> {
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
        let seconds = calendar_days(year, month, day)? * DAY + i64::from(hour * 3600 + min * 60 + sec) - offset(zone)?;
        Some(Self { seconds, nanos })
    }
}

pub(super) fn reset(value: Value<'_>) -> Option<i64> {
    value.integer().filter(|ts| *ts >= 0).or_else(|| {
        if let Some(stamp) = Stamp::iso(value) {
            return stamp.seconds.checked_add(i64::from(stamp.nanos > 0));
        }
        // Some callers pass the numeric timestamp through as a string.
        value.string(24)?.parse::<i64>().ok().filter(|ts| *ts >= 0)
    })
}

fn digits(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |n, &b| b.is_ascii_digit().then(|| n * 10 + u32::from(b - b'0')))
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

pub(super) fn relative(seconds: i64) -> String {
    match seconds {
        ..MINUTE => "<1 m".into(),
        MINUTE..HOUR => format!("{} m", seconds / MINUTE),
        HOUR..DAY => {
            let minutes = seconds % HOUR / MINUTE;
            if minutes == 0 { format!("{} h", seconds / HOUR) } else { format!("{} h {minutes} m", seconds / HOUR) }
        }
        _ => format!("{} d", seconds / DAY),
    }
}
