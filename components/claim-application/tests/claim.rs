//! The claim use case with the real factory use case and local adapters
//! (the device's key, its `remora-factory.yaml`); the station is a scripted
//! stub, being the network port -- the real one, over HTTP, is exercised
//! end to end by `remora-etcher`'s `station` test.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use error_stack::Report;
use remora_claim::{
    adapter::station::{self, StationClientAdapter, StationClientAdapterService},
    application::{ClaimServiceInterface, Error},
    model::ClaimPlan,
};
use remora_claim_application::ClaimControllerImpl;
use remora_context::{
    adapter::{
        credentials::CredentialStoreAdapterService, platform::PlatformSessionAdapterService,
        store::ContextStoreAdapterService,
    },
    application::ContextService,
};
use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};
use remora_context_adapter_grpc::PlatformSessionAdapterImpl;
use remora_context_application::ContextControllerImpl;
use remora_factory::{
    adapter::{
        credential::CredentialWriterAdapterService,
        key::DeviceKeyAdapterService,
        provisioning::{FactoryProvisioningAdapterService, ProvisionedIdentity},
    },
    application::FactoryService,
};
use remora_factory_adapter_grpc::FactoryGatewayAdapterImpl;
use remora_factory_adapter_local::{CredentialWriterAdapterImpl, DeviceKeyAdapterImpl};
use remora_factory_application::FactoryControllerImpl;
use remora_progress::OperationContext;
use remora_station::model::{
    Ack, ClaimId, ClaimRequest, ClaimState, ClaimStatus, HardwareInfo, Hello, ImageInfo, LabelState,
};
use tokio_stream::StreamExt;

/// A station answering from a script: each claim and status call takes
/// the next answer; what was asked is kept.
#[derive(Default)]
struct ScriptedStation {
    claims: Mutex<VecDeque<station::Result<ClaimStatus>>>,
    statuses: Mutex<VecDeque<ClaimStatus>>,
    acks: Arc<Mutex<Vec<Ack>>>,
}

#[async_trait::async_trait]
impl StationClientAdapter for ScriptedStation {
    async fn hello(&self, _: &str) -> station::Result<Hello> {
        Ok(Hello {
            environment: "test".into(),
        })
    }

    async fn claim(&self, _: &str, _: &ClaimRequest) -> station::Result<ClaimStatus> {
        self.claims
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted claim")
    }

