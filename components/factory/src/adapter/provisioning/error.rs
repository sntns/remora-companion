#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to reach the platform's gateway")]
    Request,
    #[error("{0} was already manufactured (pass --force to re-sign it and revoke its old IDevID)")]
    AlreadyExists(String),
    #[error("factory-device provisioning was refused")]
    Refused,
    /// The context's credentials no longer authenticate (revoked, expired):
    /// a matter for whoever runs the tool, not a refusal of this device.
    #[error("the platform did not accept the context's credentials")]
    Unauthenticated,
    #[error("failed to parse the platform's response")]
    ParseResponse,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
