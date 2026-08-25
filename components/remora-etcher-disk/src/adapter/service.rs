use std::path::Path;

use crate::model::DiskInfo;

use super::error::Result;

/// DI seam for `remora-etcher-disk-application`: swap the real `/sys/block`
/// reader for a test double without touching the use case.
pub trait DiskAdapter: Send + Sync {
    fn enumerate(&self) -> Result<Vec<DiskInfo>>;
    fn info(&self, path: &Path) -> Result<DiskInfo>;
}

/// Injectable handle to whatever `DiskAdapter` was wired at startup.
#[derive(Clone)]
pub struct DiskAdapterService(busybody::Service<Box<dyn DiskAdapter>>);

impl DiskAdapterService {
    pub fn new<T: DiskAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for DiskAdapterService {
    type Target = busybody::Service<Box<dyn DiskAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
