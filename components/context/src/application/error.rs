/// What went wrong, binary-neutral: several binaries mount the context
/// commands, so what to run about it is each transport's to say (see
/// `remora-context-application-transport-cli`'s `hint`).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid context name {0:?}: use letters, digits, '.', '_', '+' or '-', starting with a letter or digit")]
    InvalidName(String),
    #[error("context {0:?} already exists")]
    AlreadyExists(String),
    #[error("context {0:?} does not exist")]
    NotFound(String),
    #[error("no context selected")]
    NoContext,
    #[error("several contexts exist and none is current")]
    Ambiguous,
    #[error("not logged in to context {0:?}")]
    NotLoggedIn(String),
    #[error("failed to verify the credentials with the platform")]
    Verify,
    #[error("no role {0:?} in this context")]
    UnknownRole(String),
    #[error("{0:?} is not a role URN (urn:…:role:…)")]
    InvalidRoleUrn(String),
    #[error("role alias {0:?} already exists")]
    RoleExists(String),
    #[error("failed to assume role {0}")]
    Assume(String),
    #[error("contexts {1} use the login of {0:?}: remove them first")]
    InUse(String, String),
    #[error("failed to access the context store")]
    Store,
    #[error("failed to access the credential store")]
    Credentials,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
