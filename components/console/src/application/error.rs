#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open the serial port {0}")]
    Open(String),
    #[error("the serial port failed")]
    Serial,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
