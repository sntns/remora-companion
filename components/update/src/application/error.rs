#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to find the latest release")]
    Check,
    #[error("installed with Homebrew: update with `brew upgrade {0}`")]
    Homebrew(String),
    #[error("not installed by the {0} installer, so not updated in place (pass --force to install the release over it anyway)")]
    Unmanaged(String),
    #[error("failed to install {0}")]
    Install(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
