#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("step {index} ({kind}) failed")]
    Step { index: usize, kind: &'static str },
    #[error("batch was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
