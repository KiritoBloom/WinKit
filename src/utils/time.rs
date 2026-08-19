//! Time helpers shared by providers and diagnostics.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Format a `SystemTime` as RFC3339, or `None` for times before the Unix
/// epoch.
pub fn format_rfc3339_opt(t: SystemTime) -> Option<String> {
    let d = t.duration_since(UNIX_EPOCH).ok()?;
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    let days = secs / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    let (hour, minute, second) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
    ))
}

/// Convert a Win32 `FILETIME` (100ns ticks since 1601-01-01) into an
/// RFC3339 UTC timestamp string, or `None` for an invalid value.
pub fn filetime_to_rfc3339(high: u32, low: u32) -> Option<String> {
    let ticks = ((high as u64) << 32) | low as u64;
    if ticks == 0 {
        return None;
    }
    // Windows epoch is 1601-01-01; Unix epoch is 1970-01-01.
    const WINDOWS_TO_UNIX_100NS: u64 = 11_644_473_600 * 10_000_000;
    if ticks < WINDOWS_TO_UNIX_100NS {
        return None;
    }
    let unix_ns = (ticks - WINDOWS_TO_UNIX_100NS) * 100;
    let secs = unix_ns / 1_000_000_000;
    let nanos = (unix_ns % 1_000_000_000) as u32;
    let t = UNIX_EPOCH + Duration::new(secs, nanos);
    Some(format_rfc3339(t))
}

/// Convert 100ns tick counts into milliseconds.
pub fn ticks_to_ms(high: u32, low: u32) -> u64 {
    let ticks = ((high as u64) << 32) | low as u64;
    ticks / 10_000
}

/// Format a `SystemTime` as an RFC3339 UTC string with millisecond precision.
pub fn format_rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    let days = secs / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    let (hour, minute, second) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse the RFC3339 UTC timestamps produced by [`format_rfc3339`]
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`) back into Unix epoch seconds. Returns
/// `None` for anything that is not that exact shape.
pub fn parse_rfc3339_epoch_secs(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let time = time.split('.').next().unwrap_or(time);
    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let minute: u64 = time_parts.next()?.parse().ok()?;
    let second: u64 = time_parts.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some(days as u64 * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (inverse of
/// [`civil_from_days`]).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = ((153 * mp + 2) / 5 + d - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Format an RFC3339 timestamp `minutes` ago from now.
pub fn minutes_ago_rfc3339(minutes: u64) -> String {
    let now = SystemTime::now() - Duration::from_secs(minutes * 60);
    format_rfc3339(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_epoch() {
        for secs in [0u64, 1, 86_400, 1_700_000_000] {
            let t = UNIX_EPOCH + Duration::from_secs(secs);
            let s = format_rfc3339(t);
            assert_eq!(parse_rfc3339_epoch_secs(&s), Some(secs), "input {s}");
        }
    }

    #[test]
    fn parses_known_timestamp() {
        assert_eq!(
            parse_rfc3339_epoch_secs("2026-08-13T07:59:00.000Z"),
            Some(1_786_607_940)
        );
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(parse_rfc3339_epoch_secs("not a date"), None);
        assert_eq!(parse_rfc3339_epoch_secs(""), None);
    }
}
