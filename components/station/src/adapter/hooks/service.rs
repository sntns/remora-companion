use std::{path::PathBuf, time::Duration};

use remora_factory::adapter::provisioning::ProvisionedIdentity;

use super::error::Result;
use crate::model::{BoardPolicy, ClaimId, ClaimState, HardwareInfo, ImageInfo, LabelState};

/// When the station runs external hooks: the scripts of `<event>.d/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// The platform issued an identity (registering it elsewhere).
    Issued,
    /// A device became the one being labelled (printing its label). The
    /// only blocking one: its label can't be validated until these succeed.
    Label,
    /// Its label was validated (traceability, batch counters).
    Labelled,
    /// The device wrote its identity.
    Installed,
    /// The device rejected its identity, or the platform refused one.
    Failed,
}

impl HookEvent {
    /// `argv[1]` and `REMORA_EVENT`; the directory is `<name>.d`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Issued => "issued",
            Self::Label => "label",
            Self::Labelled => "labelled",
            Self::Installed => "installed",
            Self::Failed => "failed",
        }
    }

    pub fn directory(self) -> String {
        format!("{}.d", self.name())
    }
}

/// Everything known about a claim, for a hook: the device's request, what
/// was issued (none when the platform refused), and where it stands.
#[derive(Debug, Clone)]
pub struct ClaimRecord {
    pub claim_id: ClaimId,
    pub hardware: HardwareInfo,
    pub image: ImageInfo,
    pub policy: BoardPolicy,
    pub identity: Option<ProvisionedIdentity>,
    pub state: Option<ClaimState>,
    pub label: Option<LabelState>,
    /// Why it failed, for `failed`.
    pub reason: Option<String>,
}

/// One event's hooks to run, for one claim.
#[derive(Debug, Clone)]
pub struct HookInvocation {
    pub event: HookEvent,
    pub claim: ClaimRecord,
    /// The context the station issues as (`REMORA_CONTEXT`).
    pub context: String,
    /// 1, then 2, 3... on a reprint or when a device comes back.
    pub attempt: u32,
    /// The journal's path (`REMORA_JOURNAL`).
    pub journal: PathBuf,
}

/// Where the hooks are, how long each may take, and what to run.
#[derive(Debug, Clone)]
pub struct HookRun {
    /// Holds `<event>.d/`.
    pub dir: PathBuf,
    pub timeout: Duration,
    pub invocation: HookInvocation,
}

/// How one script went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookOutcome {
    /// `<event>.d/<script>`, as journaled.
    pub hook: String,
    /// None when it was killed (timeout, signal) or never started.
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    /// Includes why it never started, if so.
    pub stderr: String,
}

impl HookOutcome {
    pub fn succeeded(&self) -> bool {
        self.exit == Some(0)
    }
}

/// DI seam for `remora-station-application`: running an event's hook
/// scripts, run-parts style, and saying how each went.
#[async_trait::async_trait]
pub trait HookRunnerAdapter: Send + Sync {
    /// Whether the hooks directory `dir` can be read, before serving.
    async fn check(&self, dir: &std::path::Path) -> Result<()>;
    /// Runs the event's scripts one after the other, in lexical order,
    /// each with the claim on stdin; their outcomes in that order (none
    /// without an `<event>.d/`). A failing script doesn't stop the next.
    async fn run(&self, run: &HookRun) -> Result<Vec<HookOutcome>>;
}

/// Injectable handle to whatever `HookRunnerAdapter` was wired at startup.
#[derive(Clone)]
pub struct HookRunnerAdapterService(busybody::Service<Box<dyn HookRunnerAdapter>>);

impl HookRunnerAdapterService {
    pub fn new<T: HookRunnerAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for HookRunnerAdapterService {
    type Target = busybody::Service<Box<dyn HookRunnerAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
