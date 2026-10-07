#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read image")]
    Read,
    #[error("failed to write the raw image")]
    Write,
    #[error("malformed .bmaptar bundle: {0}")]
    Bundle(&'static str),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
