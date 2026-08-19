pub mod error;

use std::path::Path;

use crate::model::disk::DiskInfo;

pub use error::Error;

pub fn list() -> Result<Vec<DiskInfo>, Error> {
    Ok(crate::adapter::disk_enum::enumerate()?)
}

pub fn info(path: &Path) -> Result<DiskInfo, Error> {
    Ok(crate::adapter::disk_enum::info(path)?)
}
