use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read {0}")]
    ReadFile(PathBuf),
    #[error("failed to write {0}")]
    WriteFile(PathBuf),
    #[error("{0} is not a container format this adapter understands")]
    InvalidHeader(PathBuf),
    #[error("{0} uses an unsupported feature: {1}")]
    UnsupportedFeature(PathBuf, String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
