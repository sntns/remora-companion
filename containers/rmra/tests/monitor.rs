//! `rmra monitor` against the in-process fake gateway's time series store.

use std::{
    process::Stdio,
    time::{Duration, SystemTime},
};

use remora_platform_http::test_gateway::TestGateway;
use serde_json::json;

const BUCKET: &str = "thing-data";
const DEV1: &str = "sntns:iot:test:acme:thing:DEV1";
const SYSTEM: &str = "sntns:catalog:test:acme:schema:remora-monitoring-system-v1";
const HEALTH: &str = "sntns:catalog:test:acme:schema:remora-monitoring-health-v1";
const HEARTBEAT: &str = "sntns:catalog:test:acme:schema:remora-device-heartbeat";

fn rfc3339(time: SystemTime) -> String {
    remora_platform_http::model::timestamp(time)
}

/// A day of a device's reports, as an account's pipelines store them: a
/// heartbeat every 5 minutes and a `system` document every 30, but for a
/// few hours offline, and a `health` document.
fn report_a_day(gateway: &TestGateway) {
    let now = SystemTime::now();
    for beat in (0..288u64).filter(|beat| !(120..156).contains(beat)) {
        gateway.store_point(
            BUCKET,
            DEV1,
            HEARTBEAT,
            &rfc3339(now - Duration::from_secs((287 - beat) * 300 + 30)),
            json!({"interval": 300}),
        );
    }
    for step in (0..48u32).filter(|step| !(20..26).contains(step)) {
        let captured = now - Duration::from_secs(u64::from(47 - step) * 1800 + 60);
        let wave = f64::from(step % 12) / 11.0;
        gateway.store_point(
            BUCKET,
            DEV1,
            SYSTEM,
            &rfc3339(captured + Duration::from_secs(2)),
            json!({
                "schema-version": 1,
                "boot-id": "b1",
                "seq": step,
                "captured-at": rfc3339(captured),
                "clock-synced": true,
                "monotonic-ms": u64::from(step) * 1_800_000,
                "document": {
                    "uptime-seconds": u64::from(step) * 1800,
                    "cpu": {
                        "count": 4,
                        "load": [0.2 + wave * 2.0, 0.5, 0.4],
                        "usage-percent": 5.0 + wave * 70.0,
                    },
                    "memory": {"total-bytes": 4_000_000_000u64, "available-bytes": 1_480_000_000u64},
                    "filesystems": [{
                        "mount-point": "/mnt/datafs",
                        "total-bytes": 1000,
                        "available-bytes": 900 - u64::from(step) * 5,
                    }],
                    "temperatures": [
                        {"zone": "cpu", "celsius": 45.0 + wave * 12.0},
                        {"zone": "board", "celsius": 38.0},
                    ],
                    "disks": [],
                },
            }),
        );
    }
    gateway.store_point(
        BUCKET,
        DEV1,
        HEALTH,
        &rfc3339(now - Duration::from_secs(300)),
        json!({
            "captured-at": rfc3339(now - Duration::from_secs(302)),
            "clock-synced": true,
            "document": {"state": "ok", "checks": [{"name": "net"}, {"name": "disk"}]},
        }),
    );
}

