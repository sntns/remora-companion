//! The shipped `rmra` binary managing over-the-air updates against the
//! in-process fake remora gateway: publish a release (its upload dropped
//! mid-way and resumed on its own), roll it out, and follow it to the end.

use std::process::Stdio;

use remora_ota_adapter_grpc::test_gateway::TestGateway;

struct Rmra {
    config: tempfile::TempDir,
}

impl Rmra {
    /// A context logged in to `address`, written straight to disk: login
    /// itself is covered by fake_gateway.rs, and this gateway serves no IAM.
    fn new(address: &str) -> Self {
        let config = tempfile::tempdir().unwrap();
        let dir = config.path().join("contexts/test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("meta.json"),
            format!(r#"{{"name":"test","endpoint":{{"address":"{address}","tls":{{"disabled":true}}}}}}"#),
        )
        .unwrap();
        std::fs::write(
            dir.join("credentials.json"),
            r#"{"kind":"access-key","token":"t"}"#,
        )
        .unwrap();
        Self { config }
    }

    async fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_rmra"))
            .args(args)
            .env("RMRA_CONFIG", self.config.path())
            .env_remove("RMRA_CONTEXT")
            .stdin(Stdio::null())
            .output()
            .await
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

#[tokio::test]
async fn publish_deploy_and_follow_an_update() {
    let (gateway, resolved) = TestGateway::serve().await;
    let rmra = Rmra::new(&resolved.context.endpoint.address);

    let bundle_dir = tempfile::tempdir().unwrap();
    let bundle = bundle_dir.path().join("update-rp5.raucb");
    let bytes: Vec<u8> = (0..3_500_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&bundle, &bytes).unwrap();
    gateway.0.lock().unwrap().fail_next_upload_after = Some(1_500_000);

    let (code, _, stderr) = rmra
        .run(&[
            "release",
            "create",
            "R1",
            "--version",
            "2.0.0",
            "--label",
            "channel=beta",
            "--artifact",
            bundle.to_str().unwrap(),
            "--tag-condition",
            "board:rp5",
        ])
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("resumed 1 time after a dropped connection"),
        "{stderr}"
    );
    assert_eq!(
        gateway.artifact_bytes("R1", "update-rp5.raucb").unwrap(),
        bytes
    );

    let (code, stdout, _) = rmra
        .run(&["release", "show", "R1", "--format", "json"])
        .await;
    assert_eq!(code, 0);
    let release: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(release["version"], "2.0.0");
    assert_eq!(release["artifacts"][0]["tagCondition"], "board:rp5");
    assert_eq!(release["artifacts"][0]["contentLength"], bytes.len());

    let (code, stdout, _) = rmra
        .run(&["release", "ls", "--label", "channel=beta", "-q"])
        .await;
    assert_eq!((code, stdout.trim()), (0, "R1"));

    // A selector reaching several devices is never deployed to unasked.
    let (code, _, stderr) = rmra.run(&["deploy", "R1", "--selector", "board=rp5"]).await;
    assert_eq!(code, 1);
    assert!(stderr.contains("--yes"), "{stderr}");

    // One device succeeds, one fails: watching reports it and fails.
    let (code, stdout, stderr) = rmra
        .run(&[
            "deploy",
            "R1",
            "--device",
            "DEV1",
            "--device",
            "BROKEN",
            "--watch",
            "--interval",
            "1",
        ])
        .await;
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stdout.contains("r1-dev1") && stdout.contains("r1-broken"),
        "{stdout}"
    );
    assert!(
        stderr.contains("1 of 2 deployments did not succeed"),
        "{stderr}"
    );
    assert!(stderr.contains("bundle signature invalid"), "{stderr}");

    // DEV1 is done, so it may get another; DEV2 has nothing in flight.
    let (code, stdout, stderr) = rmra
        .run(&[
            "deploy",
            "R1",
            "--selector",
            "board=rp5",
            "--yes",
            "--draft",
        ])
        .await;
    // r1-dev1 and r1-broken already exist by those names: refused, DEV2 drafted.
    assert_eq!(code, 1, "{stderr}");
    assert!(stdout.contains("draft"), "{stdout}");
    assert!(
        stderr.contains("1 of 3 deployments created as drafts"),
        "{stderr}"
    );

    let (code, stdout, _) = rmra.run(&["deployment", "ls", "--format", "json"]).await;
    assert_eq!(code, 0);
    let listed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let statuses: Vec<_> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            format!(
                "{}={}",
                d["name"].as_str().unwrap(),
                d["status"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        statuses,
        ["r1-broken=FAILED", "r1-dev1=SUCCEEDED", "r1-dev2=PENDING"]
    );

    let (code, _, stderr) = rmra
        .run(&["deployment", "start", "r1-dev2", "--watch"])
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("succeeded"), "{stderr}");

    let (code, stdout, _) = rmra.run(&["deployment", "logs", "r1-dev2"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("50/100 %") && stdout.contains("success"),
        "{stdout}"
    );

    let (code, _, stderr) = rmra.run(&["deployment", "cancel", "r1-dev2"]).await;
    assert_eq!(code, 1);
    assert!(stderr.contains("terminal state"), "{stderr}");
    let (code, _, _) = rmra.run(&["deployment", "rm", "-f", "r1-dev2"]).await;
    assert_eq!(code, 0);
}
