//! Disk enumeration: list candidate flash targets and answer "is this the
//! system disk?" without shelling out to any external tool. Linux is
//! implemented via `/sys/block` + `/proc/self/mountinfo`; other OSes land in
//! a later phase (see the project plan).

use std::{fmt, path::Path};

use crate::model::disk::DiskInfo;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    NotFound { path: std::path::PathBuf },
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error while enumerating disks: {e}"),
            Error::NotFound { path } => write!(f, "no such disk: {}", path.display()),
            Error::Unsupported => {
                write!(f, "disk enumeration is not yet implemented on this OS")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

#[cfg(target_os = "linux")]
mod linux;

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
