use remora_context::model::ContextOverride;

use super::error::Result;
use crate::model::{Ack, ClaimId, ClaimRequest, ClaimStatus, Hello, StationConfig, StationSummary};

/// The station vertical's application-facing port: a provisioning station
/// on a workshop network, issuing identities to devices that booted a
/// cloned image, and queueing them for their label one at a time.
///
/// Issuing is automatic and parallel -- any configured board is approved;
/// labelling is sequential: exactly one device is `Active` (its LED
/// steady) until the operator scans the label stuck on it, which must read
/// its serial. That scan is the commit point: a device writes its identity
/// only once `Labelled`. Every transition is journaled, and a claim is
/// named by its key, so any interruption (a lost response, a device or the
/// station restarting) resumes on the same claim without a second serial.
#[async_trait::async_trait]
pub trait StationServiceInterface: Send + Sync {
    /// Checks `config` and the context `over` selects, reloads the journal,
    /// and makes the station ready to serve: before it listens, so a
    /// misconfigured station never answers a device. Once only.
    async fn start(
        &self,
        config: StationConfig,
        over: Option<ContextOverride>,
    ) -> Result<StationSummary>;

    async fn hello(&self) -> Result<Hello>;

    /// A device's claim: the identity already issued for its key, or a
    /// new one from the platform, queued for its label. Concurrent
    /// requests for one key make one platform call.
    async fn submit(&self, request: ClaimRequest) -> Result<ClaimStatus>;

    /// Where a claim stands. Polling it is also how a device says it is
    /// still there: the active one is requeued once it stops.
    async fn status(&self, claim: &ClaimId) -> Result<ClaimStatus>;

    /// The device has written its identity, or rejected it.
    async fn ack(&self, claim: &ClaimId, ack: Ack) -> Result<ClaimStatus>;

    /// Runs the labelling console until the operator quits: scans, the
    /// `r`/`s`/`f` commands, and requeueing an active device gone silent.
    async fn operate(&self) -> Result<()>;
}

/// Injectable handle to whatever `StationServiceInterface` implementation
/// was wired at startup (normally `remora-station-application`'s
/// `StationControllerImpl`).
#[derive(Clone)]
pub struct StationService(busybody::Service<Box<dyn StationServiceInterface>>);

impl StationService {
    pub fn new<T: StationServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for StationService {
    type Target = busybody::Service<Box<dyn StationServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
