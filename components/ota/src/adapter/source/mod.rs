mod error;
mod service;

pub use error::{Error, Result};
pub use service::{ArtifactReader, ArtifactSourceAdapter, ArtifactSourceAdapterService};
