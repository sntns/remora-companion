use std::time::Duration;

use remora_context::model::ContextOverride;
use remora_progress::OperationContext;

use super::error::Result;
use crate::adapter::gateway::ArtifactChunks;
use crate::model::{
    DeployRequest, Deployment, DeploymentFilter, DeploymentProgress, DeploymentStatus,
    DeploymentSummary, DownloadOutcome, DownloadRequest, Labels, LogEntry, Planned, Release,
    ReleaseSummary, Targets, UploadOutcome, UploadRequest, WatchOutcome,
};

/// The ota vertical's application-facing port: over-the-air updates as an
/// operator runs them -- publish a release, deploy it to devices, follow it.
/// Every call runs under the selected context.
#[async_trait::async_trait]
pub trait OtaServiceInterface: Send + Sync {
    async fn list_releases(
        &self,
        over: Option<&ContextOverride>,
        labels: &Labels,
    ) -> Result<Vec<ReleaseSummary>>;
    async fn get_release(&self, over: Option<&ContextOverride>, name: &str) -> Result<Release>;
    async fn create_release(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
        version: &str,
        labels: &Labels,
    ) -> Result<()>;
    async fn label_release(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
        set: &Labels,
        unset: &[String],
    ) -> Result<()>;
    async fn delete_release(&self, over: Option<&ContextOverride>, name: &str) -> Result<()>;
    /// Uploads a file as an artifact, reporting bytes through `ctx`. Resumes
    /// from where the platform got to: on its own after a dropped stream,
    /// and across runs with [`UploadRequest::resume`]. Cancelled through
    /// `ctx`, it fails with [`super::Error::Cancelled`], which carries the
    /// token to resume with; nothing is committed either way.
    async fn upload(
        &self,
        over: Option<&ContextOverride>,
        request: UploadRequest,
        ctx: &OperationContext,
    ) -> Result<UploadOutcome>;
    /// Streams the artifact `file_name` of `release` from byte `offset` to
    /// its end. A range of it is a fresh call from another offset: reading
    /// one like a file means one call per jump.
    async fn download(
        &self,
        over: Option<&ContextOverride>,
        release: &str,
        file_name: &str,
        offset: u64,
    ) -> Result<Box<dyn ArtifactChunks>>;
    /// Downloads an artifact to a local file, reporting bytes through
    /// `ctx`: into a partial file next to it, picked up again where it
    /// stopped after a dropped connection and, across runs, after an
    /// interrupted one; checked against the release's sha256 before it
    /// becomes the file. Cancelled through `ctx`, it fails with
    /// [`super::Error::DownloadCancelled`], the partial file kept.
    async fn download_file(
        &self,
        over: Option<&ContextOverride>,
        request: DownloadRequest,
        ctx: &OperationContext,
    ) -> Result<DownloadOutcome>;

    /// The devices `targets` names: as given, or every device matching the
    /// selector. Lets a transport show (and confirm) a rollout's reach --
    /// then deploy to exactly those, as [`Targets::Devices`], so that a
    /// device labelled in the meantime isn't updated unasked.
    async fn resolve_targets(
        &self,
        over: Option<&ContextOverride>,
        targets: &Targets,
    ) -> Result<Vec<String>>;

    /// Creates (and optionally starts) one deployment per target device.
    async fn deploy(
        &self,
        over: Option<&ContextOverride>,
        request: DeployRequest,
    ) -> Result<Vec<Planned>>;
    async fn list_deployments(
        &self,
        over: Option<&ContextOverride>,
        filter: &DeploymentFilter,
    ) -> Result<Vec<DeploymentSummary>>;
    async fn get_deployment(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
    ) -> Result<Deployment>;
    async fn deployment_logs(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
    ) -> Result<Vec<LogEntry>>;
    /// Polls deployments every `interval` until each is settled (see
    /// [`DeploymentStatus::is_settled`]), calling `on_progress` whenever one
    /// is polled. Cancelling `ctx` stops watching (not the deployments):
    /// those still going are returned as pending.
    async fn watch(
        &self,
        over: Option<&ContextOverride>,
        names: &[String],
        interval: Duration,
        ctx: &OperationContext,
        on_progress: &mut (dyn FnMut(DeploymentProgress) + Send),
    ) -> Result<WatchOutcome>;
    /// Calls `on_entry` with each report of the device, oldest first, as
    /// they arrive, polling every `interval` until the deployment is
    /// settled; returns its status then, or `None` once `ctx` is cancelled.
    async fn follow_logs(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
        interval: Duration,
        ctx: &OperationContext,
        on_entry: &mut (dyn FnMut(LogEntry) + Send),
    ) -> Result<Option<DeploymentStatus>>;
    async fn start_deployment(&self, over: Option<&ContextOverride>, name: &str) -> Result<()>;
    async fn cancel_deployment(&self, over: Option<&ContextOverride>, name: &str) -> Result<()>;
    async fn delete_deployment(&self, over: Option<&ContextOverride>, name: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct OtaService(busybody::Service<Box<dyn OtaServiceInterface>>);

impl OtaService {
    pub fn new<T: OtaServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for OtaService {
    type Target = busybody::Service<Box<dyn OtaServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
