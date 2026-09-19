#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to generate a device keypair")]
    Keygen,
    #[error("failed to build a certificate signing request")]
    Csr,
    #[error("failed to provision a factory device credential")]
    Provision,
    #[error("failed to write the factory credential into the image")]
    Image,
    #[error("factory provisioning was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
