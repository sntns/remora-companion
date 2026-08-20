use std::{fmt, io, path::PathBuf};

use backhand::BackhandError;

#[derive(Debug)]
pub enum Error {
    Backhand(BackhandError),
    OpenInput { path: PathBuf, source: io::Error },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Backhand(e) => write!(f, "backhand error: {e}"),
            Error::OpenInput { path, source } => {
                write!(f, "failed to open {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<BackhandError> for Error {
    fn from(e: BackhandError) -> Self {
        Error::Backhand(e)
    }
}
