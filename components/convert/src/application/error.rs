use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to decode {0}")]
    Decode(PathBuf),
    #[error("failed to encode {0}")]
    Encode(PathBuf),
    #[error("failed to copy {0} to {1}")]
    Copy(PathBuf, PathBuf),
    #[error("failed to read {0}")]
    Read(PathBuf),
    #[error("{0} is named as {1} but holds {2} data")]
    Mismatch(PathBuf, &'static str, &'static str),
    #[error("{0} is named as {1} but isn't {1} data")]
    NotFormat(PathBuf, &'static str),
    #[error("{0} holds {1} data, not a raw image: name it *.{2} to convert it")]
    NotRaw(PathBuf, &'static str, &'static str),
    #[error("conversion was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
