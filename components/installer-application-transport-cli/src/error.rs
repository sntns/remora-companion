#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to pack the installer")]
    Pack,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
