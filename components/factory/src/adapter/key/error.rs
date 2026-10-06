#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to generate a device keypair")]
    Keygen,
    #[error("failed to build a certificate signing request")]
    Csr,
    #[error("invalid certificate signing request")]
    InvalidCsr,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
