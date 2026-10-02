#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to look up target disk")]
    Disk,
    #[error("failed to flash image")]
    Flash,
    #[error("I/O error while confirming the target device")]
    Confirm,
    #[error("nobody at the terminal to confirm overwriting the device: pass --yes")]
    Unattended,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
