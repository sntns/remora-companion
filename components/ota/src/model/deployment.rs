use std::time::SystemTime;

use super::Labels;

/// Asking one device to install one release.
#[derive(Debug, Clone)]
pub struct Deployment {
    pub name: String,
    pub labels: Labels,
    pub release: String,
    pub target: String,
    pub status: DeploymentStatus,
    pub details: Labels,
    pub updated_at: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct DeploymentSummary {
    pub name: String,
    pub status: DeploymentStatus,
}

/// Where a deployment is in its life. Kept open-ended: a status this version
/// doesn't know is shown as the platform spelled it, not rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentStatus {
    Pending,
    Running,
    Canceling,
    Succeeded,
    Failed,
    Canceled,
    Rejected,
    Other(String),
}

impl DeploymentStatus {
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_uppercase().as_str() {
            "PENDING" => Self::Pending,
            "RUNNING" => Self::Running,
            "CANCELING" | "CANCELLING" => Self::Canceling,
            "SUCCEEDED" => Self::Succeeded,
            "FAILED" => Self::Failed,
            "CANCELED" | "CANCELLED" => Self::Canceled,
            "REJECTED" => Self::Rejected,
            _ => Self::Other(value.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Pending => "PENDING",
            Self::Running => "RUNNING",
            Self::Canceling => "CANCELING",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Canceled => "CANCELED",
            Self::Rejected => "REJECTED",
            Self::Other(value) => value,
        }
    }

    /// Nothing more will happen to it.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Canceled | Self::Rejected
        )
    }

    /// Terminal and not the outcome asked for.
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed | Self::Canceled | Self::Rejected)
    }
}

impl std::fmt::Display for DeploymentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry of what the device reported while updating.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub execution: String,
    pub result: String,
    pub code: i32,
    pub details: Vec<String>,
    pub recorded_at: Option<SystemTime>,
    pub progress: Option<LogProgress>,
}

#[derive(Debug, Clone)]
pub struct LogProgress {
    pub current: i64,
    pub max: i64,
    /// e.g. "%" or "bytes".
    pub unit: String,
    pub speed: f64,
}

/// Which deployments to list; empty fields don't filter.
#[derive(Debug, Clone, Default)]
pub struct DeploymentFilter {
    pub device: Option<String>,
    pub release: Option<String>,
    pub labels: Labels,
}

/// Which devices a release goes to.
#[derive(Debug, Clone)]
pub enum Targets {
    /// These devices, by name (serial).
    Devices(Vec<String>),
    /// Every device carrying all these labels.
    Selector(Labels),
}

/// Deploying a release to a set of devices: one deployment per device.
#[derive(Debug, Clone)]
pub struct DeployRequest {
    pub release: String,
    pub targets: Targets,
    /// The deployment's name, for a single device; otherwise each is named
    /// `<release>-<device>`, lowercased.
    pub name: Option<String>,
    pub labels: Labels,
    /// Start each deployment right after creating it, instead of leaving it
    /// a draft.
    pub start: bool,
}

/// What `deploy` did for one device. A failure for one device doesn't stop
/// the others: a rollout to a hundred devices shouldn't hinge on one that
/// already has an update in flight.
#[derive(Debug)]
pub struct Planned {
    pub device: String,
    pub deployment: String,
    pub outcome: Result<bool, String>,
}