    async fn status(&self, _: &str, _: &ClaimId) -> station::Result<ClaimStatus> {
        Ok(self
            .statuses
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted status"))
    }

    async fn ack(&self, _: &str, claim: &ClaimId, ack: &Ack) -> station::Result<ClaimStatus> {
        self.acks.lock().unwrap().push(ack.clone());
        Ok(status(
            claim.as_str(),
            ClaimState::Installed,
            LabelState::Labelled,
        ))
    }
}

fn status(claim: &str, state: ClaimState, label: LabelState) -> ClaimStatus {
    ClaimStatus {
        claim_id: ClaimId::new(claim),
        state,
        label,
        queue_position: (label == LabelState::Queued).then_some(1),
        identity: ProvisionedIdentity {
            serial_number: "1H7Z".into(),
            factory_device_name: "urn:test:factory-device:1H7Z".into(),
            certificate_der: b"idevid".to_vec(),
            certificate_authority_der: b"factory ca".to_vec(),
            server_certificate_authority_der: b"server ca".to_vec(),
            key_id: "urn:test:certificate-1H7Z-1".into(),
            access_url: "https://access.test/access/v1".into(),
        },
    }
}

/// The factory use case as wired, its platform never called here.
fn factory() -> FactoryService {
    let root = tempfile::tempdir().unwrap().keep();
    let contexts = ContextService::new(ContextControllerImpl::new(
        ContextStoreAdapterService::new(FileContextStoreImpl::new(&root)),
        CredentialStoreAdapterService::new(FileCredentialStoreImpl::new(&root)),
        PlatformSessionAdapterService::new(PlatformSessionAdapterImpl),
    ));
    FactoryService::new(FactoryControllerImpl::new(
        contexts,
        DeviceKeyAdapterService::new(DeviceKeyAdapterImpl),
        FactoryProvisioningAdapterService::new(FactoryGatewayAdapterImpl),
        CredentialWriterAdapterService::new(CredentialWriterAdapterImpl),
    ))
}

fn plan(output: &std::path::Path) -> ClaimPlan {
    ClaimPlan {
        station: "http://station.test:8484".into(),
        hardware: HardwareInfo {
            board: "hub-virtual".into(),
            temp_hostname: "525400123456".into(),
            ..Default::default()
        },
        image: ImageInfo::default(),
        output: output.to_path_buf(),
        poll_interval: Duration::from_millis(10),
    }
}

#[tokio::test]
async fn waits_out_an_unavailable_station_and_writes_once_labelled() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("remora-factory.yaml");
    let acks = Arc::new(Mutex::new(Vec::new()));
    let station = ScriptedStation {
        claims: Mutex::new(VecDeque::from([
            Err(Report::new(station::Error::Unavailable {
                retry_after: Some(Duration::ZERO),
            })),
            Err(Report::new(station::Error::Unreachable("station".into()))),
            Ok(status("c1", ClaimState::Issued, LabelState::Queued)),
        ])),
        statuses: Mutex::new(VecDeque::from([
            status("c1", ClaimState::Issued, LabelState::Active),
            status("c1", ClaimState::Issued, LabelState::Labelled),
        ])),
        acks: acks.clone(),
    };
    let claims = ClaimControllerImpl::new(StationClientAdapterService::new(station), factory());

    let (sink, mut events) = remora_progress::channel();
    let ctx = OperationContext::new(sink, Default::default());
    let outcome = claims.claim(&plan(&output), &ctx).await.unwrap();
    assert_eq!(outcome.serial, "1H7Z");
    assert!(outcome.acknowledged);
    assert_eq!(*acks.lock().unwrap(), [Ack::Installed]);
    let yaml = std::fs::read_to_string(&output).unwrap();
    assert!(yaml.contains("BEGIN EC PRIVATE KEY"), "{yaml}");

    // The LED as a hub's would show it, retries logged on the way.
    drop(ctx);
    let mut shown = Vec::new();
    while let Some(event) = events.next().await {
        shown.push(format!("{event:?}"));
    }
    let shown = shown.join("\n");
    assert!(shown.contains("retrying"), "{shown}");
    assert!(shown.contains("LED: queued (position 1)"), "{shown}");
    assert!(shown.contains("LED: steady -- label me: 1H7Z"), "{shown}");
}

#[tokio::test]
async fn gives_up_on_a_refusal_and_on_a_claim_failed_while_queued() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("remora-factory.yaml");
    let refused = ScriptedStation {
        claims: Mutex::new(VecDeque::from([Err(Report::new(station::Error::Refused))])),
        ..Default::default()
    };
    let claims = ClaimControllerImpl::new(StationClientAdapterService::new(refused), factory());
    let report = claims
        .claim(&plan(&output), &OperationContext::noop())
        .await
        .unwrap_err();
    assert!(matches!(report.current_context(), Error::Refused));

    let failed = ScriptedStation {
        claims: Mutex::new(VecDeque::from([Ok(status(
            "c1",
            ClaimState::Issued,
            LabelState::Queued,
        ))])),
        statuses: Mutex::new(VecDeque::from([status(
            "c1",
            ClaimState::Failed,
            LabelState::Queued,
        )])),
        ..Default::default()
    };
    let claims = ClaimControllerImpl::new(StationClientAdapterService::new(failed), factory());
    let report = claims
        .claim(&plan(&output), &OperationContext::noop())
        .await
        .unwrap_err();
    assert!(matches!(
        report.current_context(),
        Error::Abandoned("failed")
    ));
    assert!(!output.exists(), "nothing written without a label");
}
