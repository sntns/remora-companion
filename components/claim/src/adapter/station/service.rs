use remora_station::model::{Ack, ClaimId, ClaimRequest, ClaimStatus, Hello};

use super::error::Result;

/// DI seam for `remora-claim-application`: talking to a provisioning
/// station. Named by its base URL on each call, like a path for a file
/// adapter: which station is the command line's to say.
#[async_trait::async_trait]
pub trait StationClientAdapter: Send + Sync {
    /// The station at `station`, if it is one speaking this protocol.
    async fn hello(&self, station: &str) -> Result<Hello>;
    async fn claim(&self, station: &str, request: &ClaimRequest) -> Result<ClaimStatus>;
    async fn status(&self, station: &str, claim: &ClaimId) -> Result<ClaimStatus>;
    async fn ack(&self, station: &str, claim: &ClaimId, ack: &Ack) -> Result<ClaimStatus>;
}

/// Injectable handle to whatever `StationClientAdapter` was wired at
/// startup.
#[derive(Clone)]
pub struct StationClientAdapterService(busybody::Service<Box<dyn StationClientAdapter>>);

impl StationClientAdapterService {
    pub fn new<T: StationClientAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for StationClientAdapterService {
    type Target = busybody::Service<Box<dyn StationClientAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
