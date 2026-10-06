#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid device serial")]
    InvalidSerial,
    #[error("{0}")]
    Provision(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

/// Lifts a factory-service failure, keeping its message as the headline
/// (e.g. which context isn't logged in, or the missing access URL).
pub(crate) fn provision_error(
    report: error_stack::Report<remora_factory::application::Error>,
) -> error_stack::Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Provision(message))
}
