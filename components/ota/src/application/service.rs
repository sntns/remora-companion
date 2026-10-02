use remora_context::model::ContextOverride;
use remora_progress::OperationContext;

use super::error::Result;
use crate::model::{
    DeployRequest, Deployment, DeploymentFilter, DeploymentSummary, Labels, LogEntry, Planned,
    Release, ReleaseSummary, Targets, UploadOutcome, UploadRequest,
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
    /// and across runs with [`UploadRequest::resume`].
    async fn upload(
        &self,
        over: Option<&ContextOverride>,
        request: UploadRequest,
        ctx: &OperationContext,
    ) -> Result<UploadOutcome>;

    /// The devices `targets` names: as given, or every device matching the
    /// selector. Lets a transport show (and confirm) a rollout's reach.
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
