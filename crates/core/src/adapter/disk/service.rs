use std::path::Path;

use crate::model::disk::DiskInfo;

use super::error::Error;

#[cfg(target_os = "linux")]
use super::linux;

#[cfg(target_os = "linux")]
pub fn enumerate() -> Result<Vec<DiskInfo>, Error> {
    linux::enumerate()
}

#[cfg(target_os = "linux")]
pub fn info(path: &Path) -> Result<DiskInfo, Error> {
    linux::info(path)
}

#[cfg(not(target_os = "linux"))]
pub fn enumerate() -> Result<Vec<DiskInfo>, Error> {
    Err(Error::Unsupported)
}

#[cfg(not(target_os = "linux"))]
pub fn info(_path: &Path) -> Result<DiskInfo, Error> {
    Err(Error::Unsupported)
}
