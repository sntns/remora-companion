use std::time::SystemTime;

use serde_json::Value;

use super::Document;

/// The document the metrics are read from.
pub const SYSTEM: &str = "system";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// 0 to 100.
    Percent,
    Celsius,
    /// Runnable tasks, averaged: no unit.
    Load,
    Bytes,
    BytesPerSecond,
    /// A radio signal's strength.
    Dbm,
    Milliseconds,
}

/// One number of a device, over time.
#[derive(Debug, Clone, PartialEq)]
pub struct Metric {
    /// `cpu`, `load`, `memory`, `disk /mnt/datafs`, `temperature`.
    pub name: String,
    pub unit: Unit,
    /// Oldest first.
    pub points: Vec<(SystemTime, f64)>,
}

/// The numbers worth a glance in `system` documents (oldest first): CPU
/// use and load, memory and filesystems in use, the hottest temperature.
/// A document without one of them leaves no point for it.
pub fn system_metrics(documents: &[Document]) -> Vec<Metric> {
    let mut metrics: Vec<Metric> = Vec::new();
    let mut add = |name: String, unit: Unit, at: SystemTime, value: Option<f64>| {
        let Some(value) = value.filter(|value| value.is_finite()) else {
            return;
        };
        match metrics.iter_mut().find(|metric| metric.name == name) {
            Some(metric) => metric.points.push((at, value)),
            None => metrics.push(Metric {
                name,
                unit,
                points: vec![(at, value)],
            }),
        }
    };
    for document in documents {
        let (at, body) = (document.captured_at, &document.body);
        let cpu = &body["cpu"];
        add(
            "cpu".into(),
            Unit::Percent,
            at,
            cpu["usage-percent"].as_f64(),
        );
        add("load".into(), Unit::Load, at, cpu["load"][0].as_f64());
        add(
            "memory".into(),
            Unit::Percent,
            at,
            used_percent(&body["memory"]),
        );
        for filesystem in body["filesystems"].as_array().into_iter().flatten() {
            if let Some(mount_point) = filesystem["mount-point"].as_str() {
                add(
                    format!("disk {mount_point}"),
                    Unit::Percent,
                    at,
                    used_percent(filesystem),
                );
            }
        }
        let hottest = body["temperatures"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|zone| zone["celsius"].as_f64())
            .reduce(f64::max);
        add("temperature".into(), Unit::Celsius, at, hottest);
    }
    // The same order whichever document first had which.
    metrics.sort_by_key(|metric| match metric.name.as_str() {
        "cpu" => 0,
        "load" => 1,
        "memory" => 2,
        "temperature" => 4,
        _ => 3,
    });
    metrics
}

/// The documents with numbers over time, beside [`SYSTEM`].
pub const NETWORK: &str = "network";
pub const ACCESS: &str = "access";
pub const PROCESSES: &str = "processes";

/// The numbers worth a glance in the documents named `name` (oldest
/// first): [`system_metrics`] for `system`; the traffic of each interface
/// and its Wi-Fi link for `network`; the platform session's round trip for
/// `access`; the selected processes' CPU use and memory for `processes`.
/// None for a document without numbers over time.
pub fn metrics(name: &str, documents: &[Document]) -> Vec<Metric> {
    match name {
        SYSTEM => system_metrics(documents),
        NETWORK => network_metrics(documents),
        ACCESS => access_metrics(documents),
        PROCESSES => processes_metrics(documents),
        _ => Vec::new(),
    }
}

/// Adds a point to the metric `name`, the first of them making it.
fn push(metrics: &mut Vec<Metric>, name: String, unit: Unit, at: SystemTime, value: Option<f64>) {
    let Some(value) = value.filter(|value| value.is_finite()) else {
        return;
    };
    match metrics.iter_mut().find(|metric| metric.name == name) {
        Some(metric) => metric.points.push((at, value)),
        None => metrics.push(Metric {
            name,
            unit,
            points: vec![(at, value)],
        }),
    }
}

