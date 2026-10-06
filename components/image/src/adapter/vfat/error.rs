use std::{io, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on {0}")]
    Io(PathBuf, #[source] io::Error),
    #[error(
        "partition window [{offset}, {}) does not fit inside {} ({window_size} bytes)",
        offset + len, path.display()
    )]
    OutOfWindow {
        path: PathBuf,
        offset: u64,
        len: u64,
        window_size: u64,
    },
    #[error("{0}")]
    Fat(#[from] fatfs::Error<io::Error>),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
