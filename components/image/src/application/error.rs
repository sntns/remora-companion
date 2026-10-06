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
    #[error("failed to read from partition")]
    Read,
    #[error("failed to format {0}")]
    Format(PathBuf),
    #[error("failed to walk {0}")]
    Walk(PathBuf),
    #[error("unsupported filesystem entry: {0}")]
    UnsupportedEntry(PathBuf),
    #[error("copy was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
