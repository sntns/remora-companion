#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to convert to a raw image")]
    ToRaw,
    #[error("failed to convert from a raw image")]
    FromRaw,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
