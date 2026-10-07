#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Install(String),
    #[error("failed to read the release")]
    Release,
    #[error("failed to pick the bundle")]
    Choose,
    #[error("failed to set up the ssh session")]
    Ssh,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
