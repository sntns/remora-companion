use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to set up a scratch directory next to {0}")]
    Scratch(PathBuf),
    #[error("failed to read {0}")]
    Read(PathBuf),
    #[error("failed to decode the installer {0}")]
    DecodeInstaller(PathBuf),
    #[error("failed to bundle {0} as a .bmaptar")]
    BundleImage(PathBuf),
    #[error("{0} is named as a .bmaptar but isn't one (no tar header)")]
    NotBmaptar(PathBuf),
    #[error("failed to put the image into the installer's payload partition")]
    Embed,
    #[error("failed to encode {0}")]
    Encode(PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
