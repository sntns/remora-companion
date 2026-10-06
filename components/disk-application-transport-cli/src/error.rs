#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to list disks")]
    List,
    #[error("failed to look up disk")]
    Info,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
