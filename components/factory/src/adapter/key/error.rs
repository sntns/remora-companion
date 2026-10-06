#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to generate a device keypair")]
    Keygen,
    #[error("failed to build a certificate signing request")]
    Csr,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
