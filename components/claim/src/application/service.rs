use remora_progress::OperationContext;

use super::error::Result;
use crate::model::{ClaimOutcome, ClaimPlan};

/// The claim vertical's application-facing port: a device getting its
/// factory identity from a provisioning station, as a hub does at first
/// boot -- a key of its own, a claim retried while the station or its
/// platform is unavailable, the labelling queue followed (its LED), and,
/// once labelled, its `remora-factory.yaml` written and acknowledged.
#[async_trait::async_trait]
pub trait ClaimServiceInterface: Send + Sync {
    /// Reports each step (the LED's state among them) as a phase on
    /// `ctx.sink`, retries as logs, and stops between steps once
    /// `ctx.cancel` fires.
    async fn claim(&self, plan: &ClaimPlan, ctx: &OperationContext) -> Result<ClaimOutcome>;
}

/// Injectable handle to whatever `ClaimServiceInterface` implementation
/// was wired at startup (normally `remora-claim-application`'s
/// `ClaimControllerImpl`).
#[derive(Clone)]
pub struct ClaimService(busybody::Service<Box<dyn ClaimServiceInterface>>);

impl ClaimService {
    pub fn new<T: ClaimServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for ClaimService {
    type Target = busybody::Service<Box<dyn ClaimServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
