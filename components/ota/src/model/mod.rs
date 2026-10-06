mod deployment;
mod release;

pub use deployment::{
    DeployRequest, Deployment, DeploymentFilter, DeploymentProgress, DeploymentStatus,
    DeploymentSummary, LogEntry, LogProgress, Planned, PlannedOutcome, Targets, WatchOutcome,
};
pub use release::{Artifact, Release, ReleaseSummary, UploadOutcome, UploadRequest};

/// A label map, sorted for stable output.
pub type Labels = std::collections::BTreeMap<String, String>;
