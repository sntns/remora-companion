use std::path::Path;

use remora_disk::model::DiskInfo;
use remora_progress::OperationContext;

use crate::model::{FlashOutcome, FlashRequest};

use super::error::Result;

/// The flash vertical's application-facing port. `preflight` is exposed on
/// its own (not just folded into `flash`) so a CLI (or a future GUI) can run
/// the safety check up front, before asking for interactive confirmation.
/// Both look the target up themselves, through the disk vertical: a guard
/// fed a caller-supplied `DiskInfo` would check whatever disk the caller
/// described, not the one `request.device` opens.
///
/// `flash` reports coarse `Phase` events through `ctx`, not fine-grained
/// byte progress: the destination is a raw block device, whose reported
/// size is its fixed capacity, not "bytes written so far" -- unlike a
/// regular output file, there's no size to poll (see `FlashControllerImpl`).
/// `ctx.cancel` is checked before the copy starts; once under way, the
/// underlying `bmap_parser` copy is not preemptible.
#[async_trait::async_trait]
pub trait FlashServiceInterface: Send + Sync {
    /// The disk `device` resolves to, if it's safe to overwrite: never the
    /// system disk, and a non-removable one only with `force`.
    async fn preflight(&self, device: &Path, force: bool) -> Result<DiskInfo>;
    async fn flash(&self, request: &FlashRequest, ctx: &OperationContext) -> Result<FlashOutcome>;
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
