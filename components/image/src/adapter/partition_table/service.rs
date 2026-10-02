use std::path::Path;

use crate::model::{FsKind, PartitionTable};

use super::error::Result;

/// DI seam for `remora-image-application`: swap the real GPT/MBR/
/// signature reader for a test double without touching the partition-
/// selection use cases.
pub trait PartitionTableAdapter: Send + Sync {
    fn read(&self, path: &Path) -> Result<PartitionTable>;
    fn detect_fs_kind(&self, image: &Path, partition_offset: u64) -> Result<FsKind>;
}

/// Injectable handle to whatever `PartitionTableAdapter` was wired at
/// startup.
#[derive(Clone)]
pub struct PartitionTableAdapterService(busybody::Service<Box<dyn PartitionTableAdapter>>);

impl PartitionTableAdapterService {
    pub fn new<T: PartitionTableAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for PartitionTableAdapterService {
    type Target = busybody::Service<Box<dyn PartitionTableAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
