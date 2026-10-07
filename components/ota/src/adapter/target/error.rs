#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open the download's file")]
    Open,
    #[error("failed to write the download's file")]
    Write,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
