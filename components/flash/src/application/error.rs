use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open image {0}")]
    OpenImage(String),
    #[error("failed to list the disk images of release {0:?}")]
    DiskImages(String),
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
    #[error(
        "refusing to flash {path}: the image is {}, the disk only {}",
        remora_format::human_size(*image),
        remora_format::human_size(*disk)
    )]
    ImageTooLarge {
        path: PathBuf,
        image: u64,
        disk: u64,
    },
    #[error("failed to flush the image to {0}")]
    Sync(PathBuf),
    #[error("flash was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
