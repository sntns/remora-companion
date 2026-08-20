use std::fmt;

use crate::adapter::disk;

#[derive(Debug)]
pub enum Error {
    Enumerate(disk::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Enumerate(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<disk::Error> for Error {
    fn from(e: disk::Error) -> Self {
        Error::Enumerate(e)
    }
}
