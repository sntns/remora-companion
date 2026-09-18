use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to decode {0}")]
    Decode(PathBuf),
    #[error("failed to encode {0}")]
    Encode(PathBuf),
    #[error("failed to copy {0} to {1}")]
    Copy(PathBuf, PathBuf),
    #[error("conversion was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
