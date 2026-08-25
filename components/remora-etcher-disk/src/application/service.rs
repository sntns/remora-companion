use std::path::Path;

use crate::model::DiskInfo;

use super::error::Result;

/// The disk vertical's application-facing port: what every transport (CLI
/// today, anything else later) calls into.
pub trait DiskServiceInterface: Send + Sync {
    fn list(&self) -> Result<Vec<DiskInfo>>;
    fn info(&self, path: &Path) -> Result<DiskInfo>;
}

/// Injectable handle to whatever `DiskServiceInterface` implementation was
/// wired at startup (normally `remora-etcher-disk-application`'s
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
