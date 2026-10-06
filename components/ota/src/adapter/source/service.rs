use std::path::Path;

use super::error::Result;

/// An artifact being read, front to back.
#[async_trait::async_trait]
pub trait ArtifactReader: Send {
    /// Up to `max` bytes; empty once the end is reached.
    async fn read(&mut self, max: usize) -> Result<Vec<u8>>;
}

/// Where the bytes of an artifact to upload come from: a local file for
/// rmra. Async, unlike the context store, because it streams a whole bundle
/// alongside the upload, which a blocked runtime thread would stall.
#[async_trait::async_trait]
pub trait ArtifactSourceAdapter: Send + Sync {
    /// How many bytes the artifact holds.
    async fn length(&self, path: &Path) -> Result<u64>;
    /// The artifact, from byte `offset` on (a resumed upload's start).
    async fn open(&self, path: &Path, offset: u64) -> Result<Box<dyn ArtifactReader>>;
}

#[derive(Clone)]
pub struct ArtifactSourceAdapterService(busybody::Service<Box<dyn ArtifactSourceAdapter>>);

impl ArtifactSourceAdapterService {
    pub fn new<T: ArtifactSourceAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for ArtifactSourceAdapterService {
    type Target = busybody::Service<Box<dyn ArtifactSourceAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
