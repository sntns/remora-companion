#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to enumerate disks")]
    Enumerate,
    #[error("failed to look up disk")]
    Info,
    #[error("failed to open disk")]
    Open,
    #[error("failed to unmount disk")]
    Hold,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
