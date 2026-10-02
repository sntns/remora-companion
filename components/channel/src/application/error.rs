#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Context(String),
    #[error("failed to open the channel to {0}")]
    Open(String),
    #[error("{0}")]
    RefusedArgument(String),
    #[error("{0}")]
    Operands(String),
    #[error("failed to generate a throwaway ssh key")]
    Keygen,
    #[error("failed to have the ssh key certified for {0}")]
    Certify(String),
    #[error("failed to prepare the ssh session's files")]
    Workdir,
    #[error("failed to run the ssh client")]
    Ssh,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
