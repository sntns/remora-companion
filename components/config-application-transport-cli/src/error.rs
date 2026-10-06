#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to build the config image")]
    Build,
    #[error("failed to upload into the config image")]
    Upload,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
