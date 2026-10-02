#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Ota(String),
    #[error("cancelled")]
    Cancelled,
    #[error("failed to read the terminal")]
    Prompt,
    #[error("invalid label {0:?}: expected key=value")]
    Label(String),
    #[error("{0} of {1} deployments did not succeed")]
    Unsuccessful(usize, usize),
    #[error("{0} of {1} deployments could not be created")]
    NotCreated(usize, usize),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

/// Lifts an ota-service failure, keeping its message as the headline.
pub(crate) fn ota_error(
    report: error_stack::Report<remora_ota::application::Error>,
) -> error_stack::Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Ota(message))
}

pub(crate) fn prompt_error(error: std::io::Error) -> error_stack::Report<Error> {
    if error.kind() == std::io::ErrorKind::Interrupted {
        error_stack::Report::new(Error::Cancelled)
    } else {
        error_stack::Report::new(error).change_context(Error::Prompt)
    }
}
