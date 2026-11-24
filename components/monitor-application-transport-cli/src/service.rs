use std::time::{Duration, SystemTime};

use error_stack::Report;
use remora_context::model::ContextOverride;
use remora_monitor::{
    application::MonitorService,
    model::{
        heartbeat_interval, is_silent, presence, system_metrics, Document, Unit, DEFAULT_BUCKET,
        HEARTBEAT, SYSTEM,
    },
};
use remora_tui as tui;
use serde_json::{json, Value};

use crate::{
    chart,
    error::{monitor_error, Error, Result},
    watch,
};

/// A chart's plot's height, in terminal rows.
const CHART_HEIGHT: u16 = 6;
/// A chart's width, in columns: within these, the terminal's.
const CHART_WIDTH: std::ops::RangeInclusive<u16> = 40..=120;
/// How long a document's summary may be, in a table cell.
const SUMMARY_LENGTH: usize = 100;

#[derive(clap::Args)]
pub struct Target {
    /// The device, by name.
    #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
    pub(crate) device: String,
    /// The time series bucket the account stores what its devices send in.
    #[arg(
        long,
        env = "RMRA_MONITOR_BUCKET",
        value_name = "NAME",
        default_value = DEFAULT_BUCKET
    )]
    pub(crate) bucket: String,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// What a device last reported of itself: one line per document
    /// (heartbeat, system, network, health...), with when.
    Status {
        #[command(flatten)]
        target: Target,
        /// `table` for humans, `json` for scripts (the documents in full).
        #[arg(long, default_value = "table")]
        format: StatusFormat,
    },

    /// One monitoring document in full, as JSON: as last reported, or,
    /// with a period (`--since`, or `--from` and `--to`), every one
    /// reported in it, oldest first, as an array.
    Show {
        #[command(flatten)]
        target: Target,
        /// The document: `heartbeat`, `system`, `network`... (`rmra monitor
        /// status` lists a device's).
        document: String,
        /// Those reported this far back: `30m`, `24h`, `7d`.
        #[arg(long, value_parser = remora_time::parse_duration, conflicts_with = "from")]
        since: Option<Duration>,
        /// Those reported from then on: `2026-11-14T08:00:00Z`.
        #[arg(long, value_name = "TIME", value_parser = time_arg)]
        from: Option<SystemTime>,
        /// Those reported until then, not until now; with `--since` or
        /// `--from`.
        #[arg(long, value_name = "TIME", value_parser = time_arg)]
        to: Option<SystemTime>,
        /// Print what this jq filter makes of the JSON instead:
        /// `.document.cpu`, `.[] | select(.document.state != "ok")`.
        #[arg(long, value_name = "FILTER")]
        jq: Option<String>,
    },

    /// A device over time, as charts: its presence (from its heartbeats)
    /// and its system metrics -- CPU, load, memory, filesystems,
    /// temperature.
    Graph {
        #[command(flatten)]
        target: Target,
        /// How far back: `30m`, `24h`, `7d`.
        #[arg(long, default_value = "24h", value_parser = remora_time::parse_duration)]
        since: Duration,
        /// Only these, by the start of their name (`presence`, `cpu`,
        /// `disk`); repeatable.
        #[arg(long = "metric", value_name = "NAME")]
        metrics: Vec<String>,
        /// `chart` for humans, `json` for scripts (every point).
        #[arg(long, default_value = "chart")]
        format: GraphFormat,
    },

    /// The same, full screen and in color, refreshed as the device
    /// reports: its presence, what it last reported, and one document in
    /// detail with its numbers over time (system, network, access,
    /// processes).
    Watch {
        #[command(flatten)]
        target: Target,
        /// How far back: `30m`, `24h`, `7d`; changed with a key once in.
        #[arg(long, default_value = "24h", value_parser = remora_time::parse_duration)]
        since: Duration,
        /// How often to read the platform again.
        #[arg(long, default_value = "1m", value_parser = remora_time::parse_duration)]
        every: Duration,
        /// The document shown in detail to start with: `network`,
        /// `processes`... (`system` otherwise); changed with a key once in.
        #[arg(long, value_name = "NAME")]
        document: Option<String>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum StatusFormat {
    Table,
    Json,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum GraphFormat {
    Chart,
    Json,
}

pub async fn run(
    command: Command,
    service: &MonitorService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        Command::Status { target, format } => {
            let documents = latest(service, over, &target).await?;
            match format {
                StatusFormat::Json => {
                    print_json(&Value::Array(documents.iter().map(document_json).collect()))
                }
                StatusFormat::Table => {
                    let mut table = tui::Table::new(["document", "reported", "summary"]);
                    for document in &documents {
                        table.row(
                            [
                                document.name.clone(),
                                tui::ago(document.captured_at),
                                summary(document, SystemTime::now()),
                            ],
                            false,
                        );
                    }
                    table.print();
                }
            }
            Ok(())
        }
        Command::Watch {
            target,
            since,
            every,
            document,
        } => watch::run(service, over, &target, since, every, document).await,
        Command::Show {
            target,
            document,
            since,
            from,
            to,
            jq,
        } => {
            let now = SystemTime::now();
            if let Some(from) = period(since, from, to, now)? {
                let spinner = tui::Spinner::start("Reading the device's history");
                let reported = history(service, over, &target, &document, from).await;
                drop(spinner);
                let reported = within(reported.map_err(monitor_error)?, from, to);
                return print_filtered(
                    &Value::Array(reported.iter().map(document_json).collect()),
                    jq.as_deref(),
                );
            }
            let documents = latest(service, over, &target).await?;
            let Some(found) = documents.iter().find(|d| d.name == document) else {
                let reported: Vec<_> = documents.iter().map(|d| d.name.as_str()).collect();
                return Err(Report::new(Error::Monitor(format!(
                    "device {:?} reports no {document:?} document",
                    target.device
                )))
                .attach(format!("it reports: {}", reported.join(", "))));
            };
            print_filtered(&document_json(found), jq.as_deref())
        }
        Command::Graph {
            target,
            since,
            metrics: wanted,
            format,
        } => {
            let to = SystemTime::now();
            let from = to.checked_sub(since).unwrap_or(SystemTime::UNIX_EPOCH);
            let spinner = tui::Spinner::start("Reading the device's history");
            // Only what is asked for is read: a history is every document
            // in full.
            let only_presence =
                !wanted.is_empty() && wanted.iter().all(|w| "presence".starts_with(w.as_str()));
            let no_presence =
                !wanted.is_empty() && !wanted.iter().any(|w| "presence".starts_with(w.as_str()));
            let heartbeats = if no_presence {
                Ok(Vec::new())
            } else {
                history(service, over, &target, HEARTBEAT, from).await
            };
            let system = if only_presence {
                Ok(Vec::new())
            } else {
                history(service, over, &target, SYSTEM, from).await
            };
            drop(spinner);
            // A device reports one, the other or both: one it doesn't is
            // an empty history, not an error.
            let (heartbeats, system) = (
                heartbeats.map_err(monitor_error)?,
                system.map_err(monitor_error)?,
            );
            let is_wanted =
                |name: &str| wanted.is_empty() || wanted.iter().any(|w| name.starts_with(w));
            let presence = presence(&heartbeats, from, to).filter(|_| is_wanted("presence"));
            let metrics: Vec<_> = system_metrics(&system)
                .into_iter()
                .filter(|metric| is_wanted(&metric.name))
                .collect();
            match format {
                GraphFormat::Json => print_json(&json!({
                    "device": target.device,
                    "from": rfc3339(from),
                    "to": rfc3339(to),
                    "presence": presence.as_ref().map(|presence| json!({
                        "intervalSeconds": presence.interval.as_secs(),
                        "heartbeats": presence.beats.len(),
                        "availabilityPercent": presence.availability_percent,
                        "gaps": presence.gaps.iter().map(|(since, until)| json!({
                            "from": rfc3339(*since),
                            "to": rfc3339(*until),
                        })).collect::<Vec<_>>(),
                    })),
                    "metrics": metrics.iter().map(|metric| json!({
                        "name": metric.name,
                        "unit": match metric.unit {
                            Unit::Percent => "percent",
                            Unit::Celsius => "celsius",
                            Unit::Load => "load",
                            Unit::Bytes => "bytes",
                            Unit::BytesPerSecond => "bytes-per-second",
                            Unit::Dbm => "dbm",
                            Unit::Milliseconds => "milliseconds",
                        },
                        "points": metric.points.iter().map(|(at, value)| json!({
                            "at": rfc3339(*at),
                            "value": value,
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })),
                GraphFormat::Chart if presence.is_none() && metrics.is_empty() => {
                    tui::info(format!(
                        "Nothing to draw: {} reported nothing in the last {}.",
                        tui::accent(&target.device),
                        remora_time::format_duration(since)
                    ))
                }
                GraphFormat::Chart => {
                    let width = console::Term::stdout()
                        .size()
                        .1
                        .saturating_sub(2)
                        .clamp(*CHART_WIDTH.start(), *CHART_WIDTH.end());
                    println!(
                        "{} {}",
                        tui::accent(&target.device),
                        tui::dim(format!("· last {}", remora_time::format_duration(since)))
                    );
                    if let Some(presence) = &presence {
                        println!();
                        // As wide as a chart's plot, less its frame.
                        chart::print_presence(presence, from, to, usize::from(width) - 2);
                    }
                    for metric in &metrics {
                        println!();
                        chart::print(metric, from, to, width, CHART_HEIGHT);
                    }
                }
            }
            Ok(())
        }
    }
}

/// Where the period asked of `show` starts, if one is: `since` back from
/// `now`, or `from`. An end alone, or before its start, is a usage error.
fn period(
    since: Option<Duration>,
    from: Option<SystemTime>,
    to: Option<SystemTime>,
    now: SystemTime,
) -> Result<Option<SystemTime>> {
    let start = match (since, from) {
        (Some(since), _) => Some(now.checked_sub(since).unwrap_or(SystemTime::UNIX_EPOCH)),
        (None, from) => from,
    };
    match (start, to) {
        (None, Some(_)) => Err(Report::new(Error::Monitor(
            "--to ends a period: give its start, with --from or --since".to_owned(),
        ))),
        (Some(start), Some(to)) if to < start => Err(Report::new(Error::Monitor(format!(
            "the period ends ({}) before it starts ({})",
            rfc3339(to),
            rfc3339(start)
        )))),
        (start, _) => Ok(start),
    }
}

/// The documents captured from `from` to `to` (to the last one, without).
fn within(documents: Vec<Document>, from: SystemTime, to: Option<SystemTime>) -> Vec<Document> {
    documents
        .into_iter()
        .filter(|document| {
            document.captured_at >= from && to.is_none_or(|to| document.captured_at <= to)
        })
        .collect()
}

/// A document's history; empty for one the device doesn't report.
pub(crate) async fn history(
    service: &MonitorService,
    over: Option<&ContextOverride>,
    target: &Target,
    document: &str,
    from: SystemTime,
) -> std::result::Result<Vec<Document>, Report<remora_monitor::application::Error>> {
    use remora_monitor::application::Error as Monitor;
    match service
        .history(over, &target.bucket, &target.device, document, from)
        .await
    {
        Err(report) if matches!(report.current_context(), Monitor::NoDocument { .. }) => {
            Ok(Vec::new())
        }
        history => history,
    }
}

async fn latest(
    service: &MonitorService,
    over: Option<&ContextOverride>,
    target: &Target,
) -> Result<Vec<Document>> {
    let spinner = tui::Spinner::start("Reading what the device reported");
    let documents = service.latest(over, &target.bucket, &target.device).await;
    drop(spinner);
    documents.map_err(monitor_error)
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("json values serialize")
    );
}

/// The JSON, or what a jq filter makes of it.
fn print_filtered(value: &Value, jq: Option<&str>) -> Result<()> {
    let Some(jq) = jq else {
        print_json(value);
        return Ok(());
    };
    for line in crate::filter::apply(jq, value)? {
        println!("{line}");
    }
    Ok(())
}

/// `--from`/`--to`: an RFC 3339 time, UTC when no offset is given.
fn time_arg(text: &str) -> std::result::Result<SystemTime, String> {
    remora_time::parse_time(text)
        .ok_or_else(|| format!("{text:?} is not a time like 2026-11-14T08:00:00Z"))
}

fn rfc3339(time: SystemTime) -> String {
    remora_time::format_seconds(time)
}

fn document_json(document: &Document) -> Value {
    json!({
        "name": document.name,
        "schema": document.schema,
        "capturedAt": rfc3339(document.captured_at),
        "bootId": document.boot_id,
        "seq": document.seq,
        "document": document.body,
    })
}

/// A document in one line: the heartbeat as whether the device still
/// beats, the `system` one as its metrics and uptime, any other as its own
/// top-level fields (`state=ok, checks=4`).
pub(crate) fn summary(document: &Document, now: SystemTime) -> String {
    if document.name == HEARTBEAT {
        let every = span(heartbeat_interval(document).as_secs());
        return if is_silent(document, now) {
            format!("every {every} · silent")
        } else {
            format!("every {every} · present")
        };
    }
    let parts: Vec<String> = if document.name == SYSTEM {
        let uptime = document.body["uptime-seconds"]
            .as_u64()
            .map(|seconds| format!("up {}", span(seconds)));
        system_metrics(std::slice::from_ref(document))
            .iter()
            .filter_map(|metric| {
                let (_, last) = metric.points.last()?;
                Some(format!(
                    "{} {}",
                    metric.name,
                    chart::value(metric.unit, *last)
                ))
            })
            .chain(uptime)
            .collect()
    } else {
        // What reads at a glance first, what is only counted after.
        let fields = || document.body.as_object().into_iter().flatten();
        let plain = fields().filter_map(|(key, value)| match value {
            // A date says little in a summary: the document's own is in
            // the table already.
            Value::String(text) if remora_time::parse_time(text).is_some() => None,
            Value::String(text) => Some(format!("{key}={text}")),
            Value::Bool(_) | Value::Number(_) => Some(format!("{key}={value}")),
            _ => None,
        });
        let counted = fields().filter_map(|(key, value)| match value {
            Value::Array(items) => Some(format!("{key}={}", items.len())),
            Value::Object(fields) => Some(format!("{key}={{{}}}", fields.len())),
            _ => None,
        });
        plain.chain(counted).collect()
    };
    let (separator, mut line) = if document.name == SYSTEM {
        (" · ", String::new())
    } else {
        (", ", String::new())
    };
    line.push_str(&parts.join(separator));
    if line.chars().count() > SUMMARY_LENGTH {
        line = line.chars().take(SUMMARY_LENGTH - 1).collect();
        line.push('…');
    }
    line
}

/// `3d 4h`, `4h 12m`, `12m`: the two largest units are plenty.
pub(crate) fn span(seconds: u64) -> String {
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3600,
        seconds % 3600 / 60,
    );
    match (days, hours) {
        (0, 0) if minutes == 0 => format!("{seconds}s"),
        (0, 0) => format!("{minutes}m"),
        (0, _) if minutes == 0 => format!("{hours}h"),
        (0, _) => format!("{hours}h {minutes}m"),
        (_, 0) => format!("{days}d"),
        _ => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Summed up now.
    fn summary(document: &Document) -> String {
        super::summary(document, SystemTime::now())
    }

    fn document(name: &str, body: Value) -> Document {
        Document {
            name: name.into(),
            schema: format!("remora-monitoring-{name}-v1"),
            captured_at: SystemTime::UNIX_EPOCH,
            boot_id: None,
            seq: None,
            body,
        }
    }

    #[test]
    fn the_system_document_is_summed_up_as_its_metrics() {
        let line = summary(&document(
            "system",
            json!({
                "uptime-seconds": 273_600,
                "cpu": {"load": [0.416, 0.3, 0.2], "usage-percent": 12.4},
                "memory": {"total-bytes": 1000, "available-bytes": 370},
                "filesystems": [
                    {"mount-point": "/mnt/datafs", "total-bytes": 100, "available-bytes": 59},
                ],
                "temperatures": [{"zone": "cpu", "celsius": 53.6}],
            }),
        ));
        assert_eq!(
            line,
            "cpu 12% · load 0.42 · memory 63% · disk /mnt/datafs 41% · temperature 54 °C · up 3d 4h"
        );
    }

    #[test]
    fn another_document_is_summed_up_as_its_fields() {
        let line = summary(&document(
            "health",
            json!({
                "state": "ok",
                "checks": [1, 2],
                "since": null,
                "failed": 0,
                "last": {"a": 1},
                "evaluated-at": "2026-11-14T11:54:18.564957111Z",
            }),
        ));
        assert_eq!(line, "failed=0, state=ok, checks=2, last={1}");
        let long = summary(&document("x", json!({"text": "a".repeat(300)})));
        assert_eq!(long.chars().count(), SUMMARY_LENGTH);
        assert!(long.ends_with('…'));
        assert_eq!(summary(&document("x", json!("text"))), "");
    }

    #[test]
    fn a_period_starts_since_or_from_and_ends_after_its_start() {
        let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        assert_eq!(period(None, None, None, at(100)).unwrap(), None);
        assert_eq!(
            period(Some(Duration::from_secs(60)), None, None, at(100)).unwrap(),
            Some(at(40))
        );
        assert_eq!(
            period(None, Some(at(10)), Some(at(20)), at(100)).unwrap(),
            Some(at(10))
        );
        assert!(period(None, None, Some(at(20)), at(100)).is_err());
        assert!(period(None, Some(at(30)), Some(at(20)), at(100)).is_err());

        let captured = |seconds| Document {
            captured_at: at(seconds),
            ..document("system", json!({}))
        };
        let seconds = |from, to| -> Vec<SystemTime> {
            within(
                vec![captured(5), captured(10), captured(20), captured(30)],
                at(from),
                to,
            )
            .iter()
            .map(|document| document.captured_at)
            .collect()
        };
        assert_eq!(seconds(10, None), [at(10), at(20), at(30)]);
        assert_eq!(seconds(10, Some(at(20))), [at(10), at(20)]);
    }

    #[test]
    fn a_span_keeps_its_two_largest_units() {
        assert_eq!(span(59), "59s");
        assert_eq!(span(300), "5m");
        assert_eq!(span(750), "12m");
        assert_eq!(span(7200), "2h");
        assert_eq!(span(15_120), "4h 12m");
        assert_eq!(span(273_600), "3d 4h");
        assert_eq!(span(86_400), "1d");
    }

    #[test]
    fn a_heartbeat_is_summed_up_as_the_devices_presence() {
        let mut beat = document("heartbeat", json!({"interval": 300}));
        assert_eq!(summary(&beat), "every 5m · silent");
        beat.captured_at = SystemTime::now();
        assert_eq!(summary(&beat), "every 5m · present");
    }
}
