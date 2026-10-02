#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read {0}")]
    Read(std::path::PathBuf),
    #[error("failed to write {0}")]
    Write(std::path::PathBuf),
    #[error("{0} is not a valid context file")]
    Parse(std::path::PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
