//! Release artifacts for the flash vertical, through the ota vertical's
//! application port: which of a release's artifacts are disk images, and
//! their bytes from any offset.

mod service;

pub use service::ReleaseArtifactAdapterImpl;
