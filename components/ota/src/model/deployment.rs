use std::time::SystemTime;

use super::Labels;
use crate::application::Error;

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

    /// Following it can stop: it is terminal, or this version doesn't know
    /// the status at all. Polling an unknown status could otherwise last
    /// forever (the platform may have added a terminal one); stopping
    /// early instead is the safe side, and it counts as not succeeded.
    pub fn is_settled(&self) -> bool {
        self.is_terminal() || matches!(self, Self::Other(_))
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
    pub outcome: PlannedOutcome,
}

#[derive(Debug)]
pub enum PlannedOutcome {
    Started,
    /// Created and left a draft, as asked.
    Draft,
    /// The whole chain, so that the platform's reason shows as it does for
    /// any other failure (and in full with `--verbose`).
    NotCreated(error_stack::Report<Error>),
    /// Created, but the start was refused: it is left a draft.
    NotStarted(error_stack::Report<Error>),
}

impl PlannedOutcome {
    pub fn failure(&self) -> Option<&error_stack::Report<Error>> {
        match self {
            Self::NotCreated(report) | Self::NotStarted(report) => Some(report),
            Self::Started | Self::Draft => None,
        }
    }
}

/// Where a followed deployment is, as an operator wants to see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentProgress {
    pub name: String,
    pub status: DeploymentStatus,
    /// 0-100, from the latest report that gave a progress.
    pub percent: Option<u64>,
    /// Why it failed, else the latest thing the device said.
    pub detail: String,
}

impl DeploymentProgress {
    pub fn of(deployment: &Deployment, logs: &[LogEntry]) -> Self {
        let percent = if deployment.status == DeploymentStatus::Succeeded {
            Some(100)
        } else {
            logs.iter()
                .rev()
                .find_map(|entry| entry.progress.as_ref())
                .filter(|progress| progress.max > 0)
                .map(|progress| {
                    (progress.current.clamp(0, progress.max) * 100 / progress.max) as u64
                })
        };
        let detail = if deployment.status.is_failure() && !deployment.details.is_empty() {
            deployment
                .details
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            logs.iter()
                .rev()
                .find_map(|entry| entry.details.last().cloned())
                .unwrap_or_default()
        };
        Self {
            name: deployment.name.clone(),
            status: deployment.status.clone(),
            percent,
            detail,
        }
    }
}

/// How following deployments ended, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchOutcome {
    pub succeeded: Vec<String>,
    /// Failed, cancelled or rejected.
    pub failed: Vec<String>,
    /// Stopped at a status this version doesn't know.
    pub unknown: Vec<String>,
    /// Still going when the watch was cancelled.
    pub pending: Vec<String>,
}
