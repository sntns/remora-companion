//! The station's use case through its application port, wired as `serve`
//! wires it -- the JSONL journal, the hook runner with real scripts, the
//! factory and context verticals over the in-process fake gateway -- with
//! the devices' requests made straight on `StationService` (the HTTP
//! projection has its own tests in the transport crate).

#![cfg(unix)]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant},
};

use error_stack::Report;
use rcgen::{CertificateParams, KeyPair};
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
        hooks::HookRunnerAdapterService,
        journal::{self, JournalAdapter, JournalAdapterService, JournalEntry, JournalEvent},
        operator::{OperatorAdapter, OperatorAdapterService, OperatorEvent, OperatorInput},
    },
    application::{Error, StationService},
    model::{
        Ack, BoardPolicy, ClaimId, ClaimRequest, ClaimState, ClaimStatus, ConfirmMode, FieldError,
        HardwareInfo, ImageInfo, LabelState, StationConfig,
    },
};
use remora_station_adapter_hooks_process::ProcessHookRunnerImpl;
use remora_station_adapter_jsonl::JsonlJournalImpl;
use remora_station_application::StationControllerImpl;

/// What the console was shown, kept for the test to inspect: the
/// terminal's own console can't be read back, and its input reaches the
/// station through `handle` anyway.
#[derive(Clone, Default)]
struct RecordingOperator(Arc<StdMutex<Vec<OperatorEvent>>>);

