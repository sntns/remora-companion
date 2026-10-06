//! The station end to end, as `station serve` wires it: the HTTP routes
//! over the real use case, its JSONL journal and hook runner (real scripts
//! in a temporary directory), the real factory use case and gateway adapter
//! against the in-process fake gateway, and a real HTTP client playing the
//! devices. Only the operator console's display is a stub, keeping what it
//! was shown; the operator's scans and commands go to `handle`, as `serve`
//! sends them. Unix only: the hooks are shell scripts.

#![cfg(unix)]

use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use rcgen::{CertificateParams, DnType, KeyPair};
use remora_context::{
    adapter::{
        credentials::{CredentialStoreAdapter, CredentialStoreAdapterService},
        platform::PlatformSessionAdapterService,
        store::{ContextStoreAdapter, ContextStoreAdapterService},
    },
    application::ContextService,
    model::ResolvedContext,
};
use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};
use remora_context_adapter_grpc::PlatformSessionAdapterImpl;
use remora_context_application::ContextControllerImpl;
use remora_factory::{
    adapter::{
        credential::CredentialWriterAdapterService, key::DeviceKeyAdapterService,
        provisioning::FactoryProvisioningAdapterService,
    },
    application::FactoryService,
};
use remora_factory_adapter_grpc::FactoryGatewayAdapterImpl;
use remora_factory_adapter_local::{CredentialWriterAdapterImpl, DeviceKeyAdapterImpl};
use remora_factory_application::FactoryControllerImpl;
use remora_ota_adapter_grpc::test_gateway::TestGateway;
use remora_station::{
    adapter::{
        hooks::{HookEvent, HookRunnerAdapterService},
        journal::JournalAdapterService,
        operator::{
            LabelBlock, OperatorAdapter, OperatorAdapterService, OperatorEvent, OperatorInput,
        },
    },
    application::StationService,
    model::{BoardPolicy, ConfirmMode, DeviceNameTemplate, StationConfig},
};
use remora_station_adapter_hooks_process::ProcessHookRunnerImpl;
use remora_station_adapter_jsonl::JsonlJournalImpl;
use remora_station_application::StationControllerImpl;
use remora_station_application_transport_http::{serve, BODY_LIMIT};
use remora_station_protocol::{
    AckBody, AckState, ClaimBody, ClaimStatusBody, ErrorCode, HardwareBody, ImageBody, LabelBody,
    StateBody,
};
use tokio::sync::oneshot;

mod client;
use client::{Error, StationClient};

/// What the console was shown, kept for the test to inspect: the
/// terminal's own console can't be read back.
struct RecordingOperator(Arc<StdMutex<Vec<OperatorEvent>>>);