/// Per interface: what it received and sent a second since the document
/// before (`rx`, `tx` -- from its counters, within a boot), and its Wi-Fi
/// link's signal and channel use. An interface nothing went through has
/// no traffic to show.
fn network_metrics(documents: &[Document]) -> Vec<Metric> {
    // What each interface's counters were, when, and in which boot.
    let mut before: Vec<(String, SystemTime, Option<&str>, f64, f64)> = Vec::new();
    let mut metrics = Vec::new();
    for document in documents {
        let (at, boot) = (document.captured_at, document.boot_id.as_deref());
        let interfaces = document.body["interfaces"].as_array();
        for interface in interfaces.into_iter().flatten() {
            let Some(name) = interface["name"].as_str() else {
                continue;
            };
            let counters = &interface["counters"];
            if let (Some(rx), Some(tx)) =
                (counters["rx-bytes"].as_f64(), counters["tx-bytes"].as_f64())
            {
                let known = before.iter().position(|(known, ..)| known == name);
                if let Some((_, then, booted, rx_then, tx_then)) = known.map(|index| &before[index])
                {
                    let seconds = at
                        .duration_since(*then)
                        .map_or(0.0, |elapsed| elapsed.as_secs_f64());
                    // Counters start again with a boot.
                    if seconds > 0.0 && *booted == boot && rx >= *rx_then && tx >= *tx_then {
                        let rate = |now: f64, then: f64| Some((now - then) / seconds);
                        push(
                            &mut metrics,
                            format!("rx {name}"),
                            Unit::BytesPerSecond,
                            at,
                            rate(rx, *rx_then),
                        );
                        push(
                            &mut metrics,
                            format!("tx {name}"),
                            Unit::BytesPerSecond,
                            at,
                            rate(tx, *tx_then),
                        );
                    }
                }
                let now = (name.to_owned(), at, boot, rx, tx);
                match known {
                    Some(index) => before[index] = now,
                    None => before.push(now),
                }
            }
            let link = &interface["wifi-link"];
            push(
                &mut metrics,
                format!("signal {name}"),
                Unit::Dbm,
                at,
                link["signal-dbm"].as_f64(),
            );
            push(
                &mut metrics,
                format!("channel {name}"),
                Unit::Percent,
                at,
                link["channel-utilization-percent"].as_f64(),
            );
        }
    }
    metrics.retain(|metric| {
        metric.unit != Unit::BytesPerSecond || metric.points.iter().any(|(_, value)| *value > 0.0)
    });
    // An interface's metrics together, the same order whichever came first.
    metrics.sort_by_key(|metric| {
        let (kind, interface) = metric.name.split_once(' ').unwrap_or((&metric.name, ""));
        let kind = ["rx", "tx", "signal", "channel"]
            .iter()
            .position(|known| *known == kind);
        (interface.to_owned(), kind)
    });
    metrics
}

/// The round trip to the platform, as last measured.
fn access_metrics(documents: &[Document]) -> Vec<Metric> {
    let mut metrics = Vec::new();
    for document in documents {
        push(
            &mut metrics,
            "rtt".into(),
            Unit::Milliseconds,
            document.captured_at,
            document.body["rtt-ms"]["last"].as_f64(),
        );
    }
    metrics
}

/// The selected processes together: their CPU use, their memory.
fn processes_metrics(documents: &[Document]) -> Vec<Metric> {
    let mut metrics = Vec::new();
    for document in documents {
        let Some(processes) = document.body["processes"].as_array() else {
            continue;
        };
        let sum = |of: &dyn Fn(&Value) -> Option<f64>| -> Option<f64> {
            let values: Vec<f64> = processes.iter().filter_map(of).collect();
            (!values.is_empty()).then(|| values.iter().sum())
        };
        push(
            &mut metrics,
            "cpu".into(),
            Unit::Percent,
            document.captured_at,
            sum(&|process| process["cpu"]["usage-percent"].as_f64()),
        );
        push(
            &mut metrics,
            "memory".into(),
            Unit::Bytes,
            document.captured_at,
            sum(&|process| process["memory"]["rss-bytes"].as_f64()),
        );
    }
    metrics
}

