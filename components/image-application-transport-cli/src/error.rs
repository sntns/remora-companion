#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("'{0}' is not a partition index or a known role (shared/efi/slota/slotb/data)")]
    ParseSelector(String),
    #[error("failed to run image command")]
    Image,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
