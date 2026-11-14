//! A document as last reported, laid out to be read: the network's
//! interfaces and the processes as tables, any other as its fields.

use ratatui::text::{Line, Span};
use remora_monitor::model::{Document, NETWORK, PROCESSES};
use serde_json::Value;

use crate::{
    area::{Paint, BAD, GOOD, TEXT, TITLE, WARN},
    chart::bytes,
    service::span,
};

/// The lines that show `document`.
pub(crate) fn lines(document: &Document, paint: Paint) -> Vec<Line<'static>> {
    match document.name.as_str() {
        NETWORK => network(&document.body, paint),
        PROCESSES => processes(&document.body, paint),
        _ => fields(&document.body, paint),
    }
}

/// A text or `-`.
fn text(value: &Value) -> String {
    value.as_str().unwrap_or("-").to_owned()
}

/// A count or `-`.
fn count(value: &Value) -> String {
    value
        .as_u64()
        .map_or_else(|| "-".to_owned(), |count| count.to_string())
}

/// A number of bytes or `-`.
fn size(value: &Value) -> String {
    value.as_f64().map_or_else(|| "-".to_owned(), bytes)
}

/// A row of cells, each padded to its column's width, in its own style.
fn row<const N: usize>(
    cells: [(String, ratatui::style::Style); N],
    widths: [usize; N],
) -> Line<'static> {
    Line::from(
        cells
            .into_iter()
            .zip(widths)
            .map(|((cell, style), width)| Span::styled(format!("{cell:<width$} "), style))
            .collect::<Vec<_>>(),
    )
}

fn header<const N: usize>(names: [&str; N], widths: [usize; N], paint: Paint) -> Line<'static> {
    row(names.map(|name| (name.to_owned(), paint.dim())), widths)
}

/// `label` then what it is, on a line.
fn labelled(label: &str, what: String, paint: Paint) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), paint.dim()),
        Span::styled(what, paint.fg(TEXT)),
    ])
}

/// How good a Wi-Fi signal is.
fn signal_color(dbm: f64) -> crate::area::Rgb {
    match dbm {
        dbm if dbm >= -65.0 => GOOD,
        dbm if dbm >= -78.0 => WARN,
        _ => BAD,
    }
}

