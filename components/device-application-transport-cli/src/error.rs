#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Device(String),
    #[error("invalid label {0:?}: expected key=value")]
    Label(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

pub(crate) fn device_error(
    report: error_stack::Report<remora_device::application::Error>,
) -> error_stack::Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Device(message))
}
