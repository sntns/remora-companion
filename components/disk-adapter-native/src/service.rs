use std::path::Path;

use remora_disk::{
    adapter::{DiskAdapter, Result},
    model::DiskInfo,
};

#[cfg(not(target_os = "linux"))]
use remora_disk::adapter::Error;

#[cfg(target_os = "linux")]
use crate::linux;

#[derive(Debug, Default, Clone, Copy)]
pub struct DiskAdapterImpl;

impl DiskAdapter for DiskAdapterImpl {
    #[cfg(target_os = "linux")]
    fn enumerate(&self) -> Result<Vec<DiskInfo>> {
        linux::enumerate()
    }

    #[cfg(target_os = "linux")]
    fn info(&self, path: &Path) -> Result<DiskInfo> {
        linux::info(path)
    }

    #[cfg(not(target_os = "linux"))]
    fn enumerate(&self) -> Result<Vec<DiskInfo>> {
        Err(error_stack::Report::new(Error::Unsupported))
    }

    #[cfg(not(target_os = "linux"))]
    fn info(&self, _path: &Path) -> Result<DiskInfo> {
        Err(error_stack::Report::new(Error::Unsupported))
    }
}
