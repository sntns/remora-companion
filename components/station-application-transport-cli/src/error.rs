use std::{net::SocketAddr, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read the station configuration {0}")]
    ReadConfig(PathBuf),
    /// Which key or option is wrong; why is the cause, or attached.
    #[error("invalid station configuration: {0}")]
    Config(String),
    #[error("failed to start the station")]
    Start,
    #[error("failed to listen on {0}")]
    Listen(SocketAddr),
    #[error("the station's HTTP server failed")]
    Serve,
    #[error("the labelling console failed")]
    Operate,
    #[error("the simulated hub did not get its identity")]
    Simulate,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
