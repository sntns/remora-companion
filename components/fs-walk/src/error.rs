use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to walk {0}")]
    Walk(PathBuf),
    #[error("unsupported filesystem entry at {0}")]
    UnsupportedEntry(PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
