#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to look up target disk")]
    Disk,
    #[error("failed to flash image")]
    Flash,
    #[error("no usable context to download the image as")]
    Context,
    #[error("release {0:?} has no {1} image to flash")]
    NoDiskImage(String, String),
    #[error("release {0:?} has {1} images for several boards")]
    SeveralDiskImages(String, String),
    #[error("failed to read the disk image to flash")]
    Choose,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
