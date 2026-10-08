use remora_progress::OperationContext;

use super::error::Result;
use crate::model::{PackOutcome, PackRequest};

/// The installer vertical's application-facing port.
///
/// An installer is a disk image of its own, as meta-remora builds it
/// (`remora-installer-<machine>.wic.bmaptar`): an EFI partition booting
/// the installer's initramfs, then, last, a raw `installer` partition
/// holding the disk image it flashes onto the device's own disk -- a
/// `.bmaptar`, read in place where it lies. Flashed to a USB stick, it
/// installs that image on whatever device boots it.
#[async_trait::async_trait]
pub trait InstallerServiceInterface: Send + Sync {
    /// Write `request.output`: `request.installer` with its payload
    /// replaced by `request.image` -- e.g. a product image provisioned
    /// with `image`/`identity`/`config` -- the payload partition resized to
    /// it. Both inputs may be in any format `convert` reads; the image is
    /// bundled as a `.bmaptar` first when it isn't one, and the output's
    /// format is the one its extension names, as for `convert from-raw`.
    ///
    /// It works on a raw copy of the installer in a scratch directory next
    /// to `request.output` (sparse: about the payload's size, not the
    /// image's), removed afterwards. Reports one phase per step through
    /// `ctx`, which also cancels it.
    async fn pack(&self, request: &PackRequest, ctx: &OperationContext) -> Result<PackOutcome>;
}

/// Injectable handle to whatever `InstallerServiceInterface`
/// implementation was wired at startup (normally
/// `remora-installer-application`'s `InstallerControllerImpl`).
#[derive(Clone)]
pub struct InstallerService(busybody::Service<Box<dyn InstallerServiceInterface>>);

impl InstallerService {
    pub fn new<T: InstallerServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for InstallerService {
    type Target = busybody::Service<Box<dyn InstallerServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