/// The Internet connection, each interface with its addresses and its
/// traffic -- under it its Wi-Fi link or its mesh --, and the routes.
fn network(body: &Value, paint: Paint) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let internet = &body["internet"];
    let mut reached = match internet["connection"].as_str() {
        Some(connection) => format!(
            "{connection} via {} on {}",
            text(&internet["gateway"]),
            text(&internet["interface"])
        ),
        None => "none".to_owned(),
    };
    if let Some(profile) = body["profile"].as_str() {
        reached.push_str(&format!(" · profile {profile}"));
    }
    lines.push(labelled("internet", reached, paint));

    let widths = [10, 8, 12, 18, 9, 9, 8, 6];
    lines.push(header(
        [
            "INTERFACE",
            "STATE",
            "CONNECTION",
            "ADDRESS",
            "RECEIVED",
            "SENT",
            "DROPPED",
            "ERRORS",
        ],
        widths,
        paint,
    ));
    for interface in body["interfaces"].as_array().into_iter().flatten() {
        let state = text(&interface["operstate"]);
        let state_style = match state.as_str() {
            "up" => paint.fg(GOOD),
            "down" => paint.dim(),
            _ => paint.fg(WARN),
        };
        let addresses: Vec<&str> = interface["addresses"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|address| address["address"].as_str())
            .collect();
        let counters = &interface["counters"];
        let sum = |first: &str, second: &str| {
            let sum =
                counters[first].as_u64().unwrap_or(0) + counters[second].as_u64().unwrap_or(0);
            (
                sum.to_string(),
                if sum == 0 {
                    paint.dim()
                } else {
                    paint.fg(WARN)
                },
            )
        };
        let plain = paint.fg(TEXT);
        lines.push(row(
            [
                (text(&interface["name"]), paint.bold(TITLE)),
                (state, state_style),
                (text(&interface["active"]), plain),
                (
                    if addresses.is_empty() {
                        "-".to_owned()
                    } else {
                        addresses.join(" ")
                    },
                    plain,
                ),
                (size(&counters["rx-bytes"]), plain),
                (size(&counters["tx-bytes"]), plain),
                sum("rx-dropped", "tx-dropped"),
                sum("rx-errors", "tx-errors"),
            ],
            widths,
        ));
        let link = &interface["wifi-link"];
        if link.is_object() {
            let mut wifi = vec![
                Span::styled("  wifi  ", paint.dim()),
                Span::styled(text(&link["ssid"]), plain),
            ];
            if let Some(dbm) = link["signal-dbm"].as_f64() {
                wifi.push(Span::styled(" · ", paint.dim()));
                wifi.push(Span::styled(
                    format!("{dbm:.0} dBm"),
                    paint.bold(signal_color(dbm)),
                ));
            }
            let mut rest = String::new();
            if let Some(frequency) = link["frequency-mhz"].as_u64() {
                rest.push_str(&format!(" · {frequency} MHz"));
            }
            if let Some(busy) = link["channel-utilization-percent"].as_f64() {
                rest.push_str(&format!(" · channel {busy:.0}% busy"));
            }
            if let Some(stations) = link["stations"].as_u64() {
                rest.push_str(&format!(" · {stations} stations"));
            }
            wifi.push(Span::styled(rest, paint.dim()));
            lines.push(Line::from(wifi));
        }
        let mesh = &interface["mesh"];
        if mesh.is_object() {
            let length = |of: &Value| of.as_array().map_or(0, Vec::len);
            lines.push(labelled(
                "  mesh",
                format!(
                    "{} · {} links · {} gateways",
                    text(&mesh["gateway-mode"]),
                    length(&mesh["links"]),
                    length(&mesh["gateways"])
                ),
                paint,
            ));
        }
    }
    let routes: Vec<String> = body["routes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|route| {
            let mut line = text(&route["destination"]);
            if let Some(gateway) = route["gateway"].as_str() {
                line.push_str(&format!(" via {gateway}"));
            }
            format!("{line} on {}", text(&route["interface"]))
        })
        .collect();
    if !routes.is_empty() {
        lines.push(labelled("routes", routes.join(" · "), paint));
    }
    lines
}

/// Each selected process, and how its services fare.
fn processes(body: &Value, paint: Paint) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let widths = [24, 7, 5, 6, 9, 7, 8, 8];
    lines.push(header(
        [
            "PROCESS", "PID", "STATE", "CPU", "MEMORY", "THREADS", "RESTARTS", "UP",
        ],
        widths,
        paint,
    ));
    let plain = paint.fg(TEXT);
    for process in body["processes"].as_array().into_iter().flatten() {
        let restarts = process["restarts"].as_u64().unwrap_or(0);
        lines.push(row(
            [
                (text(&process["name"]), paint.bold(TITLE)),
                (count(&process["pid"]), paint.dim()),
                (text(&process["state"]), plain),
                (
                    process["cpu"]["usage-percent"]
                        .as_f64()
                        .map_or_else(|| "-".to_owned(), |cpu| format!("{cpu:.1}%")),
                    plain,
                ),
                (size(&process["memory"]["rss-bytes"]), plain),
                (count(&process["threads"]), plain),
                (
                    restarts.to_string(),
                    if restarts == 0 {
                        paint.dim()
                    } else {
                        paint.fg(WARN)
                    },
                ),
                (
                    process["uptime-seconds"]
                        .as_u64()
                        .map_or_else(|| "-".to_owned(), span),
                    plain,
                ),
            ],
            widths,
        ));
    }
    let services: Vec<&Value> = body["services"].as_array().into_iter().flatten().collect();
    if !services.is_empty() {
        let state = |service: &&Value| service["active-state"].as_str().map(str::to_owned);
        let active = services
            .iter()
            .filter(|service| state(service).as_deref() == Some("active"))
            .count();
        // A service that ran and ended is not one that failed.
        let failed: Vec<String> = services
            .iter()
            .filter(|service| state(service).as_deref() == Some("failed"))
            .map(|service| text(&service["unit"]))
            .collect();
        let restarts: u64 = services
            .iter()
            .filter_map(|service| service["restarts"].as_u64())
            .sum();
        let mut counts = format!("{} · {active} active", services.len());
        let other = services.len() - active - failed.len();
        if other > 0 {
            counts.push_str(&format!(" · {other} not running"));
        }
        counts.push_str(&format!(" · {restarts} restarts"));
        let mut line = vec![
            Span::styled("services  ", paint.dim()),
            Span::styled(counts, plain),
        ];
        if !failed.is_empty() {
            line.push(Span::styled(" · ", paint.dim()));
            line.push(Span::styled(
                format!("failed: {}", failed.join(", ")),
                paint.bold(BAD),
            ));
        }
        lines.push(Line::from(line));
    }
    lines
}

