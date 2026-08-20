use std::{fmt, io, path::PathBuf};

#[derive(Debug)]
pub enum Error {
    Open {
        path: PathBuf,
        source: io::Error,
    },
    OutOfWindow {
        path: PathBuf,
        offset: u64,
        len: u64,
        window_size: u64,
    },
    Fat(fatfs::Error<io::Error>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Open { path, source } => {
                write!(f, "failed to open {}: {source}", path.display())
            }
            Error::OutOfWindow {
                path,
                offset,
                len,
                window_size,
            } => write!(
                f,
                "partition window [{offset}, {}) does not fit inside {} ({window_size} bytes)",
                offset + len,
                path.display()
            ),
            Error::Fat(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}
