use std::fmt;

use crate::adapter::disk_enum;

#[derive(Debug)]
pub enum Error {
    Enumerate(disk_enum::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Enumerate(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<disk_enum::Error> for Error {
    fn from(e: disk_enum::Error) -> Self {
        Error::Enumerate(e)
    }
}
