use remora_disk::model::DiskInfo;
use remora_progress::OperationContext;

use crate::model::{FlashOutcome, FlashRequest};

use super::error::Result;

/// The flash vertical's application-facing port. `preflight` is exposed on
/// its own (not just folded into `flash`) so a CLI (or a future GUI) can run
/// the safety check up front, before asking for interactive confirmation.
///
/// `flash` reports coarse `Phase` events through `ctx`, not fine-grained
/// byte progress: the destination is a raw block device, whose reported
/// size is its fixed capacity, not "bytes written so far" -- unlike a
/// regular output file, there's no size to poll (see `FlashControllerImpl`).
/// `ctx.cancel` is checked before the copy starts; once under way, the
/// underlying `bmap_parser` copy is not preemptible.
#[async_trait::async_trait]
pub trait FlashServiceInterface: Send + Sync {
    async fn preflight(&self, info: &DiskInfo, force: bool) -> Result<()>;
    async fn flash(
        &self,
        request: &FlashRequest,
        info: &DiskInfo,
        ctx: &OperationContext,
    ) -> Result<FlashOutcome>;
}

/// Injectable handle to whatever `FlashServiceInterface` implementation was
/// wired at startup (normally `remora-flash-application`'s
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
