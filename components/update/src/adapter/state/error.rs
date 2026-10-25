#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read when updates were last looked for")]
    Read,
    #[error("failed to record when updates were last looked for")]
    Write,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
