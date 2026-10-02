#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to reach the gateway")]
    Unreachable,
    #[error("the platform refused these credentials")]
    Unauthenticated,
    #[error("not allowed")]
    PermissionDenied,
    #[error("device not found")]
    NotFound,
    #[error("the platform call failed")]
    Call,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
