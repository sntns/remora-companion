use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to create {0}")]
    CreateScratchDir(PathBuf),
    #[error("failed to write {0}")]
    WriteScratchFile(PathBuf),
    #[error("failed to build identity.squashfs")]
    Squashfs,
    #[error("failed to read {0}")]
    ReadBuiltImage(PathBuf),
    #[error("failed to generate a default identity value")]
    Keygen,
    #[error("failed to write identity.squashfs to the shared partition")]
    Image,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
