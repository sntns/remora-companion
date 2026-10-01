use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to format {0}")]
    Format(PathBuf),
    #[error("failed to walk {0}")]
    Walk(PathBuf),
    #[error("failed to read {0}")]
    ReadFile(PathBuf),
    #[error("unsupported filesystem entry at {0}")]
    UnsupportedEntry(PathBuf),
    #[error("failed to populate {0}")]
    Populate(PathBuf),
    #[error("failed to read {0}")]
    ReadSource(PathBuf),
    #[error("failed to write {0}")]
    WriteTempImage(PathBuf),
    #[error("failed to read {0}")]
    ReadTempImage(PathBuf),
    #[error("failed to access the image's shared partition")]
    Image,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
