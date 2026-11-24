//! `rmra device list` and `rmra device rm` against the in-process fake remora gateway.

use std::process::Stdio;

use remora_platform_http::test_gateway::TestGateway;

#[tokio::test]
async fn lists_devices_with_their_labels() {
    let (gateway, resolved) = TestGateway::serve().await;
    gateway
        .0
        .lock()
        .unwrap()
        .device_created_at
        .insert("DEV1".into(), "2026-11-02T08:15:00Z".into());
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
                // Tables show local time; JSON stays UTC.
                .env("TZ", "Europe/Paris")
                .env_remove("RMRA_CONTEXT")
                .stdin(Stdio::null())
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        }
    };

    let table = run(&["device", "list"]).await;
    let lines: Vec<_> = table.lines().map(str::trim_end).collect();
    assert_eq!(
        lines[0].split_whitespace().collect::<Vec<_>>(),
        ["NAME", "LABELS", "ENROLLED"]
    );
    assert!(
        lines.contains(&"DEV1     board=rp5   2026-11-02 09:15:00"),
        "{table}"
    );
    // Created before the platform kept it: no date.
    assert!(lines.contains(&"DEV3     board=hdc"), "{table}");
    assert_eq!(lines.len(), 5, "{table}");

    assert_eq!(
        run(&["device", "ls", "-q", "--label", "board=hdc"]).await,
        "DEV3\n"
    );

    let json: serde_json::Value = serde_json::from_str(
        &run(&["device", "ls", "--format", "json", "--label", "board=rp5"]).await,
    )
    .unwrap();
    assert_eq!(json.as_array().unwrap().len(), 3);
    assert_eq!(json[1]["name"], "DEV1");
    assert_eq!(json[1]["labels"]["board"], "rp5");
    assert_eq!(json[1]["enrolledAt"], "2026-11-02 08:15:00Z");
    assert_eq!(json[2]["enrolledAt"], serde_json::Value::Null);
}

#[tokio::test]
async fn removes_a_device() {
    let (gateway, resolved) = TestGateway::serve().await;
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
            tokio::process::Command::new(env!("CARGO_BIN_EXE_rmra"))
                .args(args)
                .env("RMRA_CONFIG", config)
                // Tables show local time; JSON stays UTC.
                .env("TZ", "Europe/Paris")
                .env_remove("RMRA_CONTEXT")
                .stdin(Stdio::null())
                .output()
                .await
                .unwrap()
        }
    };

    // Not a terminal: no confirmation asked.
    let output = run(&["device", "rm", "DEV3"]).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!gateway.0.lock().unwrap().devices.contains_key("DEV3"));

    let output = run(&["device", "rm", "--force", "DEV3"]).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no device \"DEV3\""), "{stderr}");
}

