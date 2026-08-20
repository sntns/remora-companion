use std::{fmt, io, path::PathBuf};

use crate::{adapter::squashfs, model};

#[derive(Debug)]
pub enum Error {
    InvalidOptions(model::error::Error),
    NoInputs,
    Walk { path: PathBuf, source: io::Error },
    UnsupportedEntry { path: PathBuf },
    Build(squashfs::Error),
    CreateOutput { path: PathBuf, source: io::Error },
    OpenInput { path: PathBuf, source: io::Error },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidOptions(e) => write!(f, "invalid build options: {e}"),
            Error::NoInputs => write!(f, "no input files or directories given"),
            Error::Walk { path, source } => {
                write!(f, "failed to walk {}: {source}", path.display())
            }
            Error::UnsupportedEntry { path } => {
                write!(f, "unsupported filesystem entry at {}", path.display())
            }
            Error::Build(e) => write!(f, "failed to build squashfs image: {e}"),
            Error::CreateOutput { path, source } => {
                write!(f, "failed to create {}: {source}", path.display())
            }
            Error::OpenInput { path, source } => {
                write!(f, "failed to open {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<model::error::Error> for Error {
    fn from(e: model::error::Error) -> Self {
        Error::InvalidOptions(e)
    }
}

impl From<squashfs::Error> for Error {
    fn from(e: squashfs::Error) -> Self {
        Error::Build(e)
    }
}
