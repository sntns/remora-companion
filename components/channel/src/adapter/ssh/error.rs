#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to start {0}")]
    Spawn(std::path::PathBuf),
    #[error("failed waiting for ssh to finish")]
    Wait,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
