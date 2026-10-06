use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open image {0}")]
    OpenImage(PathBuf),
    #[error("failed to read .bmap file {0}")]
    OpenBmap(PathBuf),
    #[error("failed to open device {0}")]
    OpenDevice(PathBuf),
    #[error("failed to look up target disk {0}")]
    Disk(PathBuf),
    #[error("failed to copy image")]
    Bmap,
    #[error("refusing to flash {path}: {reason}")]
    UnsafeTarget { path: PathBuf, reason: &'static str },
    #[error("refusing to flash {device}: it opens {resolved}, not the checked disk {checked}")]
    TargetMismatch {
        device: PathBuf,
        resolved: PathBuf,
        checked: PathBuf,
    },
    #[error("flash was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
