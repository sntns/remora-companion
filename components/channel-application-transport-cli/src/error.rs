#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Channel(String),
    #[error("the connection to {0} was lost")]
    Relay(String),
    #[error("the channel did not finish within {0} s of a hangup")]
    HangupGrace(u64),
    #[error("cannot tell where this program is, to use it as ssh's ProxyCommand")]
    SelfPath,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

/// Lifts a channel-service failure, keeping its message as the headline.
pub(crate) fn channel_error(
    report: error_stack::Report<remora_channel::application::Error>,
) -> error_stack::Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Channel(message))
}
