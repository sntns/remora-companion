use std::path::Path;

use error_stack::ResultExt;
use remora_disk::{
    adapter::DiskAdapterService,
    application::{DiskServiceInterface, Error, Result},
    model::DiskInfo,
};

/// The disk vertical's use case: purely orchestration, no I/O of its own —
/// every actual I/O call goes through the injected `DiskAdapterService`.
pub struct DiskControllerImpl {
    adapter: DiskAdapterService,
}

impl DiskControllerImpl {
    pub fn new(adapter: DiskAdapterService) -> Self {
        Self { adapter }
    }
}

#[async_trait::async_trait]
impl DiskServiceInterface for DiskControllerImpl {
    async fn list(&self) -> Result<Vec<DiskInfo>> {
        self.adapter.enumerate().change_context(Error::Enumerate)
    }

    async fn info(&self, path: &Path) -> Result<DiskInfo> {
        self.adapter.info(path).change_context(Error::Info)
    }
}
