use remora_context::model::ContextOverride;

use super::error::Result;
use crate::model::{DiskImage, ReleaseArtifact};

/// An artifact download in flight: its bytes from the requested offset, in
/// order, a chunk at a time. Dropping it cancels the download.
#[async_trait::async_trait]
pub trait ArtifactChunks: Send {
    /// The next chunk; `None` once the artifact's end is reached.
    async fn next(&mut self) -> Result<Option<Vec<u8>>>;
}

/// The releases disk images are published in, as far as flashing needs
/// them: which images of a type a release has, and their bytes from any
/// offset -- what reading one like a file takes.
#[async_trait::async_trait]
pub trait ReleaseArtifactAdapter: Send + Sync {
    /// The artifacts of `release` tagged `type:<image_type>`.
    async fn disk_images(
        &self,
        over: Option<&ContextOverride>,
        release: &str,
        image_type: &str,
    ) -> Result<Vec<DiskImage>>;
    async fn download(
        &self,
        artifact: &ReleaseArtifact,
        offset: u64,
    ) -> Result<Box<dyn ArtifactChunks>>;
}

/// Injectable handle to whatever `ReleaseArtifactAdapter` was wired at
/// startup.
#[derive(Clone)]
pub struct ReleaseArtifactAdapterService(busybody::Service<Box<dyn ReleaseArtifactAdapter>>);

impl ReleaseArtifactAdapterService {
    pub fn new<T: ReleaseArtifactAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for ReleaseArtifactAdapterService {
    type Target = busybody::Service<Box<dyn ReleaseArtifactAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
