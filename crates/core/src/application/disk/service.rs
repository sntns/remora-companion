use std::path::Path;

use crate::model::disk::DiskInfo;

use super::error::Error;

pub fn list() -> Result<Vec<DiskInfo>, Error> {
    Ok(crate::adapter::disk::enumerate()?)
}

pub fn info(path: &Path) -> Result<DiskInfo, Error> {
    Ok(crate::adapter::disk::info(path)?)
}
