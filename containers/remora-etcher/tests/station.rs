//! `remora-etcher station serve` and `station simulate`, the shipped binary
//! end to end: a station issuing as a context pointing at the in-process
//! fake gateway, a simulated hub claiming from it, the operator's scan
//! typed on the station's stdin, and the hub's `remora-factory.yaml` read
//! the way remora-edge's `FactoryIdentity` reads it.

use std::{process::Stdio, time::Duration};

use remora_context::{
    adapter::{credentials::CredentialStoreAdapter, store::ContextStoreAdapter},
    model::Context,
};
use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};
use remora_ota_adapter_grpc::test_gateway::TestGateway;
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

/// remora-edge's `FactoryIdentity` (access-application's configuration),
/// field for field: kebab-case, `key-id` required, both authorities
/// optional.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)]
struct FactoryIdentity {
    url: String,
    key: String,
    certificate: String,
    key_id: String,
    #[serde(default)]
    authority: Option<String>,
    #[serde(default)]
    server_authority: Option<String>,
}

fn etcher(config: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_remora-etcher"));
    command
        .env("RMRA_CONFIG", config)
        .env_remove("RMRA_CONTEXT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_simulated_hub_claims_is_labelled_and_writes_its_identity() {
    let (gateway, resolved) = TestGateway::serve().await;
    let config = tempfile::tempdir().unwrap();
    FileContextStoreImpl::new(config.path())
        .put(&Context {
            name: "factory".into(),
            ..resolved.context.clone()
        })
        .unwrap();
    FileCredentialStoreImpl::new(config.path())
        .put("factory", &resolved.credentials)
        .unwrap();

    let work = tempfile::tempdir().unwrap();
    std::fs::create_dir(work.path().join("station.d")).unwrap();
    let port = free_port().await;
    let station_yaml = work.path().join("station.yaml");
    std::fs::write(
        &station_yaml,
        format!(
            "listen: 127.0.0.1:{port}\njournal: ./station.jsonl\nhooks: ./station.d\n\
             presence-timeout: 10s\n"
        ),
    )
    .unwrap();

    let mut serve = etcher(config.path())
        .args(["-c", "factory", "station", "serve", "--config"])
        .arg(&station_yaml)
        .args(["--serial-policy", "hub-virtual=hubs-virtual"])
        .spawn()
        .unwrap();
    let mut station_in = serve.stdin.take().unwrap();
    let mut station_out = BufReader::new(serve.stdout.take().unwrap()).lines();
    // Its dashboard, kept for a failure's message (and so a full pipe never
    // blocks it).
    let mut dashboard = serve.stderr.take().unwrap();
    let dashboard = tokio::spawn(async move {
        let mut text = String::new();
        let _ = tokio::io::AsyncReadExt::read_to_string(&mut dashboard, &mut text).await;
        text
    });

    let url = format!("http://127.0.0.1:{port}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the station never listened"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let output = work.path().join("remora-factory.yaml");
    let simulate = etcher(config.path())
        .args([
            "station",
            "simulate",
            "--url",
            &url,
            "--board",
            "hub-virtual",
        ])
        .args([
            "--temp-hostname",
            "525400123456",
            "--poll-interval",
            "100ms",
        ])
        .arg("--output")
        .arg(&output)
        .spawn()
        .unwrap();

    // The station prints the serial it issued; the operator scans it.
    let issued = tokio::time::timeout(Duration::from_secs(30), station_out.next_line())
        .await
        .expect("the station issues a serial")
        .unwrap()
        .expect("a serial on stdout");
    assert_eq!(issued, "hubs-virtual-1");
    // Scanned again until it takes, like an operator would: a scan while
    // the label is still printing is refused.
    let mut waiting = Box::pin(simulate.wait_with_output());
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    let simulated = loop {
        station_in
            .write_all(format!("{issued}\n").as_bytes())
            .await
            .unwrap();
        tokio::select! {
            done = &mut waiting => break done.unwrap(),
            () = &mut deadline => panic!("the hub never finished"),
            () = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
    };
    assert!(
        simulated.status.success(),
        "{}",
        String::from_utf8_lossy(&simulated.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&simulated.stdout),
        "hubs-virtual-1\n"
    );

    let yaml = std::fs::read_to_string(&output).unwrap();
    let identity: FactoryIdentity = yaml_serde::from_str(&yaml).unwrap();
    assert_eq!(identity.url, "https://access.test/access/v1");
    assert_eq!(identity.key_id, "urn:test:certificate-hubs-virtual-1-1");
    p256::SecretKey::from_sec1_pem(&identity.key).expect("a SEC1 P-256 key");
    assert_eq!(
        pem::parse(&identity.certificate).unwrap().contents(),
        b"idevid hubs-virtual-1"
    );
    assert_eq!(
        pem::parse(identity.server_authority.unwrap())
            .unwrap()
            .contents(),
        b"server ca"
    );
    assert_eq!(gateway.0.lock().unwrap().factory_devices.len(), 1);

    // The operator quits; the journal has the whole story.
    station_in.write_all(b"q\n").await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(30), serve.wait())
        .await
        .expect("the station stops")
        .unwrap();
    let dashboard = dashboard.await.unwrap();
    assert!(status.success(), "{dashboard}");
    assert!(dashboard.contains("Labelled"), "{dashboard}");
    let journal = std::fs::read_to_string(work.path().join("station.jsonl")).unwrap();
    let events: Vec<String> = journal
        .lines()
        .map(|line| {
            let entry: serde_json::Value = serde_json::from_str(line).unwrap();
            entry["event"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(
        events,
        [
            "received",
            "issued",
            "label-active",
            "labelled",
            "installed"
        ]
    );
}
