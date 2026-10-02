mod error;
mod service;

pub use error::{Error, Result};
pub use service::{ArtifactSink, InitialUpload, OtaGatewayAdapter, OtaGatewayAdapterService};
