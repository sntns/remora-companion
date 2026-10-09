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
    #[error("the device is not connected to the platform")]
    NotConnected,
    #[error("the platform refused the request")]
    InvalidArgument,
    #[error("the device or account is not set up for this")]
    FailedPrecondition,
    #[error("the gateway answered something this client does not understand")]
    Protocol,
    #[error("the channel failed")]
    Channel,
    #[error("the gateway ended the channel")]
    Ended,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
