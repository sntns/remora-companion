use remora_context::model::ContextOverride;
use remora_disk::model::DiskInfo;
use remora_progress::OperationContext;

use crate::model::{DiskImage, FlashOutcome, FlashRequest, ImageSummary};

use super::error::Result;

/// The flash vertical's application-facing port. `inspect` and `preflight`
/// are exposed on their own (not just folded into `flash`) so a CLI (or a
/// future GUI) can show what is about to be written and run the safety
/// check up front, before anything is written. They look
/// the target up themselves, through the disk vertical: a guard fed a
/// caller-supplied `DiskInfo` would check whatever disk the caller
/// described, not the one `request.device` opens.
///
/// `flash` reports its advancement through `ctx`: a "flashing" phase whose
/// `Progress` counts the image bytes copied so far (not the unmapped ones
/// skipped) out of those it will copy -- the mapped ones with a `.bmap`,
/// else the whole image's, else unknown -- plus, for a release artifact,
/// `Transfer` for the bytes downloaded out of the artifact's -- then a
/// "syncing" one while what
/// the kernel still buffers reaches the disk. `ctx.cancel` stops the copy
/// at its next read of the image, or within a few MiB of a skip.
#[async_trait::async_trait]
pub trait FlashServiceInterface: Send + Sync {
    /// The disk images of `release`: its artifacts tagged `type:diskimage`.
    async fn disk_images(
        &self,
        over: Option<&ContextOverride>,
        release: &str,
    ) -> Result<Vec<DiskImage>>;
    /// What `request` would write: its image and `.bmap`, read without
    /// copying anything.
    async fn inspect(&self, request: &FlashRequest) -> Result<ImageSummary>;
    /// The disk `request.device` resolves to, if it's safe to overwrite
    /// with `request.image`: never the system disk, a non-removable one
    /// only with `request.force`, and never one smaller than the image.
    async fn preflight(&self, request: &FlashRequest) -> Result<DiskInfo>;
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
