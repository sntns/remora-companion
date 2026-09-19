#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to generate a device keypair")]
    Keygen,
    #[error("failed to build a certificate signing request")]
    Csr,
    #[error("failed to provision a factory device credential")]
    Provision,
    #[error(
        "the platform did not return an access URL for this deployment, and no --access-url override was given"
    )]
    MissingAccessUrl,
    #[error("failed to render remora-factory.yaml")]
    Render,
    #[error("failed to write {0}")]
    WriteOutput(std::path::PathBuf),
    #[error("factory provisioning was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
