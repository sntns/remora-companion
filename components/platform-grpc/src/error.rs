#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid gateway address {0:?}")]
    InvalidAddress(String),
    #[error("invalid TLS configuration")]
    Tls,
    #[error("failed to connect to the gateway at {0}")]
    Connect(String),
    #[error("credentials cannot be sent as gRPC metadata (they must be printable ASCII)")]
    InvalidCredentials,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

/// One line describing a failed call, for an error attachment: the status
/// code in words and the server's own message, which is where the platform
/// (and, for a channel, the device) explains itself.
pub fn status_summary(status: &tonic::Status) -> String {
    let message = status.message();
    if message.is_empty() {
        format!("{:?}", status.code())
    } else {
        format!("{:?}: {message}", status.code())
    }
}
