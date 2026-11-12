//! UTC RFC-3339 formatting and ISO datetime parsing.
//!
//! OMF 1.0's `exported_at` and per-item `created_at`/`updated_at`/`expires_at`
//! are RFC-3339 UTC strings. The rest of memd uses `i64` ms-since-epoch,
//! and the crate has no `chrono`/`time` dependency, so this helper converts
//! ms to an RFC-3339 string using Howard Hinnant's `civil_from_days`
//! algorithm (stdlib-only, proven correct for all Gregorian dates).
//!
//! Format produced: `YYYY-MM-DDTHH:MM:SSZ` (seconds precision, UTC Zulu).

/// Format an `i64` ms-since-Unix-epoch as RFC-3339 UTC.
///
/// Returns `None` for values that fall outside RFC-3339's 4-digit
/// Gregorian year range (`0001`–`9999`). A 13-digit millisecond value
/// covers years 1970–2038, so this bound is defensive rather than
/// restrictive for any real timestamp.
pub fn format_rfc3339_ms(ms: i64) -> Option<String> {
    // Break ms into (seconds_since_epoch, submillis discarded).
    // Rust's i64 div/rem rounds toward zero, which gives the wrong
    // answer for negative ms (pre-1970). Use Euclidean division so
    // (seconds * 1000 + rem) == ms with 0 <= rem < 1000 always holds.
    let seconds = ms.div_euclid(1000);

    // Split seconds into days + time-of-day.
    let days = seconds.div_euclid(86_400);
    let tod = seconds.rem_euclid(86_400) as u32;
    let hour = tod / 3600;
    let min = (tod % 3600) / 60;
    let sec = tod % 60;

    let (year, month, day) = civil_from_days(days)?;
    if !is_rfc3339_year(year) {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z"
    ))
}

/// Format an `i64` ms-since-epoch as a `YYYY-MM-DD` date-only string.
///
/// Returns `None` for values that fall outside RFC-3339's 4-digit
/// Gregorian year range (`0001`–`9999`), matching `format_rfc3339_ms`.
pub fn format_date_ms(ms: i64) -> Option<String> {
    let days = ms.div_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days)?;
    if !is_rfc3339_year(year) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Gregorian years RFC-3339 can express with a 4-digit field.
///
/// Pure helper so both formatters agree on the bound and tests can
/// target it directly.
fn is_rfc3339_year(year: i32) -> bool {
    (1..=9999).contains(&year)
}

