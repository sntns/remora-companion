#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to build the squashfs image")]
    Build,
    #[error("failed to inspect the squashfs image")]
    Inspect,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
