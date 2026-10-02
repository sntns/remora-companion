use std::path::Path;

use remora_progress::OperationContext;

use super::error::Result;

/// The convert vertical's application-facing port: unwrap a whole-disk
/// image out of (or back into) whatever container format its own path
/// extension names -- `.qcow2`, `.gz`, or a plain raw copy for anything
/// else -- entirely inside remora-etcher, with no external tool
/// (`qemu-img`, `gzip`) shelled out to.
///
/// Both directions report progress/observe cancellation through `ctx` --
/// see `ConvertControllerImpl` for how, given neither codec's own loop is
/// instrumented directly (see its doc comment for why).
#[async_trait::async_trait]
pub trait ConvertServiceInterface: Send + Sync {
    /// Decode `image` into a plain raw disk image at `output_raw`. The
    /// source format is detected from `image`'s extension.
    async fn to_raw(&self, image: &Path, output_raw: &Path, ctx: &OperationContext) -> Result<()>;

    /// Encode `raw_image` (a plain raw disk image) into `output`. The
    /// destination format is detected from `output`'s extension.
    #[allow(clippy::wrong_self_convention)] // `from_raw`/`to_raw` name the raw-image direction of the conversion, not a `From` constructor
    async fn from_raw(&self, raw_image: &Path, output: &Path, ctx: &OperationContext)
        -> Result<()>;
}

/// Injectable handle to whatever `ConvertServiceInterface` implementation
/// was wired at startup (normally `remora-convert-application`'s
/// `ConvertControllerImpl`).
#[derive(Clone)]
pub struct ConvertService(busybody::Service<Box<dyn ConvertServiceInterface>>);

impl ConvertService {
    pub fn new<T: ConvertServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for ConvertService {
    type Target = busybody::Service<Box<dyn ConvertServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
