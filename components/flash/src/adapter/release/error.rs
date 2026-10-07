#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read release {0:?}")]
    Release(String),
    #[error("failed to download {0}")]
    Download(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