/// Current wall-clock time in ms-since-Unix-epoch.
///
/// Shares the `SystemTime`/`duration_since` call site with
/// `now_utc_rfc3339` so callers that need both the raw ms (for
/// lazy-expiry comparisons) and the formatted string don't read the
/// clock twice.
pub fn now_utc_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Return the current UTC wall-clock time as RFC-3339.
///
/// Falls back to the Unix epoch string if `SystemTime::now()` is before
/// the epoch (practically impossible on any sane system).
pub fn now_utc_rfc3339() -> String {
    format_rfc3339_ms(now_utc_ms()).unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// Howard Hinnant's `civil_from_days` (proleptic Gregorian).
///
/// Given days since 1970-01-01 (negative for earlier), return
/// `(year, month_1_indexed, day_1_indexed)`. Returns `None` only if
/// the 64-bit year overflows an `i32`, which cannot happen for any
/// timestamp this millennium.
///
/// Reference: Howard E. Hinnant, "chrono-compatible Low-Level Date
/// Algorithms", <https://howardhinnant.github.io/date_algorithms.html>.
fn civil_from_days(days: i64) -> Option<(i32, u32, u32)> {
    let z = days + 719_468; // shift origin to 0000-03-01
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64; // 0..=146096
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // 0..=399
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // 0..=365
    let mp = (5 * doy + 2) / 153; // 0..=11, month starting from March
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = y + i64::from(m <= 2);
    let year = i32::try_from(y).ok()?;
    Some((year, m, d))
}

/// Parse an ISO 8601 datetime string to Unix milliseconds.
///
/// Supports formats:
/// - "2024-01-15T10:30:45Z" (UTC)
/// - "2024-01-15T10:30:45.123Z" (with millis)
/// - "2024-01-15T10:30:45+00:00" (with offset)
pub fn parse_iso_datetime(s: &str) -> crate::error::Result<i64> {
    // Try to parse RFC3339 format manually
    parse_rfc3339(s)
        .ok_or_else(|| crate::error::MemdError::ValidationError(format!("Invalid datetime: {}", s)))
}

/// Parse RFC3339 datetime string to Unix milliseconds.
fn parse_rfc3339(s: &str) -> Option<i64> {
    // Expected format: YYYY-MM-DDTHH:MM:SS[.mmm](Z|+HH:MM|-HH:MM)
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }

    // Parse date part
    let year: i32 = s.get(0..4)?.parse().ok()?;
    if s.get(4..5)? != "-" {
        return None;
    }
    let month: u32 = s.get(5..7)?.parse().ok()?;
    if s.get(7..8)? != "-" {
        return None;
    }
    let day: u32 = s.get(8..10)?.parse().ok()?;
    if s.get(10..11)? != "T" {
        return None;
    }

    // Parse time part
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    if s.get(13..14)? != ":" {
        return None;
    }
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    if s.get(16..17)? != ":" {
        return None;
    }
    let second: u32 = s.get(17..19)?.parse().ok()?;

    // Parse optional milliseconds and timezone
    let rest = &s[19..];
    let (millis, tz_offset_mins) = parse_millis_and_tz(rest)?;

    // Convert to Unix timestamp
    let days = ymd_to_days(year, month, day)?;
    let secs = days * 86400 + (hour as i64) * 3600 + (minute as i64) * 60 + (second as i64)
        - (tz_offset_mins as i64) * 60;
    let ms = secs * 1000 + (millis as i64);

    Some(ms)
}

/// Parse milliseconds and timezone offset from the remainder of an RFC3339 string.
fn parse_millis_and_tz(s: &str) -> Option<(u32, i32)> {
    let s = s.trim();
    if s.is_empty() {
        return Some((0, 0));
    }

    let (millis, rest) = if let Some(fractional) = s.strip_prefix('.') {
        // Parse milliseconds
        let end = fractional
            .find(|c: char| !c.is_ascii_digit())
            .map(|i| i + 1)
            .unwrap_or(s.len());
        let frac = &s[1..end];
        let millis: u32 = if frac.len() >= 3 {
            frac[..3].parse().ok()?
        } else {
            let padded = format!("{:0<3}", frac);
            padded.parse().ok()?
        };
        (millis, &s[end..])
    } else {
        (0, s)
    };

    let tz_offset = if rest == "Z" || rest.is_empty() {
        0
    } else if rest.starts_with('+') || rest.starts_with('-') {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let tz = &rest[1..];
        let (hours, minutes) = if tz.contains(':') {
            let parts: Vec<&str> = tz.split(':').collect();
            if parts.len() != 2 {
                return None;
            }
            (parts[0].parse::<i32>().ok()?, parts[1].parse::<i32>().ok()?)
        } else if tz.len() == 4 && tz.is_ascii() {
            (tz[..2].parse::<i32>().ok()?, tz[2..].parse::<i32>().ok()?)
        } else {
            return None;
        };
        sign * (hours * 60 + minutes)
    } else {
        return None;
    };

    Some((millis, tz_offset))
}