/// A value on a line: a duration and a size by the name of their field,
/// a list as its items.
fn scalar(key: &str, value: &Value) -> String {
    match value {
        Value::Null => "-".to_owned(),
        Value::String(text) => text.clone(),
        Value::Number(number) => match (number.as_u64(), number.as_f64()) {
            (Some(seconds), _) if key.ends_with("seconds") || key.ends_with("seconds-ago") => {
                span(seconds)
            }
            (_, Some(size)) if key.ends_with("bytes") => bytes(size),
            // `rtt-ms.average` is one; `rtt-ms.samples` counts them.
            _ if key.ends_with("-ms") || (key.contains("-ms.") && !key.ends_with("samples")) => {
                format!("{number} ms")
            }
            _ => number.to_string(),
        },
        Value::Array(items) => items
            .iter()
            .map(|item| scalar(key, item))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| format!("{key}={}", scalar(key, value)))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Bool(flag) => flag.to_string(),
    }
}

/// Every field with its value, those of an object under its name
/// (`rtt-ms.average`), a list of objects an item a line.
fn flatten(prefix: &str, value: &Value, into: &mut Vec<(String, String)>) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let name = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&name, value, into);
            }
        }
        Value::Array(items) if items.iter().any(Value::is_object) => {
            for item in items {
                into.push((prefix.to_owned(), scalar(prefix, item)));
            }
        }
        value => into.push((prefix.to_owned(), scalar(prefix, value))),
    }
}

/// How a field named `state` reads: fine, or worth a look.
const FINE: [&str; 6] = [
    "healthy",
    "established",
    "connected",
    "active",
    "idle",
    "ok",
];

