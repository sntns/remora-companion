#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read {0}")]
    Bundle(std::path::PathBuf),
    #[error("failed to download the bundle")]
    Download,
    #[error("failed to reach {0} over its channel")]
    Channel(String),
    #[error("{0} has no remora-otactl: it can't install a bundle this way")]
    NoInstaller(String),
    #[error("failed to upload the bundle to {0}")]
    Upload(String),
    #[error("the bundle on {device} doesn't match: it was removed (run again to upload it anew)")]
    Checksum { device: String },
    #[error("the install on {0} failed")]
    Install(String),
    #[error("{0} didn't come back after rebooting")]
    RebootTimeout(String),
    #[error("{device} came back on slot {slot}, not the new one: it didn't boot")]
    OldSlot { device: String, slot: String },
    #[error("failed to validate the new slot on {0}")]
    Validate(String),
    #[error("cancelled (run again to resume the upload)")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
