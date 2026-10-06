use std::path::PathBuf;

/// What a transport needs to tell apart: who is at fault (the device, the
/// station's configuration, the platform, the station's own disk) and
/// whether retrying can help.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid certificate signing request")]
    InvalidCsr,
    #[error("board {0:?} is not configured on this station")]
    UnknownBoard(String),
    /// What is wrong with it is the cause: a `FieldError`, or the
    /// `TemplateError` of a field its board's `device-name` needs.
    #[error("invalid hardware report")]
    InvalidHardware,
    #[error("this station's quota of {0} claims is reached")]
    QuotaReached(u32),
    #[error("the platform refused to issue an identity")]
    Refused,
    /// The platform issued nothing usable: its deployment has no access
    /// URL configured for devices to phone home to.
    #[error("the platform has no access URL for devices on this deployment")]
    MissingAccessUrl,
    #[error("device {0:?} already exists on the platform")]
    AlreadyExists(String),
    /// The platform can't be reached, or no longer accepts the context's
    /// credentials: someone has to act, then the device's retry works.
    #[error("the platform is unreachable")]
    Unavailable,
    #[error("no claim {0}")]
    UnknownClaim(String),
    /// An `installed` ack before the label was validated: only the commit
    /// point lets a device install its identity.
    #[error("claim {0} is not labelled yet")]
    NotLabelled(String),
    #[error("the station is not started")]
    NotStarted,
    #[error("the station is already started")]
    AlreadyStarted,
    /// The context's own message is the headline (which context isn't
    /// logged in, or refused), as in the factory vertical.
    #[error("{0}")]
    Context(String),
    #[error("the hooks directory {0} cannot be read")]
    Hooks(PathBuf),
    #[error("failed to load the journal {0}")]
    LoadJournal(PathBuf),
    /// The production register can't be written: nothing is issued or
    /// handed out that it doesn't record, so the device retries later.
    #[error("failed to write the journal {0}")]
    Journal(PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
