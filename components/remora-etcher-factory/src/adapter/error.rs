#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to reach the platform's factory-device endpoint")]
    Request,
    #[error("factory-device provisioning was refused: {0}")]
    Refused(String),
    #[error("failed to parse the platform's response")]
    ParseResponse,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
