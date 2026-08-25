use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid build options")]
    InvalidOptions,
    #[error("no input files or directories given")]
    NoInputs,
    #[error("failed to walk input")]
    Walk,
    #[error("failed to build squashfs image")]
    Build,
    #[error("failed to create {0}")]
    CreateOutput(PathBuf),
    #[error("failed to open {0}")]
    OpenInput(PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
