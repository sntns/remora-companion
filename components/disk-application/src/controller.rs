use std::path::Path;

use error_stack::ResultExt;
use remora_disk::{
    adapter::{DiskAdapterService, DiskHold, DiskTarget},
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
    // The adapter walks sysfs and stats device nodes, which can block on a
    // slow or spun-down disk: off the runtime threads.
    async fn list(&self) -> Result<Vec<DiskInfo>> {
        let adapter = self.adapter.clone();
        tokio::task::spawn_blocking(move || adapter.enumerate())
            .await
            .expect("disk worker panicked")
            .change_context(Error::Enumerate)
    }

    async fn info(&self, path: &Path) -> Result<DiskInfo> {
        let adapter = self.adapter.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || adapter.info(&path))
            .await
            .expect("disk worker panicked")
            .change_context(Error::Info)
    }

    // Unmounting waits on the OS's disk arbitration: blocking too.
    async fn open(&self, disk: &DiskInfo) -> Result<DiskTarget> {
        let adapter = self.adapter.clone();
        let disk = disk.clone();
        tokio::task::spawn_blocking(move || adapter.open(&disk))
            .await
            .expect("disk worker panicked")
            .change_context(Error::Open)
    }

    // Waits on the administrator prompt, then the helper's unmounting.
    async fn open_elevated(&self, disk: &DiskInfo) -> Result<DiskTarget> {
        let adapter = self.adapter.clone();
        let disk = disk.clone();
        tokio::task::spawn_blocking(move || adapter.open_elevated(&disk))
            .await
            .expect("disk worker panicked")
            .change_context(Error::Open)
    }

    async fn check_writable(&self, disk: &DiskInfo) -> Result<()> {
        let adapter = self.adapter.clone();
        let disk = disk.clone();
        tokio::task::spawn_blocking(move || adapter.check_writable(&disk))
            .await
            .expect("disk worker panicked")
            .change_context(Error::Access)
    }

    async fn hold(&self, path: &Path) -> Result<Option<DiskHold>> {
        let adapter = self.adapter.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || adapter.hold(&path))
            .await
            .expect("disk worker panicked")
            .change_context(Error::Hold)
    }
}
