#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to download the installer")]
    Download,
    #[error("the installer failed")]
    Install,
    #[error("failed to inspect the installation")]
    Inspect,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
