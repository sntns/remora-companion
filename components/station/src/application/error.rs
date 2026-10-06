use std::path::PathBuf;

/// What a transport needs to tell apart: who is at fault (the device, the
/// station's configuration, the platform) and whether retrying can help.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid certificate signing request")]
    InvalidCsr,
    #[error("board {0:?} is not configured on this station")]
    UnknownBoard(String),
    #[error("{0}")]
    InvalidHardware(String),
    #[error("this station's quota of {0} claims is reached")]
    QuotaReached(u32),
    #[error("the platform refused to issue an identity")]
    Refused,
    #[error("device {0:?} already exists on the platform")]
    AlreadyExists(String),
    #[error("the platform is unreachable")]
    Unavailable,
    #[error("no claim {0}")]
    UnknownClaim(String),
    #[error("the station is not started")]
    NotStarted,
    #[error("the station is already started")]
    AlreadyStarted,
    #[error("{0}")]
    Context(String),
    #[error("the hooks directory {0} cannot be read")]
    Hooks(PathBuf),
    #[error("failed to load the journal {0}")]
    LoadJournal(PathBuf),
    #[error("failed to write the journal {0}")]
    Journal(PathBuf),
    #[error("failed to read the operator's input")]
    Operator,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
