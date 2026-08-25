use remora_etcher_disk::model::DiskInfo;

use crate::model::{FlashOutcome, FlashRequest};

use super::error::Result;

/// The flash vertical's application-facing port. `preflight` is exposed on
/// its own (not just folded into `flash`) so a CLI (or a future GUI) can run
/// the safety check up front, before asking for interactive confirmation.
pub trait FlashServiceInterface: Send + Sync {
    fn preflight(&self, info: &DiskInfo, force: bool) -> Result<()>;
    fn flash(&self, request: &FlashRequest, info: &DiskInfo) -> Result<FlashOutcome>;
}

/// Injectable handle to whatever `FlashServiceInterface` implementation was
/// wired at startup (normally `remora-etcher-flash-application`'s
/// `FlashControllerImpl`).
#[derive(Clone)]
pub struct FlashService(busybody::Service<Box<dyn FlashServiceInterface>>);

impl FlashService {
    pub fn new<T: FlashServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for FlashService {
    type Target = busybody::Service<Box<dyn FlashServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