#[tokio::test]
async fn shows_what_a_device_reports_as_tables_charts_and_json() {
    let (gateway, resolved) = TestGateway::serve().await;
    report_a_day(&gateway);
    let config = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("config.json"),
        format!(
            r#"{{"version":2,"contexts":{{"test":{{"endpoint":{{"address":"{}","tls":{{"disabled":true}}}}}}}}}}"#,
            resolved.endpoint.address
        ),
    )
    .unwrap();
    std::fs::write(
        config.path().join("credentials.json"),
        r#"{"test":{"kind":"access-key","token":"t"}}"#,
    )
    .unwrap();

    let run = |args: &'static [&'static str]| {
        let config = config.path().to_path_buf();
        async move {
            let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_rmra"))
                .args(args)
                .env("RMRA_CONFIG", config)
                .env_remove("RMRA_CONTEXT")
                .env_remove("RMRA_MONITOR_BUCKET")
                .stdin(Stdio::null())
                .output()
                .await
                .unwrap();
            (
                output.status.success(),
                String::from_utf8(output.stdout).unwrap(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )
        }
    };

    // The latest of each document, summed up.
    let (ok, table, stderr) = run(&["monitor", "status", "DEV1"]).await;
    assert!(ok, "{stderr}");
    let lines: Vec<_> = table.lines().map(str::trim_end).collect();
    assert_eq!(
        lines[0].split_whitespace().collect::<Vec<_>>(),
        ["DOCUMENT", "REPORTED", "SUMMARY"]
    );
    assert!(
        lines[1].starts_with("health") && lines[1].ends_with("state=ok, checks=2"),
        "{table}"
    );
    assert!(
        lines[2].starts_with("heartbeat") && lines[2].ends_with("every 5m · present"),
        "{table}"
    );
    assert!(
        lines[3].starts_with("system") && lines[3].contains("memory 63%"),
        "{table}"
    );
    assert!(lines[3].ends_with("up 23h 30m"), "{table}");

    let (ok, json, stderr) = run(&["monitor", "status", "DEV1", "--format", "json"]).await;
    assert!(ok, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json[1]["document"]["interval"], 300);
    assert_eq!(json[2]["name"], "system");
    assert_eq!(json[2]["schema"], SYSTEM);
    assert_eq!(json[2]["seq"], 47);
    assert_eq!(json[2]["document"]["cpu"]["count"], 4);

    // One document, in full.
    let (ok, json, stderr) = run(&["monitor", "show", "DEV1", "health"]).await;
    assert!(ok, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["document"]["state"], "ok");
    let (ok, _, stderr) = run(&["monitor", "show", "DEV1", "network"]).await;
    assert!(!ok);
    assert!(
        stderr.contains("reports no \"network\" document"),
        "{stderr}"
    );

    // A document over a period: every one reported in it, oldest first.
    let (ok, json, stderr) = run(&["monitor", "show", "DEV1", "system", "--since", "3h"]).await;
    assert!(ok, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    let reported = json.as_array().unwrap();
    assert_eq!(reported.len(), 6, "{json}");
    assert_eq!(reported[0]["seq"], 42);
    assert_eq!(reported[5]["seq"], 47);
    let (ok, json, stderr) = run(&[
        "monitor",
        "show",
        "DEV1",
        "system",
        "--from",
        "2000-01-01T00:00:00Z",
        "--to",
        "2000-01-02T00:00:00Z",
    ])
    .await;
    assert!(ok, "{stderr}");
    assert_eq!(json.trim(), "[]");

    // Through a jq filter.
    let (ok, out, stderr) = run(&[
        "monitor", "show", "DEV1", "system", "--since", "3h", "--jq", ".[].seq",
    ])
    .await;
    assert!(ok, "{stderr}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        ["42", "43", "44", "45", "46", "47"]
    );
    let (ok, out, stderr) = run(&[
        "monitor",
        "show",
        "DEV1",
        "health",
        "--jq",
        ".document.state",
    ])
    .await;
    assert!(ok, "{stderr}");
    assert_eq!(out.trim(), "ok");
    let (ok, _, stderr) = run(&["monitor", "show", "DEV1", "health", "--jq", ".["]).await;
    assert!(!ok);
    assert!(stderr.contains("is not a jq filter"), "{stderr}");
    let (ok, _, stderr) = run(&[
        "monitor",
        "show",
        "DEV1",
        "system",
        "--to",
        "2000-01-01T00:00:00Z",
    ])
    .await;
    assert!(!ok);
    assert!(stderr.contains("--to ends a period"), "{stderr}");

    // Its presence and its system metrics over time.
    let (ok, charts, stderr) = run(&["monitor", "graph", "DEV1"]).await;
    assert!(ok, "{stderr}");
    println!("{charts}");
    assert!(
        charts.contains("252 heartbeats, one every 5m · 1 silence of 3h"),
        "{charts}"
    );
    for name in [
        "presence",
        "cpu",
        "load",
        "memory",
        "disk /mnt/datafs",
        "temperature",
    ] {
        assert!(
            charts.lines().any(|line| line.starts_with(name)),
            "{name}: {charts}"
        );
    }
    // The scale's ends left of each plot, and curves in braille.
    assert!(
        charts.contains("100%│") && charts.contains("  0%│"),
        "{charts}"
    );
    assert!(
        charts
            .chars()
            .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
        "{charts}"
    );
    assert!(charts.contains('█'), "{charts}");

    let (ok, json, stderr) = run(&[
        "monitor", "graph", "DEV1", "--since", "6h", "--metric", "cpu", "--format", "json",
    ])
    .await;
    assert!(ok, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    let metrics = json["metrics"].as_array().unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0]["name"], "cpu");
    assert_eq!(metrics[0]["unit"], "percent");
    assert_eq!(metrics[0]["points"].as_array().unwrap().len(), 12);
    assert!(json["presence"].is_null());

    let (ok, json, stderr) = run(&[
        "monitor", "graph", "DEV1", "--metric", "presence", "--format", "json",
    ])
    .await;
    assert!(ok, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(json["metrics"].as_array().unwrap().is_empty());
    assert_eq!(json["presence"]["heartbeats"], 252);
    assert_eq!(json["presence"]["intervalSeconds"], 300);
    assert_eq!(json["presence"]["gaps"].as_array().unwrap().len(), 1);

    // A device that reports nothing, and a role the store refuses.
    let (ok, _, stderr) = run(&["monitor", "status", "DEV2"]).await;
    assert!(!ok);
    assert!(
        stderr.contains("no monitoring data for device \"DEV2\""),
        "{stderr}"
    );
    gateway.0.lock().unwrap().tbss_denied = true;
    let (ok, _, stderr) = run(&["monitor", "status", "DEV1"]).await;
    assert!(!ok);
    assert!(
        stderr.contains("not allowed to read monitoring data"),
        "{stderr}"
    );
}
