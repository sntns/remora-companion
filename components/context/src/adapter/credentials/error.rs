#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read the credentials of context {0:?}")]
    Read(String),
    #[error("failed to store the credentials of context {0:?}")]
    Write(String),
    #[error("failed to remove the credentials of context {0:?}")]
    Delete(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
