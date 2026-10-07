use std::path::Path;

use super::error::Result;

/// A download being written, front to back, into the partial file of its
/// destination.
#[async_trait::async_trait]
pub trait ArtifactWriter: Send {
    async fn write(&mut self, chunk: &[u8]) -> Result<()>;
    /// Flushes what was written to the disk, and says the sha256 (hex) of
    /// the whole partial file, what an earlier run left included.
    async fn finish(self: Box<Self>) -> Result<String>;
}

/// Where a downloaded artifact goes: a local file for rmra, written next
/// to its destination as a partial file until it's complete and verified,
/// so that an interrupted download is picked up again by the next run and
/// never mistaken for a whole one.
#[async_trait::async_trait]
pub trait ArtifactTargetAdapter: Send + Sync {
    /// Whether `path` already exists, as a whole file.
    async fn exists(&self, path: &Path) -> Result<bool>;
    /// How many bytes an earlier, interrupted download to `path` left.
    async fn partial(&self, path: &Path) -> Result<u64>;
    /// The partial file of `path`, kept up to byte `offset` and written on
    /// from there.
    async fn open(&self, path: &Path, offset: u64) -> Result<Box<dyn ArtifactWriter>>;
    /// Makes the finished partial file `path` itself, replacing it.
    async fn commit(&self, path: &Path) -> Result<()>;
    /// Removes the partial file of `path`: what it holds is wrong.
    async fn discard(&self, path: &Path) -> Result<()>;
}

#[derive(Clone)]
pub struct ArtifactTargetAdapterService(busybody::Service<Box<dyn ArtifactTargetAdapter>>);

impl ArtifactTargetAdapterService {
    pub fn new<T: ArtifactTargetAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for ArtifactTargetAdapterService {
    type Target = busybody::Service<Box<dyn ArtifactTargetAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
