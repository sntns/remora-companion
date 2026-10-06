use remora_context::model::ContextOverride;

use super::error::Result;
use crate::{
    adapter::operator::OperatorInput,
    model::{Ack, ClaimId, ClaimRequest, ClaimStatus, Hello, StationConfig, StationSummary},
};

/// The station vertical's application-facing port: a provisioning station
/// on a workshop network, issuing identities to devices that booted a
/// cloned image, and queueing them for their label one at a time.
///
/// Issuing is automatic and parallel -- any configured board is approved;
/// labelling is sequential: exactly one device is `Active` (its LED
/// steady) until the operator scans the label stuck on it, which must read
/// its serial. That scan is the commit point: a device writes its identity
/// only once `Labelled`, and only then may it ack. Every transition is
/// journaled, and a claim is named by its key, so any interruption (a lost
/// response, a device or the station restarting) resumes on the same
/// claim without a second serial.
///
/// The labelling console is driven from outside: whoever reads the
/// operator's input hands it to `handle`, and calls `tick` every
/// `StationSummary::tick`.
#[async_trait::async_trait]
pub trait StationServiceInterface: Send + Sync {
    /// Checks `config` and that the context `over` selects is logged in
    /// (asking the platform), reloads the journal, and makes the station
    /// ready to serve: before it listens, so a misconfigured station never
    /// answers a device. Once only.
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

    /// The device has written its identity -- refused
    /// (`Error::NotLabelled`) until its label was validated -- or rejected
    /// it, at any time: then it leaves the labelling queue.
    async fn ack(&self, claim: &ClaimId, ack: Ack) -> Result<ClaimStatus>;

    /// One scan or command of the operator's, for the active device.
    async fn handle(&self, input: OperatorInput) -> Result<()>;

    /// The labelling queue's clock: requeues an active device gone silent,
    /// and activates the next one still polling.
    async fn tick(&self) -> Result<()>;

    /// Stops cleanly, once nothing reaches `submit` or `ack` anymore: no
    /// hook starts from now on, those running get `hook-timeout` to finish
    /// (their outcomes journaled) and are killed past it, and every
    /// journal entry is on disk when this returns.
    async fn shutdown(&self) -> Result<()>;
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
