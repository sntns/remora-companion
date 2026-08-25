use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("backhand error: {0}")]
    Backhand(#[from] backhand::BackhandError),
    #[error("failed to open {0}")]
    OpenInput(PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
