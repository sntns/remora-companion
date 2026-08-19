use std::{fmt, io, path::PathBuf};

use crate::adapter::bmap_bmapparser;

#[derive(Debug)]
pub enum Error {
    OpenImage { path: PathBuf, source: io::Error },
    OpenBmap { path: PathBuf, source: io::Error },
    OpenDevice { path: PathBuf, source: io::Error },
    Bmap(bmap_bmapparser::Error),
    UnsafeTarget { path: PathBuf, reason: &'static str },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::OpenImage { path, source } => {
                write!(f, "failed to open image {}: {source}", path.display())
            }
            Error::OpenBmap { path, source } => {
                write!(f, "failed to read .bmap file {}: {source}", path.display())
            }
            Error::OpenDevice { path, source } => {
                write!(f, "failed to open device {}: {source}", path.display())
            }
            Error::Bmap(e) => write!(f, "{e}"),
            Error::UnsafeTarget { path, reason } => {
                write!(f, "refusing to flash {}: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<bmap_bmapparser::Error> for Error {
    fn from(e: bmap_bmapparser::Error) -> Self {
        Error::Bmap(e)
    }
}
