#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open the artifact")]
    Open,
    #[error("failed to read the artifact")]
    Read,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
