use std::path::Path;

use remora_disk::{
    adapter::{DiskAdapter, DiskHold, DiskTarget, Result},
    model::DiskInfo,
};

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use remora_disk::adapter::Error;

#[cfg(target_os = "linux")]
use crate::linux as native;
#[cfg(target_os = "macos")]
use crate::macos as native;
#[cfg(windows)]
use crate::windows as native;

#[derive(Debug, Default, Clone, Copy)]
pub struct DiskAdapterImpl;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
impl DiskAdapter for DiskAdapterImpl {
    fn enumerate(&self) -> Result<Vec<DiskInfo>> {
        native::enumerate()
    }

    fn info(&self, path: &Path) -> Result<DiskInfo> {
        native::info(path)
    }

    fn open(&self, disk: &DiskInfo) -> Result<DiskTarget> {
        native::open(disk)
    }

    fn hold(&self, path: &Path) -> Result<Option<DiskHold>> {
        native::hold(path)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl DiskAdapter for DiskAdapterImpl {
    fn enumerate(&self) -> Result<Vec<DiskInfo>> {
        Err(error_stack::Report::new(Error::Unsupported))
    }

    fn info(&self, _path: &Path) -> Result<DiskInfo> {
        Err(error_stack::Report::new(Error::Unsupported))
    }

    fn open(&self, _disk: &DiskInfo) -> Result<DiskTarget> {
        Err(error_stack::Report::new(Error::Unsupported))
    }

    /// No disk is ever written here but an image file: nothing to hold.
    fn hold(&self, _path: &Path) -> Result<Option<DiskHold>> {
        Ok(None)
    }
}
