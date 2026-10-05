#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Context(String),
    #[error("cancelled")]
    Cancelled,
    #[error("failed to read the terminal")]
    Prompt,
    #[error("failed to read the secret from stdin")]
    Stdin,
    #[error("failed to read {0}")]
    ReadFile(std::path::PathBuf),
    #[error("{0}")]
    Usage(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

/// A prompt's io error: Ctrl-C/Esc in cliclack is `Interrupted`, which is
/// the operator changing their mind, not a failure worth a stack.
pub(crate) fn prompt_error(error: std::io::Error) -> error_stack::Report<Error> {
    if error.kind() == std::io::ErrorKind::Interrupted {
        error_stack::Report::new(Error::Cancelled)
    } else {
        error_stack::Report::new(error).change_context(Error::Prompt)
    }
}

/// Lifts a context-service failure into this transport's error, keeping the
/// service's message as the headline (it is already the useful one, e.g.
/// "not logged in to context eu2: run `rmra login`"). The service spells
/// the commands it suggests as rmra's; they are the running binary's.
pub(crate) fn context_error(
    report: error_stack::Report<remora_context::application::Error>,
) -> error_stack::Report<Error> {
    let message = report
        .current_context()
        .to_string()
        .replace("`rmra ", &format!("`{} ", crate::program()));
    report.change_context(Error::Context(message))
}
