use std::time::{Duration, SystemTime};

/// `2026-10-02 07:32:10Z` (UTC). Formatted by hand: a civil date is a few
/// lines of arithmetic, not worth a date-time dependency.
pub fn timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// How long ago, roughly: `just now`, `42s ago`, `5m ago`, `3h ago`, `2d ago`.
pub fn ago(time: SystemTime) -> String {
    let elapsed = SystemTime::now()
        .duration_since(time)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    match elapsed {
        0..=4 => "just now".to_owned(),
        5..=59 => format!("{elapsed}s ago"),
        60..=3599 => format!("{}m ago", elapsed / 60),
        3600..=86_399 => format!("{}h ago", elapsed / 3600),
        _ => format!("{}d ago", elapsed / 86_400),
    }
}

/// Howard Hinnant's days-to-civil, for the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc() {
        assert_eq!(timestamp(SystemTime::UNIX_EPOCH), "1970-01-01 00:00:00Z");
        let leap = SystemTime::UNIX_EPOCH + Duration::from_secs(951_825_600); // 2000-02-29 12:00
        assert_eq!(timestamp(leap), "2000-02-29 12:00:00Z");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        assert_eq!(timestamp(now), "2026-09-21 14:13:20Z");
    }
}