#[tokio::test]
async fn lists_factory_devices_with_and_without_their_device() {
    let (gateway, resolved) = TestGateway::serve().await;
    {
        let mut state = gateway.0.lock().unwrap();
        // DEV3 is enrolled; NEW1 was sold and waits, NEW2 is in stock.
        for serial in ["DEV3", "NEW1", "NEW2"] {
            state.factory_devices.insert(serial.into(), 1);
        }
        state.factory_owners.insert("NEW1".into(), "acme".into());
        state
            .factory_created_at
            .insert("DEV3".into(), "2026-10-01T10:00:00Z".into());
        state
            .factory_created_at
            .insert("NEW1".into(), "2026-11-20T14:30:00Z".into());
        state
            .device_created_at
            .insert("DEV3".into(), "2026-10-15T09:00:00Z".into());
    }
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
                // Tables show local time; JSON stays UTC.
                .env("TZ", "Europe/Paris")
                .env_remove("RMRA_CONTEXT")
                .stdin(Stdio::null())
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        }
    };

    // Oldest manufactured first: the undated ones (by name), DEV3 made in
    // October, NEW1 in November.
    assert_eq!(
        run(&["device", "ls", "-q", "--with-factory-devices"]).await,
        "BROKEN\nDEV1\nDEV2\nNEW2\nDEV3\nNEW1\n"
    );
    let table = run(&["device", "ls", "--with-factory-devices"]).await;
    let lines: Vec<_> = table.lines().map(str::trim_end).collect();
    assert_eq!(
        lines[0].split_whitespace().collect::<Vec<_>>(),
        ["NAME", "LABELS", "STATE", "CREATED", "ENROLLED"]
    );
    let words = |name: &str| -> Vec<String> {
        let line = lines.iter().find(|l| l.starts_with(name)).unwrap();
        line.split_whitespace().map(str::to_owned).collect()
    };
    assert_eq!(
        words("DEV3"),
        [
            "DEV3",
            "board=hdc",
            "enrolled",
            "2026-10-01",
            "12:00:00",
            "2026-10-15",
            "11:00:00"
        ]
    );
    assert_eq!(
        words("NEW1"),
        ["NEW1", "not", "enrolled", "2026-11-20", "15:30:00"]
    );
    // Made before the platform kept the date.
    assert_eq!(words("NEW2"), ["NEW2", "in", "stock"]);
    let json: serde_json::Value = serde_json::from_str(
        &run(&["device", "ls", "--format", "json", "--with-factory-devices"]).await,
    )
    .unwrap();
    assert_eq!(json.as_array().unwrap().len(), 6);
    assert_eq!(json[4]["name"], "DEV3");
    assert_eq!(json[4]["enrolled"], true);
    assert_eq!(json[4]["createdAt"], "2026-10-01 10:00:00Z");
    assert_eq!(json[4]["enrolledAt"], "2026-10-15 09:00:00Z");
    assert_eq!(json[5]["createdAt"], "2026-11-20 14:30:00Z");
    assert_eq!(json[5]["name"], "NEW1");
    assert_eq!(json[5]["enrolled"], false);
    assert_eq!(json[5]["owner"], "urn:sntns:iam:test:acme:account");
    // Without the flag, the listing is what it was.
    let json: serde_json::Value =
        serde_json::from_str(&run(&["device", "ls", "--format", "json"]).await).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 4);
    assert!(json[0].get("enrolled").is_none());

    assert_eq!(
        run(&["device", "factory", "ls", "-q"]).await,
        "NEW2\nDEV3\nNEW1\n"
    );
    assert_eq!(
        run(&["device", "factory", "ls", "-q", "--without-device"]).await,
        "NEW2\nNEW1\n"
    );
    let shown: serde_json::Value = serde_json::from_str(
        &run(&["device", "factory", "show", "NEW1", "--format", "json"]).await,
    )
    .unwrap();
    assert_eq!(shown["owner"], "urn:sntns:iam:test:acme:account");
    assert_eq!(shown["certificate"], "urn:test:certificate-NEW1-1");
    assert_eq!(shown["createdAt"], "2026-11-20 14:30:00Z");
    // A device shows with its factory record when the account made it,
    // without one otherwise.
    let made: serde_json::Value =
        serde_json::from_str(&run(&["device", "show", "DEV3", "--format", "json"]).await).unwrap();
    assert_eq!(made["name"], "DEV3");
    assert!(made["factory"]["certificate"].is_string(), "{made}");
    let bought: serde_json::Value =
        serde_json::from_str(&run(&["device", "show", "DEV1", "--format", "json"]).await).unwrap();
    assert_eq!(bought["factory"], serde_json::Value::Null);
    run(&["device", "show", "DEV3"]).await;
    let listed: serde_json::Value =
        serde_json::from_str(&run(&["device", "factory", "ls", "--format", "json"]).await).unwrap();
    assert_eq!(listed[0]["createdAt"], serde_json::Value::Null);
    assert_eq!(listed[1]["createdAt"], "2026-10-01 10:00:00Z");

    // Not a terminal: no confirmation asked.
    run(&["device", "factory", "rm", "NEW2"]).await;
    assert!(!gateway
        .0
        .lock()
        .unwrap()
        .factory_devices
        .contains_key("NEW2"));
    assert_eq!(
        run(&["device", "factory", "ls", "-q", "--without-device"]).await,
        "NEW1\n"
    );
}
