use std::{net::SocketAddr, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read the station configuration {0}")]
    ReadConfig(PathBuf),
    #[error("invalid station configuration: {0}")]
    Config(String),
    #[error("{0}")]
    Station(String),
    #[error("failed to listen on {0}")]
    Listen(SocketAddr),
    #[error("the station's HTTP server failed")]
    Serve,
    #[error("the station at {0} is not a remora station")]
    NotAStation(String),
    #[error("{0}")]
    Claim(String),
    #[error("failed to generate the device key")]
    Keygen,
    #[error("failed to write {0}")]
    WriteOutput(PathBuf),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

/// Lifts a station-service failure, keeping its message as the headline
/// (e.g. which context isn't logged in, or which journal won't load).
pub(crate) fn station_error(
    report: error_stack::Report<remora_station::application::Error>,
) -> error_stack::Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Station(message))
}
