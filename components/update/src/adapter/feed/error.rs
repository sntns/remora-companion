#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to reach the release feed")]
    Unreachable,
    #[error("no release has been published yet")]
    NoRelease,
    #[error("the release feed answered something unexpected")]
    Parse,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
