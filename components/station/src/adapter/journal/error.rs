use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read the journal {0}")]
    Read(PathBuf),
    #[error("line {line} of the journal {path} is not a journal entry")]
    Parse { path: PathBuf, line: usize },
    #[error("failed to append to the journal {0}")]
    Write(PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
