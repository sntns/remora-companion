#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Update(String),
    #[error("this build's version {0:?} is not a release version, so it can't be compared to one")]
    Version(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

pub(crate) fn update_error(
    report: error_stack::Report<remora_update::application::Error>,
) -> error_stack::Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Update(message))
}
