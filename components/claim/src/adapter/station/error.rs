use std::time::Duration;

/// How a station call failed, as far as a device decides on it: retry
/// (`Unreachable`, `Unavailable`), or give up and say why (the rest). The
/// station's own message is attached.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to reach the station at {0}")]
    Unreachable(String),
    #[error("{0} is not a provisioning station")]
    NotAStation(String),
    /// The station can't serve this now; it says when to retry.
    #[error("the station is unavailable")]
    Unavailable { retry_after: Option<Duration> },
    /// Retrying the same request won't help: a bad CSR, an unknown board,
    /// a refusal by the platform...
    #[error("the station refused the request")]
    Refused,
    #[error("unexpected answer from the station")]
    Unexpected,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
