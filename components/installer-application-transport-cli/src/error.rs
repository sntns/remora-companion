#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to pack the installer")]
    Pack,
    #[error("failed to unpack the installer")]
    Unpack,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
