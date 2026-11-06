#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Device(String),
    #[error("invalid --label")]
    Labels,
    #[error("cancelled")]
    Cancelled,
    #[error("failed to read the terminal")]
    Prompt,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

pub(crate) fn device_error(
    report: error_stack::Report<remora_device::application::Error>,
) -> error_stack::Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Device(message))
}

pub(crate) fn prompt_error(error: std::io::Error) -> error_stack::Report<Error> {
    if error.kind() == std::io::ErrorKind::Interrupted {
        error_stack::Report::new(Error::Cancelled)
    } else {
        error_stack::Report::new(error).change_context(Error::Prompt)
    }
}
