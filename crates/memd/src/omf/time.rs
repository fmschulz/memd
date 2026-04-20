//! Minimal UTC RFC-3339 formatter for OMF export.
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
/// Returns `None` for values whose second-precision truncation falls
/// outside the range representable by `(year, month, day, hour, min, sec)`
/// on a 32-bit year. In practice that cannot happen for any value that
/// `SystemTime::now()` would produce this millennium.
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
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z"
    ))
}

/// Format an `i64` ms-since-epoch as a `YYYY-MM-DD` date-only string.
///
/// Used for per-item `created_at`/`updated_at`/`expires_at` where OMF
/// 1.0 accepts either full RFC-3339 or a date-only form. Matches the
/// plan's reference implementation which emits date-only for these
/// fields (per-day granularity is sufficient for the lifecycle overlay).
pub fn format_date_ms(ms: i64) -> Option<String> {
    let days = ms.div_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days)?;
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Return the current UTC wall-clock time as RFC-3339.
///
/// Falls back to the Unix epoch string if `SystemTime::now()` is before
/// the epoch (practically impossible on any sane system).
pub fn now_utc_rfc3339() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    format_rfc3339_ms(ms).unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats_to_unix_midnight() {
        assert_eq!(
            format_rfc3339_ms(0).unwrap(),
            "1970-01-01T00:00:00Z"
        );
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
}
