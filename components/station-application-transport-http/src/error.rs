#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to serve the station's HTTP API")]
    Serve,
    #[error("failed to reach the station at {0}")]
    Unreachable(String),
    #[error("the station answered {status}: {message}")]
    Status {
        status: u16,
        /// Why, for the device to decide on.
        code: Option<crate::ErrorCode>,
        message: String,
        /// Seconds to wait before retrying, on a 503.
        retry_after: Option<u64>,
    },
    #[error("unexpected answer from the station")]
    Parse,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
