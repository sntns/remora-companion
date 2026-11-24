//! Dates and durations as rmra and remora-etcher read and write them.
//!
//! On jiff: humantime, which did this before, wraps chrono and is
//! unmaintained along with it (RUSTSEC-2026-0542).

use std::time::{Duration, SystemTime};

use jiff::{civil, tz::TimeZone, Span, SpanRelativeTo, SpanRound, Timestamp, Unit};

/// An RFC 3339 time, read loosely: `T` or a space between the date and the
/// time, any fraction of a second, and UTC when no offset is given.
pub fn parse_time(text: &str) -> Option<SystemTime> {
    let text = text.trim();
    let at = text.parse::<Timestamp>().ok().or_else(|| {
        let civil = text.parse::<civil::DateTime>().ok()?;
        Some(civil.to_zoned(TimeZone::UTC).ok()?.timestamp())
    })?;
    Some(at.into())
}

/// `2026-11-14T08:00:00Z`: to the second, truncated.
pub fn format_seconds(time: SystemTime) -> String {
    format!("{:.0}", timestamp(time))
}

/// `2026-11-14T08:00:00.123Z`: to the millisecond, truncated.
pub fn format_millis(time: SystemTime) -> String {
    format!("{:.3}", timestamp(time))
}

fn timestamp(time: SystemTime) -> Timestamp {
    Timestamp::try_from(time).unwrap_or(Timestamp::UNIX_EPOCH)
}

/// `24h`, `1m 30s`, `7d`, `2 weeks`: a day is 24 hours. Years and months,
/// whose length depends on when, are refused.
pub fn parse_duration(text: &str) -> Result<Duration, jiff::Error> {
    let span: Span = text.trim().parse()?;
    Duration::try_from(span.to_duration(SpanRelativeTo::days_are_24_hours())?)
}

/// `1d`, `6h`, `1h 30m`: days the largest unit.
pub fn format_duration(duration: Duration) -> String {
    let span = Span::try_from(duration).and_then(|span| {
        span.round(
            SpanRound::new()
                .largest(Unit::Day)
                .relative(SpanRelativeTo::days_are_24_hours()),
        )
    });
    match span {
        Ok(span) => format!("{span:#}"),
        Err(_) => format!("{}s", duration.as_secs()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn reads_times_loosely() {
        let expected = Some(at(1_790_000_000));
        assert_eq!(parse_time("2026-09-21T14:13:20Z"), expected);
        assert_eq!(parse_time("2026-09-21 14:13:20Z"), expected);
        assert_eq!(parse_time("2026-09-21T16:13:20+02:00"), expected);
        assert_eq!(parse_time("2026-09-21T14:13:20"), expected);
        assert_eq!(
            parse_time("2026-09-21T14:13:20.250Z"),
            Some(at(1_790_000_000) + Duration::from_millis(250))
        );
        assert_eq!(parse_time("yesterday"), None);
        assert_eq!(parse_time("42"), None);
    }

    #[test]
    fn writes_times() {
        let time = at(1_790_000_000) + Duration::from_micros(123_456);
        assert_eq!(format_seconds(time), "2026-09-21T14:13:20Z");
        assert_eq!(format_millis(time), "2026-09-21T14:13:20.123Z");
        assert_eq!(format_millis(at(1_790_000_000)), "2026-09-21T14:13:20.000Z");
        assert_eq!(
            parse_time(&format_millis(time)),
            Some(time - Duration::from_micros(456))
        );
    }

    #[test]
    fn reads_durations() {
        let hours = |h: u64| Duration::from_secs(h * 3600);
        assert_eq!(parse_duration("24h").unwrap(), hours(24));
        assert_eq!(parse_duration("7d").unwrap(), hours(7 * 24));
        assert_eq!(parse_duration("2 weeks").unwrap(), hours(14 * 24));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("1h 30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert!(parse_duration("1 month").is_err());
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("-1h").is_err());
    }

    #[test]
    fn writes_durations() {
        assert_eq!(format_duration(Duration::from_secs(86_400)), "1d");
        assert_eq!(format_duration(Duration::from_secs(6 * 3600)), "6h");
        assert_eq!(format_duration(Duration::from_secs(5400)), "1h 30m");
        assert_eq!(format_duration(Duration::from_secs(7 * 86_400)), "7d");
    }
}
