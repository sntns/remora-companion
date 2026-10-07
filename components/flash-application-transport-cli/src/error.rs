#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to look up target disk")]
    Disk,
    #[error("failed to flash image")]
    Flash,
    #[error("no usable context to download the image as")]
    Context,
    #[error("release {0:?} has no disk image to flash")]
    NoDiskImage(String),
    #[error("release {0:?} has disk images for several boards")]
    SeveralDiskImages(String),
    #[error("failed to read the disk image to flash")]
    Choose,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
