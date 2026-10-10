#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Console(String),
    #[error("the console needs a terminal: stdin and stdout must both be one")]
    NotATerminal,
    #[error("failed to drive the terminal")]
    Terminal,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

/// Lifts a console-service failure, keeping its message as the headline.
pub(crate) fn console_error(
    report: error_stack::Report<remora_console::application::Error>,
) -> error_stack::Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Console(message))
}
