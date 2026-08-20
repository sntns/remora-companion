use std::path::Path;

use crate::model::disk::DiskInfo;

use super::error::Error;
use super::service;

/// DI seam for `application::disk`: swap the real `/sys/block` reader for a
/// test double without touching the application layer.
pub trait DiskAdapter {
    fn enumerate(&self) -> Result<Vec<DiskInfo>, Error>;
    fn info(&self, path: &Path) -> Result<DiskInfo, Error>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Disk;

impl DiskAdapter for Disk {
    fn enumerate(&self) -> Result<Vec<DiskInfo>, Error> {
        service::enumerate()
    }

    fn info(&self, path: &Path) -> Result<DiskInfo, Error> {
        service::info(path)
    }
}
