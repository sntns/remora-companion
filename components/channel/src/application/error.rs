#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Context(String),
    #[error("failed to open the channel to {0}")]
    Open(String),
    #[error("the {0:?} profile is a datagram channel, which this version cannot relay yet")]
    Datagram(String),
    #[error("{0}")]
    RefusedArgument(String),
    #[error("{0}")]
    Operands(String),
    #[error("failed to generate a throwaway ssh key")]
    Keygen,
    #[error("failed to have the ssh key certified for {0}")]
    Certify(String),
    #[error("{0} has no entry in the local certificate the platform issued")]
    LocalDevice(String),
    #[error("failed to have a login code issued for {0}")]
    LoginCode(String),
    #[error("failed to run the ssh client")]
    Ssh,
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