impl OperatorAdapter for RecordingOperator {
    fn show(&self, event: &OperatorEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

/// The real JSONL journal, whose `issued` appends fail while `failing` is
/// set: a full or read-only disk, at the worst moment.
#[derive(Clone)]
struct FlakyJournal {
    inner: JsonlJournalImpl,
    failing: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl JournalAdapter for FlakyJournal {
    async fn load(&self, path: &Path) -> journal::Result<Vec<JournalEntry>> {
        self.inner.load(path).await
    }

    async fn append(&self, path: &Path, entry: &JournalEntry) -> journal::Result<()> {
        if self.failing.load(Ordering::SeqCst) && matches!(entry.event, JournalEvent::Issued { .. })
        {
            return Err(Report::new(journal::Error::Write(path.to_path_buf())));
        }
        self.inner.append(path, entry).await
    }
}

/// The station laptop's contexts: one, logged in to `resolved`'s gateway.
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
        boards: BTreeMap::from([(
            "hub-v2".to_string(),
            BoardPolicy::SerialNumberPolicy("hubs-v2".into()),
        )]),
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
    service: StationService,
    shown: RecordingOperator,
    journal: PathBuf,
}

impl Station {
    async fn start(contexts: &ContextService, config: StationConfig) -> Self {
        let journal = JournalAdapterService::new(JsonlJournalImpl::new());
        Self::start_with(contexts, config, journal).await
    }

    async fn start_with(
        contexts: &ContextService,
        config: StationConfig,
        journal: JournalAdapterService,
    ) -> Self {
        std::fs::create_dir_all(&config.hooks).unwrap();
        let path = config.journal.clone();
        let shown = RecordingOperator::default();
        let service = StationService::new(StationControllerImpl::new(
            contexts.clone(),
            factory(contexts),
            journal,
            HookRunnerAdapterService::new(ProcessHookRunnerImpl),
            OperatorAdapterService::new(shown.clone()),
        ));
        service.start(config, None).await.unwrap();
        Self {
            service,
            shown,
            journal: path,
        }
    }

    fn shown(&self) -> Vec<OperatorEvent> {
        self.shown.0.lock().unwrap().clone()
    }

    /// `event` entries of the journal, as `(claim_id, entry)`.
    fn journaled(&self, event: &str) -> Vec<(String, serde_json::Value)> {
        std::fs::read_to_string(&self.journal)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|entry| entry["event"] == event)
            .map(|entry| (entry["claim_id"].as_str().unwrap().to_string(), entry))
            .collect()
    }

    /// Labels `claim`, the active one, once its label has printed.
    async fn label(&self, claim: &ClaimStatus) {
        let serial = claim.identity.serial_number.clone();
        eventually("its label printed", || {
            self.shown().iter().any(|shown| {
                matches!(shown, OperatorEvent::Hook { hub, .. }
                    if hub.serial.as_deref() == Some(serial.as_str()))
            })
        })
        .await;
        self.service
            .handle(OperatorInput::Scan(serial.clone()))
            .await
            .unwrap();
        let status = self.service.status(&claim.claim_id).await.unwrap();
        assert_eq!(status.label, LabelState::Labelled);
    }
}

/// Polls `check` until it holds, for up to 15 s.
async fn eventually(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A device: its key, kept across retries, and what it says about itself.
struct Hub {
    key: KeyPair,
    hardware: HardwareInfo,
}

impl Hub {
    fn new(n: usize) -> Self {
        Self {
            key: KeyPair::generate().unwrap(),
            hardware: HardwareInfo {
                board: "hub-v2".into(),
                temp_hostname: format!("e2b4a1c09f{n:02}"),
                eth_mac: Some(format!("e2:b4:a1:c0:9f:{n:02}")),
                bsp_serial: Some(format!("c3d2{n:02}")),
                machine_id: Some(format!("{n:032}")),
                macs: BTreeMap::new(),
            },
        }
    }

    fn request(&self) -> ClaimRequest {
        ClaimRequest {
            csr_der: CertificateParams::default()
                .serialize_request(&self.key)
                .unwrap()
                .der()
                .to_vec(),
            hardware: self.hardware.clone(),
            image: ImageInfo {
                version: Some("1.4.0".into()),
                compatible: Some("v2".into()),
            },
        }
    }

    async fn claim(&self, station: &Station) -> Result<ClaimStatus, Report<Error>> {
        station.service.submit(self.request()).await
    }
}

fn platform_calls(gateway: &TestGateway) -> usize {
    gateway.0.lock().unwrap().factory_devices.len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_is_refused_until_the_label_is_validated() {
    let (_gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    hook(&config, "label", "10-print", "true");
    hook(
        &config,
        "installed",
        "10-close",
        "echo \"$REMORA_EVENT $REMORA_SERIAL\"",
    );
    let station = Station::start(&contexts(&resolved), config).await;

    let claim = Hub::new(1).claim(&station).await.unwrap();
    assert_eq!(claim.label, LabelState::Active);
    let id = &claim.claim_id;

    // Anyone knowing the claim id, before its label is scanned: nothing.
    let report = station.service.ack(id, Ack::Installed).await.unwrap_err();
    assert!(matches!(report.current_context(), Error::NotLabelled(_)));
    let status = station.service.status(id).await.unwrap();
    assert_eq!(
        (status.state, status.label),
        (ClaimState::Issued, LabelState::Active)
    );

    station.label(&claim).await;
    let installed = station.service.ack(id, Ack::Installed).await.unwrap();
    assert_eq!(
        (installed.state, installed.label, installed.queue_position),
        (ClaimState::Installed, LabelState::Labelled, None)
    );
    // A lost response: the same answer, journaled once.
    let again = station.service.ack(id, Ack::Installed).await.unwrap();
    assert_eq!(again.state, ClaimState::Installed);
    let serial = &claim.identity.serial_number;
    eventually("installed.d to run", || {
        station
            .journaled("hook")
            .iter()
            .any(|(_, entry)| entry["stdout"] == format!("installed {serial}\n"))
    })
    .await;
    assert_eq!(station.journaled("installed").len(), 1);

    let report = station
        .service
        .ack(&ClaimId::new("0123"), Ack::Installed)
        .await
        .unwrap_err();
    assert!(matches!(report.current_context(), Error::UnknownClaim(_)));
    station.service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_identity_leaves_the_queue_whenever_it_is_acked() {
    let (_gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    hook(&config, "label", "10-print", "true");
    hook(&config, "failed", "10-alert", "echo \"$REMORA_SERIAL\"");
    let station = Station::start(&contexts(&resolved), config).await;
    let rejecting = Hub::new(1).claim(&station).await.unwrap();
    let queued = Hub::new(2).claim(&station).await.unwrap();
    let last = Hub::new(3).claim(&station).await.unwrap();
    assert_eq!(rejecting.label, LabelState::Active);

    // The active hub rejects its identity: out of the queue, the next one
    // active, failed.d told -- the reason cleaned for the journal.
    let failed = Ack::Failed {
        reason: "bad\x1b[2Jchain\n of trust".into(),
    };
    let status = station
        .service
        .ack(&rejecting.claim_id, failed)
        .await
        .unwrap();
    assert_eq!(
        (status.state, status.label, status.queue_position),
        (ClaimState::Failed, LabelState::Queued, None)
    );
    let next = station.service.status(&queued.claim_id).await.unwrap();
    assert_eq!(next.label, LabelState::Active);
    eventually("the failure journaled and failed.d run", || {
        let failed = station.journaled("failed");
        let alerted = station.journaled("hook").iter().any(|(_, entry)| {
            entry["hook"] == "failed.d/10-alert"
                && entry["stdout"] == format!("{}\n", rejecting.identity.serial_number)
        });
        alerted
            && failed
                .first()
                .is_some_and(|(_, entry)| entry["reason"] == "bad [2Jchain of trust")
    })
    .await;
    // Polling keeps it where it is: never activated again.
    let status = station.service.status(&rejecting.claim_id).await.unwrap();
    assert_eq!(status.state, ClaimState::Failed);

    // A queued one, likewise: the queue closes up.
    let failed = Ack::Failed { reason: "x".into() };
    station.service.ack(&last.claim_id, failed).await.unwrap();
    let status = station.service.status(&last.claim_id).await.unwrap();
    assert_eq!(
        (status.label, status.queue_position),
        (LabelState::Queued, None)
    );
    assert_eq!(
        station
            .service
            .status(&queued.claim_id)
            .await
            .unwrap()
            .label,
        LabelState::Active
    );

    // One labelled, failing, then installing after all.
    station.label(&next).await;
    let failed = Ack::Failed { reason: "x".into() };
    let status = station.service.ack(&next.claim_id, failed).await.unwrap();
    assert_eq!(
        (status.state, status.label),
        (ClaimState::Failed, LabelState::Labelled)
    );
    let status = station
        .service
        .ack(&next.claim_id, Ack::Installed)
        .await
        .unwrap();
    assert_eq!(status.state, ClaimState::Installed);
    station.service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_quota_counts_this_run_only() {
    let (gateway, resolved) = TestGateway::serve().await;
    let contexts = contexts(&resolved);
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.max_claims = Some(1);
    let station = Station::start(&contexts, config.clone()).await;
    let (first, second, third) = (Hub::new(1), Hub::new(2), Hub::new(3));
    let issued = first.claim(&station).await.unwrap();
    let report = second.claim(&station).await.unwrap_err();
    assert!(matches!(report.current_context(), Error::QuotaReached(1)));
    station.service.shutdown().await.unwrap();

    // A new run on the same journal: what it restored doesn't count.
    let station = Station::start(&contexts, config).await;
    assert_eq!(
        first.claim(&station).await.unwrap().identity,
        issued.identity
    );
    second.claim(&station).await.unwrap();
    let report = third.claim(&station).await.unwrap_err();
    assert!(matches!(report.current_context(), Error::QuotaReached(1)));
    assert_eq!(platform_calls(&gateway), 2);
    station.service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_the_hooks_under_way_and_journals_them() {
    let (_gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    hook(&config, "issued", "10-slow", "sleep 0.5; echo registered");
    let station = Station::start(&contexts(&resolved), config).await;
    Hub::new(1).claim(&station).await.unwrap();

    let started = Instant::now();
    station.service.shutdown().await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(300));
    let hooks = station.journaled("hook");
    assert_eq!(hooks.len(), 1, "{hooks:?}");
    assert_eq!(hooks[0].1["hook"], "issued.d/10-slow");
    assert_eq!(hooks[0].1["stdout"], "registered\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_the_hooks_still_going_past_the_hook_timeout() {
    let (_gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.hook_timeout = Duration::from_secs(1);
    // Each within the timeout, both together past it.
    let marker = |name: &str| root.path().join(name);
    hook(
        &config,
        "issued",
        "10-first",
        &format!("sleep 0.7; touch {}", marker("first").display()),
    );
    hook(
        &config,
        "issued",
        "20-second",
        &format!("sleep 0.7; touch {}", marker("second").display()),
    );
    let station = Station::start(&contexts(&resolved), config).await;
    Hub::new(1).claim(&station).await.unwrap();

    let started = Instant::now();
    station.service.shutdown().await.unwrap();
    assert!(started.elapsed() < Duration::from_millis(1500));
    assert!(station.shown().iter().any(|shown| matches!(
        shown,
        OperatorEvent::Warning(message) if message.contains("stopped 1 hook run")
    )));
    // The second script was killed, not orphaned: it never finishes.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(marker("first").exists());
    assert!(!marker("second").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_identity_the_journal_cannot_record_is_held_back_never_issued_twice() {
    let (gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let failing = Arc::new(AtomicBool::new(true));
    let journal = JournalAdapterService::new(FlakyJournal {
        inner: JsonlJournalImpl::new(),
        failing: failing.clone(),
    });
    let station = Station::start_with(&contexts(&resolved), config(root.path()), journal).await;
    let hub = Hub::new(1);

    for _ in 0..2 {
        let report = hub.claim(&station).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Journal(_)));
    }
    assert_eq!(platform_calls(&gateway), 1, "a retry never re-issues");
    assert!(station.shown().iter().any(
        |shown| matches!(shown, OperatorEvent::Alert(message) if message.contains("held back"))
    ));
    assert!(!station
        .shown()
        .iter()
        .any(|shown| matches!(shown, OperatorEvent::Issued { .. })));

    // The disk is back: the next retry gets the identity issued before.
    failing.store(false, Ordering::SeqCst);
    let claim = hub.claim(&station).await.unwrap();
    assert_eq!(claim.identity.serial_number, "hubs-v2-1");
    assert_eq!(claim.label, LabelState::Active);
    assert_eq!(platform_calls(&gateway), 1);
    assert_eq!(station.journaled("issued").len(), 1);
    station.service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_that_could_forge_the_console_is_refused_before_anything() {
    let (gateway, resolved) = TestGateway::serve().await;
    let root = tempfile::tempdir().unwrap();
    let station = Station::start(&contexts(&resolved), config(root.path())).await;
    let mut hub = Hub::new(1);
    hub.hardware.temp_hostname = "\x1b[2J\x1b[1;1HLabel 1H7Z".into();

    let report = hub.claim(&station).await.unwrap_err();
    assert!(matches!(report.current_context(), Error::InvalidHardware));
    assert_eq!(
        report.downcast_ref::<FieldError>(),
        Some(&FieldError::Charset(
            "hardware.temp_hostname".into(),
            "letters, digits, ':', '.', '_', '+' and '-'"
        ))
    );
    assert_eq!(platform_calls(&gateway), 0);
    assert!(station.journaled("received").is_empty());
    station.service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_station_logged_out_never_starts_and_revoked_credentials_ask_for_a_retry() {
    let (gateway, resolved) = TestGateway::serve().await;
    let contexts = contexts(&resolved);
    let root = tempfile::tempdir().unwrap();

    gateway.0.lock().unwrap().revoked = true;
    let service = StationService::new(StationControllerImpl::new(
        contexts.clone(),
        factory(&contexts),
        JournalAdapterService::new(JsonlJournalImpl::new()),
        HookRunnerAdapterService::new(ProcessHookRunnerImpl),
        OperatorAdapterService::new(RecordingOperator::default()),
    ));
    std::fs::create_dir_all(root.path().join("station.d")).unwrap();
    let report = service.start(config(root.path()), None).await.unwrap_err();
    assert!(matches!(report.current_context(), Error::Context(_)));

    // Revoked once it runs: the hub is told to retry, the operator alerted.
    gateway.0.lock().unwrap().revoked = false;
    let station = Station::start(&contexts, config(root.path())).await;
    gateway.0.lock().unwrap().revoked = true;
    let report = Hub::new(1).claim(&station).await.unwrap_err();
    assert!(matches!(report.current_context(), Error::Unavailable));
    assert!(station
        .shown()
        .iter()
        .any(|shown| matches!(shown, OperatorEvent::Alert(_))));
    station.service.shutdown().await.unwrap();
}
