#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Context(String),
    #[error("failed to list devices")]
    List,
    #[error("failed to get device {0:?}")]
    Get(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