impl OperatorAdapter for RecordingOperator {
    fn show(&self, event: &OperatorEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

/// The contexts of a station's laptop: one, logged in, pointing at
/// `resolved`'s gateway.
fn contexts(resolved: &ResolvedContext) -> ContextService {
    let root = tempfile::tempdir().unwrap().keep();
    let store = FileContextStoreImpl::new(&root);
    store.put(&resolved.context).unwrap();
    let credentials = FileCredentialStoreImpl::new(&root);
    credentials
        .put(&resolved.context.name, &resolved.credentials)
        .unwrap();
    ContextService::new(ContextControllerImpl::new(
        ContextStoreAdapterService::new(store),
        CredentialStoreAdapterService::new(credentials),
        PlatformSessionAdapterService::new(PlatformSessionAdapterImpl),
    ))
}

fn factory(contexts: &ContextService) -> FactoryService {
    FactoryService::new(FactoryControllerImpl::new(
        contexts.clone(),
        DeviceKeyAdapterService::new(DeviceKeyAdapterImpl),
        FactoryProvisioningAdapterService::new(FactoryGatewayAdapterImpl),
        CredentialWriterAdapterService::new(CredentialWriterAdapterImpl),
    ))
}

fn config(root: &Path) -> StationConfig {
    StationConfig {
        journal: root.join("station.jsonl"),
        hooks: root.join("station.d"),
        max_claims: None,
        confirm: ConfirmMode::Scan,
        presence_timeout: Duration::from_secs(10),
        hook_timeout: Duration::from_secs(10),
        boards: BTreeMap::from([
            (
                "hub-v2".to_string(),
                BoardPolicy::SerialNumberPolicy("hubs-v2".into()),
            ),
            (
                "hub-v1".to_string(),
                BoardPolicy::DeviceName(DeviceNameTemplate::parse("{bsp_serial}").unwrap()),
            ),
        ]),
    }
}

/// `<hooks>/<event>.d/<name>`, a shell script.
fn hook(config: &StationConfig, event: &str, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let dir = config.hooks.join(format!("{event}.d"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Station {
    client: StationClient,
    service: StationService,
    shown: Arc<StdMutex<Vec<OperatorEvent>>>,
    journal: PathBuf,
    shutdown: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
    ticker: tokio::task::JoinHandle<()>,
}

impl Station {
    async fn start(contexts: &ContextService, config: StationConfig) -> Self {
        std::fs::create_dir_all(&config.hooks).unwrap();
        let journal = config.journal.clone();
        let shown = Arc::new(StdMutex::new(Vec::new()));
        let service = StationService::new(StationControllerImpl::new(
            contexts.clone(),
            factory(contexts),
            JournalAdapterService::new(JsonlJournalImpl::new()),
            HookRunnerAdapterService::new(ProcessHookRunnerImpl),
            OperatorAdapterService::new(RecordingOperator(shown.clone())),
        ));
        let summary = service.start(config, None).await.unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, stop) = oneshot::channel::<()>();
        let server = tokio::spawn({
            let service = service.clone();
            async move {
                serve(listener, service, async {
                    let _ = stop.await;
                })
                .await
                .unwrap()
            }
        });
        // The labelling queue's clock, as `serve` runs it.
        let ticker = tokio::spawn({
            let service = service.clone();
            async move {
                loop {
                    tokio::time::sleep(summary.tick).await;
                    service.tick().await.unwrap();
                }
            }
        });
        Self {
            client: StationClient::new(&url),
            service,
            shown,
            journal,
            shutdown,
            server,
            ticker,
        }
    }

    /// As `serve` stops: devices first, then the station.
    async fn stop(self) {
        self.ticker.abort();
        let _ = self.shutdown.send(());
        self.server.await.unwrap();
        self.service.shutdown().await.unwrap();
    }

    async fn send(&self, input: OperatorInput) {
        self.service.handle(input).await.unwrap();
    }

    async fn scan(&self, serial: &str) {
        self.send(OperatorInput::Scan(serial.to_string())).await;
    }

    fn shown(&self) -> Vec<OperatorEvent> {
        self.shown.lock().unwrap().clone()
    }

    fn journal(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&self.journal)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// `event` entries of the journal, as `(claim_id, entry)`.
    fn journaled(&self, event: &str) -> Vec<(String, serde_json::Value)> {
        self.journal()
            .into_iter()
            .filter(|entry| entry["event"] == event)
            .map(|entry| (entry["claim_id"].as_str().unwrap().to_string(), entry))
            .collect()
    }

    /// The `event` entries once there are `count` of them: transitions
    /// reach the journal just behind the state.
    async fn settled(&self, event: &str, count: usize) -> Vec<(String, serde_json::Value)> {
        eventually(event, || async {
            let entries = self.journaled(event);
            (entries.len() >= count).then_some(entries)
        })
        .await
    }

    /// How many `<event>.d` scripts finished for `serial`, and how many of
    /// those succeeded.
    fn hooks_finished(&self, event: HookEvent, serial: &str) -> (usize, usize) {
        let outcomes: Vec<bool> = self
            .shown()
            .into_iter()
            .filter_map(|shown| match shown {
                OperatorEvent::Hook {
                    event: ran,
                    hub,
                    outcome,
                } if ran == event && hub.serial.as_deref() == Some(serial) => {
                    Some(outcome.succeeded())
                }
                _ => None,
            })
            .collect();
        (outcomes.len(), outcomes.iter().filter(|ok| **ok).count())
    }
}

/// Polls `check` until it gives a value, for up to 15 s.
async fn eventually<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A device: its key (kept across retries, like a hub's pending key) and
/// what it says about itself.
struct Hub {
    key: KeyPair,
    hardware: HardwareBody,
}

impl Hub {
    fn new(board: &str, n: usize) -> Self {
        Self {
            key: KeyPair::generate().unwrap(),
            hardware: HardwareBody {
                board: board.into(),
                temp_hostname: format!("e2b4a1c09f{n:02}"),
                eth_mac: Some(format!("e2:b4:a1:c0:9f:{n:02}")),
                bsp_serial: Some(format!("c3d2{n:02}")),
                machine_id: Some(format!("{n:032}")),
                macs: BTreeMap::from([("wlan0".into(), format!("aa:bb:cc:dd:ee:{n:02}"))]),
            },
        }
    }

    /// A CSR for the hub's key; a different `common_name` makes a different
    /// CSR for the same key, like a hub rebuilding it at each attempt.
    fn csr(&self, common_name: &str) -> Vec<u8> {
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params
            .serialize_request(&self.key)
            .unwrap()
            .der()
            .as_ref()
            .to_vec()
    }

    fn body(&self, csr: &[u8]) -> ClaimBody {
        ClaimBody {
            csr: STANDARD.encode(csr),
            hardware: self.hardware.clone(),
            image: ImageBody {
                version: Some("1.4.0".into()),
                compatible: Some("v2".into()),
            },
        }
    }

    async fn claim(&self, station: &Station) -> Result<ClaimStatusBody, (u16, ErrorCode)> {
        station
            .client
            .claim(&self.body(&self.csr("claim")))
            .await
            .map_err(|report| code(&report))
    }

    /// A claim the station refuses.
    async fn claim_report(&self, station: &Station) -> error_stack::Report<Error> {
        station
            .client
            .claim(&self.body(&self.csr("claim")))
            .await
            .unwrap_err()
    }
}

/// The status and code of a refusal.
fn code(report: &error_stack::Report<Error>) -> (u16, ErrorCode) {
    match report.current_context() {
        Error::Status {
            status,
            code: Some(code),
            ..
        } => (*status, *code),
        other => panic!("not a coded HTTP refusal: {other} ({report:?})"),
    }
}

fn refusal(report: &error_stack::Report<Error>) -> (u16, Option<ErrorCode>, String, Option<u64>) {
    match report.current_context() {
        Error::Status {
            status,
            code,
            message,
            retry_after,
        } => (*status, *code, message.clone(), *retry_after),
        other => panic!("not an HTTP refusal: {other}"),
    }
}

async fn poll(station: &Station, claim: &ClaimStatusBody) -> ClaimStatusBody {
    station.client.status(&claim.claim_id).await.unwrap()
}

/// Waits until `claim` is the active one and its label is printed (the
/// station refuses a scan while `label.d` runs), then scans `scanned`.
async fn scan_when_printed(station: &Station, claim: &ClaimStatusBody, scanned: &str) {
    let serial = &claim.identity.serial_number;
    eventually("the hub to be active", || async {
        (poll(station, claim).await.label == LabelBody::Active).then_some(())
    })
    .await;
    eventually("its label to print", || async {
        (station.hooks_finished(HookEvent::Label, serial).0 > 0).then_some(())
    })
    .await;
    station.scan(scanned).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_concurrent_claims_are_issued_then_labelled_one_at_a_time_in_order() {
    let (gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    hook(
        &config,
        "label",
        "10-print",
        "echo \"printing $REMORA_SERIAL\"",
    );
    hook(&config, "issued", "10-register", "cat > /dev/null");
    let station = Arc::new(Station::start(&contexts(&resolved), config).await);

    let hubs: Vec<Arc<Hub>> = (0..10).map(|n| Arc::new(Hub::new("hub-v2", n))).collect();
    let claims = futures_join(hubs.iter().map(|hub| {
        let (hub, station) = (hub.clone(), station.clone());
        async move { hub.claim(&station).await.unwrap() }
    }))
    .await;

    let serials: HashSet<_> = claims
        .iter()
        .map(|claim| claim.identity.serial_number.clone())
        .collect();
    assert_eq!(serials.len(), 10, "ten distinct serials");
    assert_eq!(gateway.0.lock().unwrap().factory_devices.len(), 10);
    assert!(claims.iter().all(|claim| claim.state == StateBody::Issued));

    // Label them all, the way the operator would: whichever hub is steady.
    let mut labelled = Vec::new();
    while labelled.len() < 10 {
        let mut statuses = Vec::new();
        for claim in &claims {
            statuses.push(poll(&station, claim).await);
        }
        let active: Vec<_> = statuses
            .iter()
            .filter(|status| status.label == LabelBody::Active)
            .collect();
        assert!(active.len() <= 1, "never two active hubs: {active:?}");
        let Some(active) = active.first() else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        assert_eq!(active.queue_position, Some(0));
        let queued: Vec<_> = statuses
            .iter()
            .filter(|status| status.label == LabelBody::Queued)
            .filter_map(|status| status.queue_position)
            .collect();
        assert_eq!(
            queued.iter().copied().collect::<HashSet<_>>(),
            (1..=queued.len()).collect::<HashSet<_>>(),
            "queue positions 1, 2, ... behind the active hub"
        );
        scan_when_printed(&station, active, &active.identity.serial_number).await;
        let id = active.claim_id.clone();
        eventually("the scan to label it", || async {
            (station.client.status(&id).await.unwrap().label == LabelBody::Labelled).then_some(())
        })
        .await;
        labelled.push(id);
    }

    // FIFO: activated in the order they were issued.
    let issued: Vec<_> = station
        .settled("issued", 10)
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let activated: Vec<_> = station
        .settled("label-active", 10)
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(activated, issued);
    assert_eq!(labelled, issued);
    let station = Arc::try_unwrap(station).ok().unwrap();
    station.stop().await;
}

/// `futures::future::join_all`, without the dependency: each on its own
/// task, results in order.
async fn futures_join<T: Send + 'static>(
    futures: impl Iterator<Item = impl Future<Output = T> + Send + 'static>,
) -> Vec<T> {
    let handles: Vec<_> = futures.map(tokio::spawn).collect();
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    results
}

#[tokio::test]
async fn a_wrong_scan_changes_nothing_and_the_right_one_moves_on() {
    let (_gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    hook(&config, "label", "10-print", "true");
    let station = Station::start(&contexts(&resolved), config).await;

    let (first, second) = (Hub::new("hub-v2", 1), Hub::new("hub-v2", 2));
    let first = first.claim(&station).await.unwrap();
    let second = second.claim(&station).await.unwrap();
    assert_eq!(first.label, LabelBody::Active);
    assert_eq!(second.label, LabelBody::Queued);
    assert_eq!(second.queue_position, Some(1));

    // The second hub's label, stuck on the first hub.
    scan_when_printed(&station, &first, &second.identity.serial_number).await;
    eventually("the mismatch", || async {
        station
            .shown()
            .contains(&OperatorEvent::Mismatch {
                expected: first.identity.serial_number.clone(),
                scanned: second.identity.serial_number.clone(),
            })
            .then_some(())
    })
    .await;
    assert_eq!(poll(&station, &first).await.label, LabelBody::Active);
    assert_eq!(poll(&station, &second).await.label, LabelBody::Queued);
    let mismatches = station.settled("label-mismatch", 1).await;
    assert_eq!(mismatches.len(), 1);
    assert_eq!(mismatches[0].0, first.claim_id);
    assert_eq!(
        mismatches[0].1["scanned"],
        second.identity.serial_number.as_str()
    );

    // Nobody may say a hub not labelled yet installed: 409, no change.
    let installed = AckBody {
        state: AckState::Installed,
        reason: None,
    };
    let report = station
        .client
        .ack(&first.claim_id, &installed)
        .await
        .unwrap_err();
    assert_eq!(code(&report), (409, ErrorCode::NotLabelled));
    assert_eq!(poll(&station, &first).await.label, LabelBody::Active);

    station.scan(&first.identity.serial_number).await;
    eventually("the first hub labelled", || async {
        (poll(&station, &first).await.label == LabelBody::Labelled).then_some(())
    })
    .await;
    let first_now = poll(&station, &first).await;
    assert_eq!(first_now.queue_position, None);
    assert_eq!(poll(&station, &second).await.label, LabelBody::Active);
    let labelled = station.settled("labelled", 1).await;
    assert_eq!(
        labelled[0].1["scanned"],
        first.identity.serial_number.as_str()
    );

    // Installed: the hub's ack, journaled, the next one still active.
    let acked = station
        .client
        .ack(
            &first.claim_id,
            &AckBody {
                state: AckState::Installed,
                reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(acked.state, StateBody::Installed);
    assert_eq!(station.settled("installed", 1).await.len(), 1);
    assert_eq!(poll(&station, &second).await.label, LabelBody::Active);

    // A hub rejecting what it got before its label: failed, out of the
    // queue.
    let failed = station
        .client
        .ack(
            &second.claim_id,
            &AckBody {
                state: AckState::Failed,
                reason: Some("certificate does not chain".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (failed.state, failed.label, failed.queue_position),
        (StateBody::Failed, LabelBody::Queued, None)
    );
    assert_eq!(
        station.settled("failed", 1).await[0].1["reason"],
        "certificate does not chain"
    );
    station.stop().await;
}

#[tokio::test]
async fn retries_and_a_restarted_station_return_the_same_identity_without_the_platform() {
    let (gateway, resolved) = TestGateway::serve().await;
    let contexts = contexts(&resolved);
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    hook(&config, "label", "10-print", "true");
    let platform_calls = || gateway.0.lock().unwrap().factory_devices.len();
    let station = Station::start(&contexts, config.clone()).await;

    let (labelled, waiting) = (Hub::new("hub-v2", 1), Hub::new("hub-v2", 2));
    let first = labelled.claim(&station).await.unwrap();
    // A lost response: the same key, a CSR rebuilt.
    let retried = station
        .client
        .claim(&labelled.body(&labelled.csr("rebuilt")))
        .await
        .unwrap();
    assert_eq!(retried.claim_id, first.claim_id);
    assert_eq!(retried.identity, first.identity);
    assert_eq!(platform_calls(), 1);

    // Concurrent requests for one new key: one platform call.
    let body = waiting.body(&waiting.csr("claim"));
    let (a, b) = tokio::join!(station.client.claim(&body), station.client.claim(&body));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.identity, b.identity);
    assert_eq!(platform_calls(), 2);

    scan_when_printed(&station, &first, &first.identity.serial_number).await;
    eventually("the first hub labelled", || async {
        (poll(&station, &first).await.label == LabelBody::Labelled).then_some(())
    })
    .await;
    station.stop().await;

    // The station restarts on the same journal.
    let station = Station::start(&contexts, config).await;
    let again = labelled.claim(&station).await.unwrap();
    assert_eq!(again.identity, first.identity);
    assert_eq!(again.label, LabelBody::Labelled, "labelled stays labelled");
    let back = waiting.claim(&station).await.unwrap();
    assert_eq!(back.identity, a.identity);
    assert_eq!(
        back.label,
        LabelBody::Active,
        "back in the queue, and polling"
    );
    assert_eq!(platform_calls(), 2, "no platform call after a restart");
    eventually("its label printed again", || async {
        let attempts: Vec<_> = station
            .journaled("label-active")
            .into_iter()
            .filter(|(id, _)| id == &back.claim_id)
            .map(|(_, entry)| entry["attempt"].as_u64().unwrap())
            .collect();
        (attempts == [1, 2]).then_some(())
    })
    .await;
    station.stop().await;
}

#[tokio::test]
async fn a_silent_active_hub_goes_back_to_the_queue() {
    let (_gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.presence_timeout = Duration::from_millis(500);
    let station = Station::start(&contexts(&resolved), config).await;

    let silent = Hub::new("hub-v2", 1).claim(&station).await.unwrap();
    let polling = Hub::new("hub-v2", 2).claim(&station).await.unwrap();
    assert_eq!(silent.label, LabelBody::Active);

    // Only the second hub keeps polling.
    eventually("the polling hub to take over", || async {
        (poll(&station, &polling).await.label == LabelBody::Active).then_some(())
    })
    .await;
    let lost = station.settled("label-lost", 1).await;
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].0, silent.claim_id);
    assert!(station.shown().iter().any(
        |shown| matches!(shown, OperatorEvent::Lost { hub } if hub.claim_id.as_str() == silent.claim_id)
    ));

    // It comes back: same claim, waiting behind the active one.
    let back = poll(&station, &silent).await;
    assert_eq!(back.label, LabelBody::Queued);
    assert_eq!(back.queue_position, Some(1));
    station.stop().await;
}

#[tokio::test]
async fn a_failing_label_hook_blocks_validation_until_reprinted_or_forced() {
    let (_gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    // The printer jams on each hub's first attempt.
    hook(
        &config,
        "label",
        "10-print",
        "[ \"$REMORA_ATTEMPT\" -ge 2 ] || { echo jammed >&2; exit 1; }",
    );
    let station = Station::start(&contexts(&resolved), config).await;
    let reprinted = Hub::new("hub-v2", 1).claim(&station).await.unwrap();
    let forced = Hub::new("hub-v2", 2).claim(&station).await.unwrap();
    let blocked = |claim: &ClaimStatusBody| {
        let serial = claim.identity.serial_number.clone();
        station.shown().into_iter().any(move |shown| {
            matches!(shown, OperatorEvent::Blocked { hub, reason: LabelBlock::PrintFailed }
                if hub.serial.as_deref() == Some(serial.as_str()))
        })
    };

    // Reprint: the scan is refused until a print succeeds.
    let serial = &reprinted.identity.serial_number;
    scan_when_printed(&station, &reprinted, serial).await;
    eventually("the scan refused", || async {
        blocked(&reprinted).then_some(())
    })
    .await;
    assert_eq!(poll(&station, &reprinted).await.label, LabelBody::Active);
    station.send(OperatorInput::Reprint).await;
    eventually("the reprint", || async {
        (station.hooks_finished(HookEvent::Label, serial) == (2, 1)).then_some(())
    })
    .await;
    station.scan(serial).await;
    eventually("the reprinted hub labelled", || async {
        (poll(&station, &reprinted).await.label == LabelBody::Labelled).then_some(())
    })
    .await;
    eventually("both prints journaled", || async {
        let attempts: Vec<_> = station
            .journaled("hook")
            .into_iter()
            .filter(|(id, _)| id == &reprinted.claim_id)
            .map(|(_, entry)| (entry["attempt"].as_u64(), entry["exit"].as_i64()))
            .collect();
        (attempts == [(Some(1), Some(1)), (Some(2), Some(0))]).then_some(())
    })
    .await;

    // Force: still refused until scanned after `f`.
    let serial = &forced.identity.serial_number;
    scan_when_printed(&station, &forced, serial).await;
    eventually("the scan refused", || async {
        blocked(&forced).then_some(())
    })
    .await;
    station.send(OperatorInput::Force).await;
    station.scan(serial).await;
    eventually("the forced hub labelled", || async {
        (poll(&station, &forced).await.label == LabelBody::Labelled).then_some(())
    })
    .await;
    assert_eq!(station.settled("label-forced", 1).await.len(), 1);
    station.stop().await;
}

#[tokio::test]
async fn skipping_sends_the_active_hub_to_the_end_of_the_queue() {
    let (_gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let station = Station::start(&contexts(&resolved), config(root.path())).await;
    let skipped = Hub::new("hub-v2", 1).claim(&station).await.unwrap();
    let next = Hub::new("hub-v2", 2).claim(&station).await.unwrap();
    station.send(OperatorInput::Skip).await;
    eventually("the next hub active", || async {
        (poll(&station, &next).await.label == LabelBody::Active).then_some(())
    })
    .await;
    let back = poll(&station, &skipped).await;
    assert_eq!(
        (back.label, back.queue_position),
        (LabelBody::Queued, Some(1))
    );
    assert_eq!(station.settled("label-skipped", 1).await.len(), 1);
    station.stop().await;
}

#[tokio::test]
async fn refusals_map_to_the_contract_status_codes() {
    let (gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.max_claims = Some(2);
    hook(&config, "failed", "10-alert", "echo \"$REMORA_BSP_SERIAL\"");
    let station = Station::start(&contexts(&resolved), config).await;

    let hello = station.client.hello().await.unwrap();
    assert_eq!(
        (
            hello.service.as_str(),
            hello.protocol,
            hello.environment.as_str()
        ),
        ("remora-station", 1, resolved.context.name.as_str())
    );

    assert_eq!(
        Hub::new("hub-v3", 1).claim(&station).await.unwrap_err(),
        (403, ErrorCode::UnknownBoard)
    );

    // Only a bad CSR makes a hub regenerate its key.
    let hub = Hub::new("hub-v2", 2);
    let mut garbage = hub.body(b"not a csr");
    let report = station.client.claim(&garbage).await.unwrap_err();
    let (status, code_of, message, _) = refusal(&report);
    assert_eq!((status, code_of), (400, Some(ErrorCode::InvalidCsr)));
    assert!(
        message.contains("invalid certificate signing request"),
        "{message}"
    );
    garbage.csr = "%%% not base64".into();
    assert_eq!(
        code(&station.client.claim(&garbage).await.unwrap_err()),
        (400, ErrorCode::InvalidRequest)
    );
    let mut boardless = hub.body(&hub.csr("x"));
    boardless.hardware.board = String::new();
    assert_eq!(
        code(&station.client.claim(&boardless).await.unwrap_err()),
        (400, ErrorCode::InvalidRequest)
    );

    // An explicit device name the platform already has.
    gateway
        .0
        .lock()
        .unwrap()
        .factory_devices
        .insert("c3d203".into(), 1);
    let duplicate = Hub::new("hub-v1", 3);
    assert_eq!(
        duplicate.claim(&station).await.unwrap_err(),
        (409, ErrorCode::AlreadyExists)
    );
    let failed = station.settled("failed", 1).await;
    assert_eq!(failed.len(), 1);
    assert!(failed[0].1["reason"].as_str().unwrap().contains("c3d203"));
    eventually("failed.d to run", || async {
        station
            .journaled("hook")
            .iter()
            .any(|(_, entry)| entry["hook"] == "failed.d/10-alert" && entry["stdout"] == "c3d203\n")
            .then_some(())
    })
    .await;
    // A hub without the field its board's device-name needs.
    let mut nameless = Hub::new("hub-v1", 4);
    nameless.hardware.bsp_serial = None;
    assert_eq!(
        nameless.claim(&station).await.unwrap_err(),
        (400, ErrorCode::InvalidHardware)
    );

    // The quota: two identities, then refusals -- but a retry still gets
    // its identity back.
    let first = hub.claim(&station).await.unwrap();
    Hub::new("hub-v1", 5).claim(&station).await.unwrap();
    assert_eq!(
        Hub::new("hub-v2", 6).claim(&station).await.unwrap_err(),
        (403, ErrorCode::QuotaExceeded)
    );
    assert_eq!(hub.claim(&station).await.unwrap().identity, first.identity);

    let report = station.client.status("0123").await.unwrap_err();
    assert_eq!(code(&report), (404, ErrorCode::UnknownClaim));
    station.stop().await;

    // Without a quota from here on.
    let root = tempfile::tempdir().unwrap();
    let station = Station::start(&contexts(&resolved), crate::config(root.path())).await;

    // Each field past its limit, or able to forge the operator's console:
    // invalid-hardware, naming the field.
    let long = "a".repeat(65);
    type Change = fn(&mut HardwareBody, &mut ImageBody, &str);
    let refused: [(&str, Change); 9] = [
        ("hardware.temp_hostname", |h, _, _| {
            h.temp_hostname = "\x1b[2J1H7Z".into()
        }),
        ("hardware.temp_hostname", |h, _, long| {
            h.temp_hostname = long.into()
        }),
        ("hardware.eth_mac", |h, _, long| {
            h.eth_mac = Some(long.into())
        }),
        ("hardware.bsp_serial", |h, _, _| {
            h.bsp_serial = Some("c3d2 $(reboot)".into())
        }),
        ("hardware.machine_id", |h, _, long| {
            h.machine_id = Some(long.into())
        }),
        ("hardware.macs.wlan0", |h, _, _| {
            h.macs.insert("wlan0".into(), "aa\nbb".into());
        }),
        ("hardware.macs", |h, _, _| {
            h.macs.insert("a-very-long-interface".into(), "aa".into());
        }),
        ("more than 32 MACs", |h, _, _| {
            h.macs = (0..33).map(|n| (format!("eth{n}"), "aa".into())).collect()
        }),
        ("image.version", |_, i, long| i.version = Some(long.into())),
    ];
    for (n, (field, change)) in refused.into_iter().enumerate() {
        let hub = Hub::new("hub-v2", 10 + n);
        let mut body = hub.body(&hub.csr("x"));
        change(&mut body.hardware, &mut body.image, &long);
        let report = station.client.claim(&body).await.unwrap_err();
        let (status, code_of, message, _) = refusal(&report);
        assert_eq!((status, code_of), (400, Some(ErrorCode::InvalidHardware)));
        assert!(message.contains(field), "{field}: {message}");
    }

    // What real hubs send, unknown values as "" included.
    let real = Hub::new("hub-v2", 50);
    let mut body = real.body(&real.csr("x"));
    body.hardware.temp_hostname = "525400123456".into();
    body.hardware.eth_mac = Some(String::new());
    body.hardware.bsp_serial = Some(String::new());
    body.hardware.machine_id = Some("1abf02e5bed34d63a097f3f38f0f6408".into());
    body.hardware.macs = BTreeMap::from([
        ("bat0".into(), "aabbccddeeff".into()),
        ("wlan0".into(), "aa:bb:cc:dd:ee:ff".into()),
        ("usb0".into(), String::new()),
    ]);
    body.image.version = Some("local-748efa3".into());
    body.image.compatible = Some("virtual".into());
    station.client.claim(&body).await.unwrap();
    body.image = ImageBody {
        version: Some(String::new()),
        compatible: Some(String::new()),
    };
    station.client.claim(&body).await.unwrap();

    // A body no device sends.
    let mut huge = Hub::new("hub-v2", 8).body(&Hub::new("hub-v2", 8).csr("x"));
    huge.hardware.macs = BTreeMap::from([("wlan0".into(), "x".repeat(BODY_LIMIT))]);
    assert_eq!(
        code(&station.client.claim(&huge).await.unwrap_err()),
        (413, ErrorCode::PayloadTooLarge)
    );

    // Credentials revoked while the station runs: retry later.
    gateway.0.lock().unwrap().revoked = true;
    let report = Hub::new("hub-v2", 9).claim_report(&station).await;
    let (status, code_of, _, retry_after) = refusal(&report);
    assert_eq!(
        (status, code_of, retry_after),
        (503, Some(ErrorCode::Unavailable), Some(5))
    );
    station.stop().await;
}

#[tokio::test]
async fn issued_hooks_get_the_claim_a_hub_sent() {
    let (_gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    hook(
        &config,
        "issued",
        "10-register",
        "echo \"$1 $REMORA_SERIAL $REMORA_BOARD $REMORA_TEMP_HOSTNAME $REMORA_ETH_MAC \
         $REMORA_BSP_SERIAL $REMORA_MACHINE_ID $REMORA_IMAGE_VERSION $REMORA_ATTEMPT\"",
    );
    let station = Station::start(&contexts(&resolved), config).await;
    let hub = Hub::new("hub-v2", 1);
    let claim = hub.claim(&station).await.unwrap();
    let hooks = station.settled("hook", 1).await;
    assert_eq!(hooks[0].0, claim.claim_id);
    assert_eq!(
        hooks[0].1["stdout"],
        format!(
            "issued {} hub-v2 e2b4a1c09f01 e2:b4:a1:c0:9f:01 c3d201 {:032} 1.4.0 1\n",
            claim.identity.serial_number, 1
        )
    );
    station.stop().await;
}
