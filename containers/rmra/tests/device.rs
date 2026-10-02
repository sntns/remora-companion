//! `rmra device list` against the in-process fake remora gateway.

use std::process::Stdio;

use remora_ota_adapter_grpc::test_gateway::TestGateway;

#[tokio::test]
async fn lists_devices_with_their_labels() {
    let (_, resolved) = TestGateway::serve().await;
    let config = tempfile::tempdir().unwrap();
    let dir = config.path().join("contexts/test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("meta.json"),
        format!(
            r#"{{"name":"test","endpoint":{{"address":"{}","tls":{{"disabled":true}}}}}}"#,
            resolved.context.endpoint.address
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("credentials.json"),
        r#"{"kind":"access-key","token":"t"}"#,
    )
    .unwrap();

    let run = |args: &'static [&'static str]| {
        let config = config.path().to_path_buf();
        async move {
            let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_rmra"))
                .args(args)
                .env("RMRA_CONFIG", config)
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
        ["NAME", "LABELS"]
    );
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
}
