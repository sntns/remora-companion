use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} is not a provisioning station")]
    NotAStation(String),
    #[error("failed to generate the device key")]
    Keygen,
    #[error("the station refused the claim")]
    Refused,
    /// What the station said is the cause.
    #[error("lost the claim while waiting for its label")]
    Lost,
    #[error("the station says this claim is {0}")]
    Abandoned(&'static str),
    #[error("failed to write {0}")]
    WriteOutput(PathBuf),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
