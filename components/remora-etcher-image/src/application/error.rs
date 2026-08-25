use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read partition table")]
    ReadPartitionTable,
    #[error("failed to select partition")]
    SelectPartition,
    #[error("failed to read {0}")]
    ReadSource(PathBuf),
    #[error("failed to write into partition")]
    Write,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
