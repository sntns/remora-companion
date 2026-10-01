use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read recipe file {0}")]
    ReadRecipe(PathBuf),
    #[error("failed to parse {0} as a batch recipe (a JSON array of steps)")]
    ParseRecipe(PathBuf),
    #[error("failed to run batch")]
    Batch,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
