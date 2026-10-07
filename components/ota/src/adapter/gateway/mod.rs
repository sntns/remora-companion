mod error;
mod service;

pub use error::{Error, Result};
pub use service::{
    ArtifactChunks, ArtifactSink, InitialUpload, OtaGatewayAdapter, OtaGatewayAdapterService,
};
