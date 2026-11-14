#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Monitor(String),
    #[error("`watch` needs a terminal: use `status` or `graph` in a script")]
    NotATerminal,
    #[error("failed to drive the terminal")]
    Terminal,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

pub(crate) fn monitor_error(
    report: error_stack::Report<remora_monitor::application::Error>,
) -> error_stack::Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Monitor(message))
}
