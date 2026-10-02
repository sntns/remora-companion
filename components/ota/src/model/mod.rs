mod deployment;
mod release;

pub use deployment::{
    DeployRequest, Deployment, DeploymentFilter, DeploymentStatus, DeploymentSummary, LogEntry,
    LogProgress, Planned, Targets,
};
pub use release::{Artifact, Release, ReleaseSummary, UploadOutcome, UploadRequest};

/// A label map, sorted for stable output.
pub type Labels = std::collections::BTreeMap<String, String>;
