#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to build identity.squashfs")]
    Build,
    #[error("failed to create the identity in the image")]
    Create,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
