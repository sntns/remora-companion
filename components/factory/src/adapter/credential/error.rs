#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to render remora-factory.yaml")]
    Render,
    #[error("failed to write {0}")]
    Write(std::path::PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
