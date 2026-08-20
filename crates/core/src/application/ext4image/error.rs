use std::{fmt, io, path::PathBuf};

use crate::adapter::ext4;

#[derive(Debug)]
pub enum Error {
    Walk { path: PathBuf, source: io::Error },
    UnsupportedEntry { path: PathBuf },
    ReadFile { path: PathBuf, source: io::Error },
    Format(ext4::Error),
    Populate(ext4::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Walk { path, source } => {
                write!(f, "failed to walk {}: {source}", path.display())
            }
            Error::UnsupportedEntry { path } => write!(
                f,
                "unsupported filesystem entry at {} (only regular files and \
                 directories can go into a config ext4 image)",
                path.display()
            ),
            Error::ReadFile { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Error::Format(e) => write!(f, "failed to format ext4 image: {e}"),
            Error::Populate(e) => write!(f, "failed to populate ext4 image: {e}"),
        }
    }
}

impl std::error::Error for Error {}
