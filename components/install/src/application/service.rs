use remora_progress::OperationContext;

use super::error::Result;
use crate::model::{InstallOutcome, InstallRequest};

/// The install vertical's application-facing port: an OS image (RAUC
/// bundle) onto a device over its remora channel, as one operation.
///
/// It reports through `ctx` one phase per step -- downloading (a release's
/// bundle), hashing, checking, uploading, verifying, installing,
/// rebooting, validating -- each with its progress when it has one, and
/// with what the device says as log lines. An interrupted upload is picked
/// up where it stopped, within the run (a dropped channel) and by the next
/// one; cancelling `ctx` stops between or within steps.
#[async_trait::async_trait]
pub trait InstallServiceInterface: Send + Sync {
    async fn install(
        &self,
        request: InstallRequest,
        ctx: &OperationContext,
    ) -> Result<InstallOutcome>;
}

#[derive(Clone)]
pub struct InstallService(busybody::Service<Box<dyn InstallServiceInterface>>);

impl InstallService {
    pub fn new<T: InstallServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for InstallService {
    type Target = busybody::Service<Box<dyn InstallServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
