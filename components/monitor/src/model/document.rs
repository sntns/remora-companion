use std::time::SystemTime;

use serde_json::Value;

/// The bucket an account's pipelines store what its devices send in --
/// heartbeats and monitoring documents alike, under the thing named after
/// the device (every message as a point, the latest as the shadow). An
/// account whose pipelines store it elsewhere says where.
pub const DEFAULT_BUCKET: &str = "thing-data";

/// What the schemas of a device's own reports are named after, whatever
/// the store puts in front (a URN): remora-monitord's documents
/// (`remora-monitoring-system-v1`) and remora-accessd's heartbeat
/// (`remora-device-heartbeat`).
const SCHEMA_MARKERS: [&str; 2] = ["remora-monitoring-", "remora-device-"];

/// A message as the store holds it: when, and its JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// The store's own timestamp for it.
    pub at: SystemTime,
    pub value: Value,
}

/// The document a schema is of, when it is one of a device's reports:
/// `...:schema:remora-monitoring-system-v1` is `system`,
/// `...:schema:remora-device-heartbeat` is `heartbeat`.
pub fn document_name(schema: &str) -> Option<&str> {
    let name = schema.rsplit(':').next()?;
    let name = SCHEMA_MARKERS
        .iter()
        .find_map(|marker| name.strip_prefix(marker))?;
    let name = match name.rsplit_once("-v") {
        Some((name, version))
            if !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => name,
    };
    (!name.is_empty()).then_some(name)
}

/// One monitoring document, as a device captured it.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// `heartbeat`, `system`, `network`, `health`...
    pub name: String,
    /// Its schema, as the store names it.
    pub schema: String,
    /// When the device read it, by its own clock once synchronized;
    /// otherwise when the store got it.
    pub captured_at: SystemTime,
    /// The device's boot it was read in, when it says.
    pub boot_id: Option<String>,
    /// Its rank among the boot's documents, when it says.
    pub seq: Option<u64>,
    /// What the provider read: its shape is the provider's own.
    pub body: Value,
}

impl Document {
    /// Reads a stored message: remora-monitord's envelope (`captured-at`,
    /// `boot-id`, `seq`, `document`), or the bare document.
    pub fn from_sample(name: &str, schema: &str, sample: Sample) -> Self {
        let Sample { at, mut value } = sample;
        let body = match value.get_mut("document") {
            Some(body) => body.take(),
            None => {
                return Self {
                    name: name.to_owned(),
                    schema: schema.to_owned(),
                    captured_at: at,
                    boot_id: None,
                    seq: None,
                    body: value,
                }
            }
        };
        // A date taken before the clock was synchronized is the device's
        // guess: the store's is closer.
        let synced = value.get("clock-synced").and_then(Value::as_bool) != Some(false);
        let captured_at = value
            .get("captured-at")
            .and_then(Value::as_str)
            .filter(|_| synced)
            .and_then(remora_time::parse_time)
            .unwrap_or(at);
        Self {
            name: name.to_owned(),
            schema: schema.to_owned(),
            captured_at,
            boot_id: value
                .get("boot-id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            seq: value.get("seq").and_then(Value::as_u64),
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn a_schema_names_its_document() {
        assert_eq!(document_name("remora-monitoring-system-v1"), Some("system"));
        assert_eq!(
            document_name("sntns:catalog:eu2:acme:schema:remora-monitoring-network-v12"),
            Some("network")
        );
        assert_eq!(document_name("remora-monitoring-my-app"), Some("my-app"));
        assert_eq!(
            document_name("sntns:catalog:eu2:acme:schema:remora-device-heartbeat"),
            Some("heartbeat")
        );
        assert_eq!(document_name("iot/measurement/sample/temperature"), None);
        assert_eq!(
            document_name("sntns:catalog:eu2:acme:schema:app/remora-device-x"),
            None
        );
        assert_eq!(document_name("remora-monitoring-"), None);
    }

    #[test]
    fn an_envelope_gives_the_document_and_the_devices_date() {
        let document = Document::from_sample(
            "system",
            "remora-monitoring-system-v1",
            Sample {
                at: at(2_000_000_000),
                value: json!({
                    "boot-id": "b1",
                    "seq": 42,
                    "captured-at": "2026-11-08T13:59:00Z",
                    "clock-synced": true,
                    "document": {"uptime-seconds": 60},
                }),
            },
        );
        assert_eq!(document.captured_at, at(1_794_146_340));
        assert_eq!(document.boot_id.as_deref(), Some("b1"));
        assert_eq!(document.seq, Some(42));
        assert_eq!(document.body, json!({"uptime-seconds": 60}));
    }

    #[test]
    fn an_unsynchronized_date_or_a_bare_document_takes_the_stores() {
        let unsynced = Document::from_sample(
            "system",
            "s",
            Sample {
                at: at(100),
                value: json!({
                    "captured-at": "1970-01-01T00:00:10Z",
                    "clock-synced": false,
                    "document": {},
                }),
            },
        );
        assert_eq!(unsynced.captured_at, at(100));

        let bare = Document::from_sample(
            "system",
            "s",
            Sample {
                at: at(100),
                value: json!({"uptime-seconds": 60}),
            },
        );
        assert_eq!(bare.captured_at, at(100));
        assert_eq!(bare.body, json!({"uptime-seconds": 60}));
    }
}
