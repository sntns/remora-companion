#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid context name {0:?}: use letters, digits, '.', '_', '+' or '-', starting with a letter or digit")]
    InvalidName(String),
    #[error("context {0:?} already exists")]
    AlreadyExists(String),
    #[error("context {0:?} does not exist")]
    NotFound(String),
    #[error(
        "no context selected: create one with `rmra context create`, or log in with `rmra login`"
    )]
    NoContext,
    #[error("several contexts exist and none is current: pick one with `rmra context use <name>` or --context")]
    Ambiguous,
    #[error("not logged in to context {0:?}: run `rmra login`")]
    NotLoggedIn(String),
    #[error("failed to verify the credentials with the platform")]
    Verify,
    #[error("no role {0:?} in this context: add it with `rmra role add {0} <role-urn>`, or give the role's URN")]
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
