#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to generate ssh host key")]
    GenerateSshHostKey(#[from] ssh_key::Error),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
