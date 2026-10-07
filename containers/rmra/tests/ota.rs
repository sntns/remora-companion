//! The shipped `rmra` binary managing over-the-air updates against the
//! in-process fake remora gateway: publish a release (its upload dropped
//! mid-way and resumed on its own, or interrupted with Ctrl-C and resumed
//! with the printed token), roll it out, and follow it to the end.

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

    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_rmra"));
        command
            .args(args)
            .env("RMRA_CONFIG", self.config.path())
            .env_remove("RMRA_CONTEXT")
            .stdin(Stdio::null());
        command
    }

    async fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = self.command(args).output().await.unwrap();
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
    // Each refusal in the error, with the platform's reason.
    assert!(stderr.contains("could not be created"), "{stderr}");
    assert!(
        stderr.contains(r#"failed to create deployment "r1-dev1""#),
        "{stderr}"
    );
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
        .run(&[
            "deployment",
            "start",
            "r1-dev2",
            "--watch",
            "--interval",
            "1",
        ])
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("succeeded"), "{stderr}");

    let (code, stdout, _) = rmra.run(&["deployment", "logs", "r1-dev2"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("50/100 %") && stdout.contains("success"),
        "{stdout}"
    );
    let (code, stdout, _) = rmra
        .run(&["deployment", "logs", "r1-dev2", "--format", "json"])
        .await;
    assert_eq!(code, 0);
    let logs: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(logs[0]["progress"]["current"], 50);

    let (code, _, stderr) = rmra.run(&["deployment", "cancel", "r1-dev2"]).await;
    assert_eq!(code, 1);
    assert!(stderr.contains("terminal state"), "{stderr}");
    let (code, _, _) = rmra.run(&["deployment", "rm", "-f", "r1-dev2"]).await;
    assert_eq!(code, 0);
}

/// Ctrl-C mid-upload commits nothing and prints the command that resumes
/// it; running that command finishes the artifact.
#[cfg(unix)]
#[tokio::test]
async fn an_interrupted_upload_resumes_with_the_printed_token() {
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    let (gateway, resolved) = TestGateway::serve().await;
    let rmra = Rmra::new(&resolved.context.endpoint.address);
    let bundle_dir = tempfile::tempdir().unwrap();
    let bundle = bundle_dir.path().join("update-rp5.raucb");
    let bytes: Vec<u8> = (0..12_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&bundle, &bytes).unwrap();
    let (code, _, stderr) = rmra
        .run(&["release", "create", "R1", "--version", "1.0.0"])
        .await;
    assert_eq!(code, 0, "{stderr}");

    // A slow link, so that Ctrl-C lands mid-way.
    gateway.0.lock().unwrap().upload_chunk_delay = Some(Duration::from_millis(200));
    let mut child = rmra
        .command(&[
            "release",
            "upload",
            "R1",
            bundle.to_str().unwrap(),
            "--tag-condition",
            "board:rp5",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut seen = String::new();
    while !seen.contains("Uploading") {
        assert_ne!(stderr.read_line(&mut seen).await.unwrap(), 0, "{seen}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pid = child.id().unwrap().to_string();
    let killed = std::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .unwrap();
    assert!(killed.success());
    stderr.read_to_string(&mut seen).await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("rmra stops on Ctrl-C")
        .unwrap();
    assert_eq!(status.code(), Some(1), "{seen}");
    assert!(seen.contains("was cancelled"), "{seen}");
    assert!(seen.contains("Resume it with"), "{seen}");

    // Nothing committed: the platform only keeps the bytes for a resume.
    for _ in 0..500 {
        if gateway.0.lock().unwrap().abandoned_uploads > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(gateway.0.lock().unwrap().abandoned_uploads, 1);
    assert!(gateway.artifact_bytes("R1", "update-rp5.raucb").is_none());

    let token = seen
        .rsplit("--resume ")
        .next()
        .unwrap()
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect::<String>();
    assert_eq!(token.len(), 32, "{seen}");
    gateway.0.lock().unwrap().upload_chunk_delay = None;
    let (code, _, stderr) = rmra
        .run(&[
            "release",
            "upload",
            "R1",
            bundle.to_str().unwrap(),
            "--tag-condition",
            "board:rp5",
            "--resume",
            &token,
        ])
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("continued from"), "{stderr}");
    assert_eq!(
        gateway.artifact_bytes("R1", "update-rp5.raucb").unwrap(),
        bytes
    );
}

#[tokio::test]
async fn download_picks_an_artifact_by_board_and_type() {
    let (gateway, resolved) = TestGateway::serve().await;
    let rmra = Rmra::new(&resolved.context.endpoint.address);
    let dir = tempfile::tempdir().unwrap();
    let image: Vec<u8> = (0..2_500_000u32).map(|i| (i % 241) as u8).collect();
    let bundle: Vec<u8> = (0..100_000u32).map(|i| (i % 7) as u8).collect();
    std::fs::write(dir.path().join("disk-rp5.wic.bmaptar"), &image).unwrap();
    std::fs::write(dir.path().join("update-rp5.raucb"), &bundle).unwrap();

    let (code, _, stderr) = rmra
        .run(&["release", "create", "R1", "--version", "1.0.0"])
        .await;
    assert_eq!(code, 0, "{stderr}");
    for (file, tags) in [
        ("disk-rp5.wic.bmaptar", "board:rp5 && type:diskimage"),
        ("update-rp5.raucb", "board:rp5 && type:rauc"),
    ] {
        let path = dir.path().join(file);
        let (code, _, stderr) = rmra
            .run(&[
                "release",
                "upload",
                "R1",
                path.to_str().unwrap(),
                "--tag-condition",
                tags,
            ])
            .await;
        assert_eq!(code, 0, "{stderr}");
    }

    // Two artifacts for rp5 and nobody at the terminal: it says which.
    let out = tempfile::tempdir().unwrap();
    let out_dir = out.path().to_str().unwrap();
    let (code, _, stderr) = rmra
        .run(&["release", "download", "R1", "--board", "rp5", "-o", out_dir])
        .await;
    assert_ne!(code, 0);
    assert!(stderr.contains("several such artifacts"), "{stderr}");

    gateway.0.lock().unwrap().fail_next_download_after = Some(1_000_000);
    let (code, _, stderr) = rmra
        .run(&[
            "release",
            "download",
            "R1",
            "--board",
            "rp5",
            "--type",
            "diskimage",
            "-o",
            out_dir,
        ])
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Downloaded"), "{stderr}");
    assert_eq!(
        std::fs::read(out.path().join("disk-rp5.wic.bmaptar")).unwrap(),
        image
    );

    // Already there: kept, unless --force.
    let (code, _, stderr) = rmra
        .run(&[
            "release",
            "download",
            "R1",
            "--artifact",
            "disk-rp5.wic.bmaptar",
            "-o",
            out_dir,
        ])
        .await;
    assert_ne!(code, 0);
    assert!(stderr.contains("already exists"), "{stderr}");
}
