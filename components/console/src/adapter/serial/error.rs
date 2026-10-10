#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open {0}")]
    Open(String),
    #[error("the serial port failed")]
    Io,
    #[error("the serial port is closed")]
    Closed,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