/// Convert year, month, day to days since Unix epoch.
fn ymd_to_days(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Howard Hinnant's algorithm
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 12 } else { month };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (m - 3) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era as i64) * 146097 + (doe as i64) - 719468;
    Some(days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_iso_datetime() {
        let dt = "2024-01-01T00:00:00Z";
        let ms = parse_iso_datetime(dt).unwrap();
        assert_eq!(ms, 1704067200000);
    }

    #[test]
    fn test_parse_iso_datetime_with_offset() {
        let dt = "2024-01-01T01:00:00+01:00";
        let ms = parse_iso_datetime(dt).unwrap();
        assert_eq!(ms, 1704067200000); // Same as midnight UTC
    }

    #[test]
    fn epoch_formats_to_unix_midnight() {
        assert_eq!(format_rfc3339_ms(0).unwrap(), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn known_timestamp_matches_expected() {
        // 2026-04-18T00:00:00Z — Unix seconds 1_776_470_400.
        let ms: i64 = 1_776_470_400_000;
        assert_eq!(format_rfc3339_ms(ms).unwrap(), "2026-04-18T00:00:00Z");
    }

    #[test]
    fn sub_second_ms_truncates_to_seconds() {
        // Same second, different ms → same output (seconds precision).
        let a: i64 = 1_776_470_400_123;
        let b: i64 = 1_776_470_400_987;
        assert_eq!(format_rfc3339_ms(a), format_rfc3339_ms(b));
    }

    #[test]
    fn leap_day_2024_02_29() {
        // 2024-02-29T12:34:56Z
        let ms = 1_709_210_096_000;
        assert_eq!(format_rfc3339_ms(ms).unwrap(), "2024-02-29T12:34:56Z");
    }

    #[test]
    fn pre_epoch_formats_correctly() {
        // 1969-12-31T23:59:59Z (1s before epoch, negative ms).
        let ms: i64 = -1_000;
        assert_eq!(format_rfc3339_ms(ms).unwrap(), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn format_date_ms_returns_date_only() {
        let ms: i64 = 1_776_470_400_000; // 2026-04-18T00:00:00Z
        assert_eq!(format_date_ms(ms).unwrap(), "2026-04-18");
    }

    #[test]
    fn now_utc_rfc3339_is_well_formed() {
        let s = now_utc_rfc3339();
        assert_eq!(s.len(), 20, "expected YYYY-MM-DDTHH:MM:SSZ: {s}");
        assert!(s.ends_with('Z'));
        assert!(s.chars().nth(4) == Some('-'));
        assert!(s.chars().nth(10) == Some('T'));
    }

    #[test]
    fn century_boundary_2000_03_01() {
        // 2000-03-01T00:00:00Z (leap-century edge case for Hinnant's algo).
        let ms = 951_868_800_000;
        assert_eq!(format_rfc3339_ms(ms).unwrap(), "2000-03-01T00:00:00Z");
    }

    #[test]
    fn non_leap_century_2100_03_01() {
        // 2100-03-01T00:00:00Z (2100 is NOT a leap year — Gregorian rule).
        // Unix seconds for this date: 4_107_542_400.
        let ms: i64 = 4_107_542_400_000;
        assert_eq!(format_rfc3339_ms(ms).unwrap(), "2100-03-01T00:00:00Z");
    }

    #[test]
    fn year_after_9999_returns_none() {
        // 253_402_300_800_000 ms = year 10000 (1 second past 9999-12-31T23:59:59Z).
        let year_10000: i64 = 253_402_300_800_000;
        assert!(format_rfc3339_ms(year_10000).is_none());
        assert!(format_date_ms(year_10000).is_none());

        // Last representable instant in 9999 still formats.
        let last_9999: i64 = 253_402_300_799_000;
        assert_eq!(
            format_rfc3339_ms(last_9999).unwrap(),
            "9999-12-31T23:59:59Z"
        );
    }

    #[test]
    fn year_before_0001_returns_none() {
        // civil_from_days handles negative days, but RFC-3339's 4-digit
        // year bars year 0 and earlier. i64::MIN / 86_400_000 lands well
        // before year 1.
        let very_early: i64 = -62_167_219_200_001;
        assert!(format_rfc3339_ms(very_early).is_none());
    }

    #[test]
    fn now_utc_ms_and_rfc3339_agree_to_the_second() {
        // Regression guard: now_utc_rfc3339() now shares now_utc_ms()'s
        // clock read. The two values sampled in quick succession must
        // describe the same or adjacent wall-clock second.
        let ms = now_utc_ms();
        let s = now_utc_rfc3339();
        assert_eq!(s.len(), 20);
        // Convert back to a loose range: within 2 seconds.
        let from_s = format_rfc3339_ms(ms).unwrap();
        assert_eq!(s, from_s, "both calls must render the same second");
    }
}
