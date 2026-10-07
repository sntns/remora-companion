use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read {0}")]
    ReadFile(PathBuf),
    #[error("failed to write {0}")]
    WriteFile(PathBuf),
    #[error("failed to decode {0} into {1}")]
    Decode(PathBuf, PathBuf),
    #[error("{0} is not a container format this adapter understands")]
    InvalidHeader(PathBuf),
    #[error("{0} uses an unsupported feature: {1}")]
    UnsupportedFeature(PathBuf, String),
    #[error("{0} is corrupt: its image doesn't match its .bmap's checksums")]
    Checksum(PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
