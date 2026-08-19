use std::fmt;

#[derive(Debug)]
pub enum Error {
    InvalidBlockSize(u32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidBlockSize(size) => write!(
                f,
                "invalid squashfs block size {size}: must be a power of two between {} and {}",
                crate::model::squashfs::MIN_BLOCK_SIZE,
                crate::model::squashfs::MAX_BLOCK_SIZE
            ),
        }
    }
}

impl std::error::Error for Error {}
