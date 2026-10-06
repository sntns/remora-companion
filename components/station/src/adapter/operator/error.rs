#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read the operator's input")]
    Read,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
