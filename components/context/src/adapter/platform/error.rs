#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to reach the platform")]
    Unreachable,
    #[error("the platform refused these credentials")]
    Unauthenticated,
    #[error("the platform call failed")]
    Call,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
