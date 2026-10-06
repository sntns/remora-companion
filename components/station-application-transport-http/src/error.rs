#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to serve the station's HTTP API")]
    Serve,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
