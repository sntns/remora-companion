#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no usable context")]
    Context,
    #[error("failed to read the monitoring data of device {0:?}")]
    Read(String),
    #[error("not allowed to read monitoring data (bucket {0:?})")]
    Denied(String),
    #[error("no monitoring data for device {0:?}")]
    NoData(String),
    #[error("device {device:?} reports no {document:?} document")]
    NoDocument { device: String, document: String },
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
