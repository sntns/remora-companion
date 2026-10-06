use std::path::Path;

use super::error::Result;
use super::model::WalkEntry;

/// DI seam for any vertical that builds an image from a directory tree
/// (squashfs today, ext4image/config later): swap the real recursive
/// `walkdir` traversal for a test double without touching the build use case.
pub trait FsWalkAdapter: Send + Sync {
    /// Recursively walk `root` (`root` itself excluded, matching
    /// `WalkDir::min_depth(1)`), returning every descendant with its path
    /// relative to `root`.
    fn walk_dir(&self, root: &Path) -> Result<Vec<WalkEntry>>;
}

/// Injectable handle to whatever `FsWalkAdapter` was wired at startup.
#[derive(Clone)]
pub struct FsWalkAdapterService(busybody::Service<Box<dyn FsWalkAdapter>>);

impl FsWalkAdapterService {
    pub fn new<T: FsWalkAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for FsWalkAdapterService {
    type Target = busybody::Service<Box<dyn FsWalkAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