/// What is not available of a total, in percent.
fn used_percent(of: &Value) -> Option<f64> {
    let total = of["total-bytes"].as_f64().filter(|total| *total > 0.0)?;
    let available = of["available-bytes"].as_f64()?;
    Some(((total - available) / total * 100.0).clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    fn document(seconds: u64, body: Value) -> Document {
        Document {
            name: SYSTEM.into(),
            schema: "remora-monitoring-system-v1".into(),
            captured_at: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
            boot_id: None,
            seq: None,
            body,
        }
    }

    #[test]
    fn reads_the_numbers_of_system_documents() {
        let metrics = system_metrics(&[
            // The first read of a boot has no CPU use yet.
            document(
                0,
                json!({
                    "cpu": {"count": 4, "load": [0.5, 0.4, 0.3]},
                    "memory": {"total-bytes": 1000, "available-bytes": 250},
                    "filesystems": [
                        {"mount-point": "/mnt/datafs", "total-bytes": 200, "available-bytes": 150},
                    ],
                    "temperatures": [
                        {"zone": "cpu", "celsius": 51.5},
                        {"zone": "gpu", "celsius": 48.0},
                    ],
                }),
            ),
            document(
                300,
                json!({
                    "cpu": {"load": [1.5, 0.4, 0.3], "usage-percent": 12.5},
                    "memory": {"total-bytes": 1000, "available-bytes": 500},
                    "filesystems": [],
                    "temperatures": [],
                }),
            ),
        ]);
        let named = |name: &str| {
            let metric = metrics.iter().find(|m| m.name == name).unwrap();
            let values: Vec<f64> = metric.points.iter().map(|(_, value)| *value).collect();
            (metric.unit, values)
        };
        assert_eq!(named("cpu"), (Unit::Percent, vec![12.5]));
        assert_eq!(named("load"), (Unit::Load, vec![0.5, 1.5]));
        assert_eq!(named("memory"), (Unit::Percent, vec![75.0, 50.0]));
        assert_eq!(named("disk /mnt/datafs"), (Unit::Percent, vec![25.0]));
        assert_eq!(named("temperature"), (Unit::Celsius, vec![51.5]));
        let order: Vec<_> = metrics.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            order,
            ["cpu", "load", "memory", "disk /mnt/datafs", "temperature"]
        );
    }

    #[test]
    fn a_document_of_another_shape_gives_nothing() {
        assert!(system_metrics(&[document(0, json!({"state": "ok"}))]).is_empty());
        assert!(system_metrics(&[document(0, json!("text"))]).is_empty());
        assert!(system_metrics(&[document(
            0,
            json!({"memory": {"total-bytes": 0, "available-bytes": 0}})
        )])
        .is_empty());
    }

    fn named(name: &str, seconds: u64, boot: &str, body: Value) -> Document {
        Document {
            name: name.into(),
            boot_id: Some(boot.into()),
            ..document(seconds, body)
        }
    }

    fn values(metrics: &[Metric], name: &str) -> Vec<f64> {
        metrics
            .iter()
            .find(|metric| metric.name == name)
            .map(|metric| metric.points.iter().map(|(_, value)| *value).collect())
            .unwrap_or_default()
    }

    #[test]
    fn reads_the_traffic_and_the_wifi_link_of_network_documents() {
        let interfaces = |rx: u64, tx: u64, signal: i64| {
            json!({"interfaces": [
                {"name": "eth0", "counters": {"rx-bytes": 0, "tx-bytes": 0}},
                {
                    "name": "wlan0",
                    "counters": {"rx-bytes": rx, "tx-bytes": tx},
                    "wifi-link": {"signal-dbm": signal, "channel-utilization-percent": 40.0},
                },
            ]})
        };
        let found = metrics(
            NETWORK,
            &[
                named(NETWORK, 0, "a", interfaces(1000, 500, -60)),
                named(NETWORK, 100, "a", interfaces(4000, 500, -58)),
                // A boot: the counters start again, no rate across it.
                named(NETWORK, 200, "b", interfaces(100, 50, -70)),
                named(NETWORK, 300, "b", interfaces(300, 1050, -71)),
            ],
        );
        let names: Vec<&str> = found.iter().map(|metric| metric.name.as_str()).collect();
        // Nothing went through eth0.
        assert_eq!(
            names,
            ["rx wlan0", "tx wlan0", "signal wlan0", "channel wlan0"]
        );
        assert_eq!(values(&found, "rx wlan0"), [30.0, 2.0]);
        assert_eq!(values(&found, "tx wlan0"), [0.0, 10.0]);
        assert_eq!(values(&found, "signal wlan0"), [-60.0, -58.0, -70.0, -71.0]);
        assert_eq!(found[0].unit, Unit::BytesPerSecond);
        assert_eq!(found[2].unit, Unit::Dbm);
    }

    #[test]
    fn reads_the_round_trip_and_the_processes() {
        let found = metrics(
            ACCESS,
            &[
                named(ACCESS, 0, "a", json!({"rtt-ms": {"last": 52}})),
                named(ACCESS, 900, "a", json!({"state": "connecting"})),
                named(ACCESS, 1800, "a", json!({"rtt-ms": {"last": 31}})),
            ],
        );
        assert_eq!(values(&found, "rtt"), [52.0, 31.0]);
        assert_eq!(found[0].unit, Unit::Milliseconds);

        let found = metrics(
            PROCESSES,
            &[named(
                PROCESSES,
                0,
                "a",
                json!({"processes": [
                    {"cpu": {"usage-percent": 0.3}, "memory": {"rss-bytes": 2000}},
                    {"cpu": {"usage-percent": 1.2}, "memory": {"rss-bytes": 500}},
                ]}),
            )],
        );
        assert_eq!(values(&found, "cpu"), [1.5]);
        assert_eq!(values(&found, "memory"), [2500.0]);

        // A document without numbers over time.
        assert!(metrics(
            "health",
            &[named("health", 0, "a", json!({"state": "healthy"}))]
        )
        .is_empty());
    }
}
