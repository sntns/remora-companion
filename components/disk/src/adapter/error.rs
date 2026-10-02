#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error while enumerating disks")]
    Io,
    #[error("no such disk: {0}")]
    NotFound(std::path::PathBuf),
    #[error("disk enumeration is not yet implemented on this OS")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
