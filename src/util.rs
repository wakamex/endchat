//! Small helpers: UTC dates without a date crate, byte-size text cuts.

use std::time::{SystemTime, UNIX_EPOCH};

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// ISO 8601 UTC timestamp for a Unix time in seconds.
pub fn iso_from_unix(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

pub fn now_iso() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    iso_from_unix(secs as i64)
}

/// The day number `YYYYMMDD` of an ISO timestamp.
pub fn day_of(iso: &str) -> u32 {
    iso.get(..10).map_or(0, |d| d.replace('-', "").parse().unwrap_or(0))
}

/// Unix seconds of an ISO timestamp like `2026-10-08T12:30:00Z`.
pub fn unix_from_iso(iso: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| iso.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + n(11..13)? * 3600 + n(14..16)? * 60 + iso.get(17..19).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0))
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a character boundary.
pub fn cut_bytes(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_unix(1_791_500_000), "2026-10-08T22:53:20Z");
        assert_eq!(day_of("2026-10-08T22:53:20Z"), 20_261_008);
        for s in [0, 951_782_400, 1_791_500_000, 4_102_444_799] {
            assert_eq!(unix_from_iso(&iso_from_unix(s)), Some(s));
        }
    }

    #[test]
    fn cuts_on_char_boundary() {
        assert_eq!(cut_bytes("aé", 2), "a");
        assert_eq!(cut_bytes("aé", 3), "aé");
        assert_eq!(cut_bytes("abc", 10), "abc");
    }
}
