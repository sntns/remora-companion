use std::path::PathBuf;

/// Only what stops hooks from running at all: a script that fails, can't
/// start or times out is a `HookOutcome`, not an error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read the hooks directory {0}")]
    Unreadable(PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