fn fields(body: &Value, paint: Paint) -> Vec<Line<'static>> {
    let mut flat = Vec::new();
    flatten("", body, &mut flat);
    let names = flat
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0)
        .min(32);
    let mut before = "";
    flat.iter()
        .map(|(name, value)| {
            // A list's name once, over its items.
            let shown = if name == before { "" } else { name.as_str() };
            before = name;
            let style = match name.rsplit('.').next() {
                Some("state" | "phase" | "connection") if FINE.contains(&value.as_str()) => {
                    paint.bold(GOOD)
                }
                Some("state" | "phase" | "connection") => paint.bold(WARN),
                _ => paint.fg(TEXT),
            };
            Line::from(vec![
                Span::styled(format!("{shown:<names$}  "), paint.dim()),
                Span::styled(value.clone(), style),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use serde_json::json;

    use super::*;
    use crate::area::Theme;

    const PAINT: Paint = Paint(Theme { truecolor: true });

    fn shown(name: &str, body: Value) -> Vec<String> {
        let document = Document {
            name: name.into(),
            schema: format!("remora-monitoring-{name}-v1"),
            captured_at: SystemTime::UNIX_EPOCH,
            boot_id: None,
            seq: None,
            body,
        };
        lines(&document, PAINT)
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect()
    }

    #[test]
    fn the_network_is_its_interfaces_and_its_routes() {
        let shown = shown(
            NETWORK,
            json!({
                "profile": "default",
                "internet": {"connection": "Home", "gateway": "192.0.2.1", "interface": "wlan0"},
                "interfaces": [
                    {
                        "name": "wlan0", "operstate": "up", "active": "Home",
                        "addresses": [{"address": "192.0.2.7/24"}],
                        "counters": {
                            "rx-bytes": 4_516_980, "tx-bytes": 3_317_051,
                            "rx-dropped": 0, "tx-dropped": 2, "rx-errors": 0, "tx-errors": 0,
                        },
                        "wifi-link": {
                            "ssid": "home", "signal-dbm": -58, "frequency-mhz": 2437,
                            "channel-utilization-percent": 76.5, "stations": 5,
                        },
                    },
                    {
                        "name": "bat0", "operstate": "unknown", "addresses": [],
                        "counters": {"rx-bytes": 0, "tx-bytes": 0},
                        "mesh": {"gateway-mode": "client", "links": [{}], "gateways": []},
                    },
                    {"name": "eth0", "operstate": "down", "addresses": [], "counters": {}},
                ],
                "routes": [
                    {"destination": "default", "gateway": "192.0.2.1", "interface": "wlan0"},
                    {"destination": "192.0.2.0/24", "interface": "wlan0"},
                ],
            }),
        );
        assert_eq!(
            shown,
            [
                "internet  Home via 192.0.2.1 on wlan0 · profile default",
                "INTERFACE  STATE    CONNECTION   ADDRESS            RECEIVED  SENT      DROPPED  ERRORS",
                "wlan0      up       Home         192.0.2.7/24       4.3 MiB   3.2 MiB   2        0",
                "  wifi  home · -58 dBm · 2437 MHz · channel 76% busy · 5 stations",
                "bat0       unknown  -            -                  0 B       0 B       0        0",
                "  mesh  client · 1 links · 0 gateways",
                "eth0       down     -            -                  -         -         0        0",
                "routes  default via 192.0.2.1 on wlan0 · 192.0.2.0/24 on wlan0",
            ]
        );
    }

    #[test]
    fn the_processes_are_a_table_and_their_services() {
        let shown = shown(
            PROCESSES,
            json!({
                "processes": [{
                    "name": "remora-accessd", "pid": 514, "state": "S", "threads": 5,
                    "restarts": 0, "uptime-seconds": 25_966,
                    "cpu": {"usage-percent": 0.3}, "memory": {"rss-bytes": 20_684_800},
                }],
                "services": [
                    {"unit": "a.service", "active-state": "active", "restarts": 1},
                    {"unit": "b.service", "active-state": "failed", "restarts": 2},
                    {"unit": "c.service", "active-state": "inactive", "restarts": 0},
                ],
            }),
        );
        assert_eq!(
            shown,
            [
                "PROCESS                  PID     STATE CPU    MEMORY    THREADS RESTARTS UP",
                "remora-accessd           514     S     0.3%   20 MiB    5       0        7h 12m",
                "services  3 · 1 active · 1 not running · 3 restarts · failed: b.service",
            ]
        );
    }

    #[test]
    fn any_other_document_is_its_fields() {
        let shown = shown(
            "access",
            json!({
                "state": "established",
                "session-seconds": 12_532,
                "rtt-ms": {"last": 52, "max": 311, "samples": 279},
                "last-failure": {"cause": "dns", "seconds-ago": 26_335},
                "checks": [{"name": "heartbeat", "value": true}, {"name": "disk", "value": false}],
                "last-error": null,
            }),
        );
        assert_eq!(
            shown,
            [
                "checks                    name=heartbeat, value=true",
                "                          name=disk, value=false",
                "last-error                -",
                "last-failure.cause        dns",
                "last-failure.seconds-ago  7h 18m",
                "rtt-ms.last               52 ms",
                "rtt-ms.max                311 ms",
                "rtt-ms.samples            279",
                "session-seconds           3h 28m",
                "state                     established",
            ]
        );
    }
}
