use std::path::Path;

use crate::{
    adapter::{DiskHold, DiskTarget},
    model::DiskInfo,
};

use super::error::Result;

/// The disk vertical's application-facing port: what every transport (CLI
/// today, anything else later) calls into.
#[async_trait::async_trait]
pub trait DiskServiceInterface: Send + Sync {
    async fn list(&self) -> Result<Vec<DiskInfo>>;
    async fn info(&self, path: &Path) -> Result<DiskInfo>;
    /// The disk `disk` describes (as `info` returned it), opened to be
    /// overwritten whole.
    async fn open(&self, disk: &DiskInfo) -> Result<DiskTarget>;
    /// See [`DiskAdapter::hold`](crate::adapter::DiskAdapter::hold).
    async fn hold(&self, path: &Path) -> Result<Option<DiskHold>>;
}

/// Injectable handle to whatever `DiskServiceInterface` implementation was
/// wired at startup (normally `remora-disk-application`'s
/// `DiskControllerImpl`).
#[derive(Clone)]
pub struct DiskService(busybody::Service<Box<dyn DiskServiceInterface>>);

impl DiskService {
    pub fn new<T: DiskServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for DiskService {
    type Target = busybody::Service<Box<dyn DiskServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
