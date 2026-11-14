use std::time::{Duration, SystemTime};

use super::Document;

/// The document a device's presence is read from: what remora-accessd
/// posts at a fixed interval while it reaches the platform.
pub const HEARTBEAT: &str = "heartbeat";

/// The interval of a heartbeat that doesn't say its own.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(300);

/// How late a heartbeat may be before it counts as missed, in intervals:
/// one and a half leaves room for a slow link, and still sees one beat
/// missing.
const TOLERANCE: f64 = 1.5;

/// A device's presence over a window, from its heartbeats.
#[derive(Debug, Clone, PartialEq)]
pub struct Presence {
    /// The interval the device says it beats at (its latest word).
    pub interval: Duration,
    /// When each heartbeat was received, oldest first.
    pub beats: Vec<SystemTime>,
    /// When the device was silent: `(since, until)`, oldest first. The
    /// first may start at the window's start and the last end at its end
    /// (still silent).
    pub gaps: Vec<(SystemTime, SystemTime)>,
    /// The share of the window it was present, 0 to 100.
    pub availability_percent: f64,
}

/// A heartbeat's interval, as the device says it (`interval`, seconds).
pub fn heartbeat_interval(document: &Document) -> Duration {
    document.body["interval"]
        .as_u64()
        .filter(|seconds| *seconds > 0)
        .map_or(DEFAULT_INTERVAL, Duration::from_secs)
}

/// Whether a device whose last heartbeat is `latest` is silent at `now`.
pub fn is_silent(latest: &Document, now: SystemTime) -> bool {
    now.duration_since(latest.captured_at)
        .is_ok_and(|age| age > heartbeat_interval(latest).mul_f64(TOLERANCE))
}

/// Reads a device's presence between `from` and `to` off its heartbeats
/// (oldest first; `None` without any). A heartbeat vouches for the
/// interval before it: the device is silent wherever the next one is
/// later than that, with some tolerance.
pub fn presence(heartbeats: &[Document], from: SystemTime, to: SystemTime) -> Option<Presence> {
    let interval = heartbeat_interval(heartbeats.last()?);
    let late = interval.mul_f64(TOLERANCE);
    let beats: Vec<SystemTime> = heartbeats.iter().map(|beat| beat.captured_at).collect();

    let mut gaps = Vec::new();
    // What the previous heartbeat (or the window's start) vouches up to.
    let mut covered = from;
    for beat in &beats {
        let vouched_from = beat.checked_sub(late).unwrap_or(*beat);
        if vouched_from > covered {
            gaps.push((covered, beat.checked_sub(interval).unwrap_or(*beat)));
        }
        covered = covered.max(*beat);
    }
    if to.duration_since(covered).is_ok_and(|silent| silent > late) {
        gaps.push((covered, to));
    }

    let window = to.duration_since(from).unwrap_or_default().as_secs_f64();
    let silent: f64 = gaps
        .iter()
        .filter_map(|(since, until)| Some(until.duration_since(*since).ok()?.as_secs_f64()))
        .sum();
    let availability_percent = if window > 0.0 {
        ((window - silent) / window * 100.0).clamp(0.0, 100.0)
    } else {
        100.0
    };
    Some(Presence {
        interval,
        beats,
        gaps,
        availability_percent,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn beats(times: &[u64]) -> Vec<Document> {
        times
            .iter()
            .map(|seconds| Document {
                name: HEARTBEAT.into(),
                schema: "remora-device-heartbeat".into(),
                captured_at: at(*seconds),
                boot_id: None,
                seq: None,
                body: json!({"interval": 300}),
            })
            .collect()
    }

    #[test]
    fn a_device_that_beats_on_time_is_present_throughout() {
        // A little late each time, and the window starts between two.
        let presence = presence(&beats(&[200, 505, 812, 1100]), at(0), at(1200)).unwrap();
        assert_eq!(presence.interval, Duration::from_secs(300));
        assert_eq!(presence.beats.len(), 4);
        assert!(presence.gaps.is_empty());
        assert_eq!(presence.availability_percent, 100.0);
    }

    #[test]
    fn a_silence_runs_from_the_last_beat_to_an_interval_before_the_next() {
        let presence = presence(&beats(&[300, 600, 2400, 2700]), at(0), at(3000)).unwrap();
        // 600 vouched; 2400 vouches from 2100.
        assert_eq!(presence.gaps, [(at(600), at(2100))]);
        assert_eq!(presence.availability_percent, 50.0);
    }

    #[test]
    fn the_windows_ends_count() {
        let presence = presence(&beats(&[1500, 1800]), at(0), at(3000)).unwrap();
        assert_eq!(presence.gaps, [(at(0), at(1200)), (at(1800), at(3000))]);
        assert_eq!(presence.availability_percent, 20.0);
    }

    #[test]
    fn no_heartbeat_says_nothing() {
        assert_eq!(presence(&[], at(0), at(3000)), None);
    }

    #[test]
    fn a_device_is_silent_once_a_beat_is_overdue() {
        let latest = &beats(&[1000])[0];
        assert!(!is_silent(latest, at(1400)));
        assert!(is_silent(latest, at(1500)));
        // A heartbeat that doesn't say its interval: five minutes.
        let mut quiet = latest.clone();
        quiet.body = json!({});
        assert_eq!(heartbeat_interval(&quiet), Duration::from_secs(300));
    }
}
