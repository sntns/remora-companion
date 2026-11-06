#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no usable context")]
    Context,
    #[error("failed to list devices")]
    List,
    #[error("failed to get device {0:?}")]
    Get(String),
    #[error("no device {0:?} in this account")]
    NotFound(String),
    #[error("failed to delete device {0:?}")]
    Delete(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
