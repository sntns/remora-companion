use remora_context::model::ResolvedContext;

use super::error::Result;
use crate::model::{
    Deployment, DeploymentFilter, DeploymentSummary, Labels, LogEntry, Release, ReleaseSummary,
};

/// The opening of an artifact upload.
#[derive(Debug, Clone)]
pub struct InitialUpload {
    pub release: String,
    pub file_name: String,
    pub content_type: String,
    pub tag_condition: String,
    pub content_id: String,
}

/// An artifact upload in flight: chunks go in order, `finish` commits it.
#[async_trait::async_trait]
pub trait ArtifactSink: Send {
    async fn send(&mut self, chunk: Vec<u8>) -> Result<()>;
    async fn finish(self: Box<Self>) -> Result<()>;
}

/// sntns-platform's remora gateway, as far as over-the-air updates need it:
/// releases, their artifacts, deployments, and devices to target.
#[async_trait::async_trait]
pub trait OtaGatewayAdapter: Send + Sync {
    async fn list_releases(
        &self,
        context: &ResolvedContext,
        labels: &Labels,
    ) -> Result<Vec<ReleaseSummary>>;
    async fn get_release(&self, context: &ResolvedContext, name: &str) -> Result<Release>;
    async fn create_release(
        &self,
        context: &ResolvedContext,
        name: &str,
        version: &str,
        labels: &Labels,
    ) -> Result<()>;
    /// Merges `set` into the release's labels and removes `unset`.
    async fn update_release_labels(
        &self,
        context: &ResolvedContext,
        name: &str,
        set: &Labels,
        unset: &[String],
    ) -> Result<()>;
    async fn delete_release(&self, context: &ResolvedContext, name: &str) -> Result<()>;

    /// How many bytes the platform already holds for an upload's resume token.
    async fn upload_offset(&self, context: &ResolvedContext, content_id: &str) -> Result<u64>;
    async fn begin_upload(
        &self,
        context: &ResolvedContext,
        upload: InitialUpload,
    ) -> Result<Box<dyn ArtifactSink>>;

    async fn list_devices(&self, context: &ResolvedContext, labels: &Labels)
        -> Result<Vec<String>>;

    async fn create_deployment(
        &self,
        context: &ResolvedContext,
        name: &str,
        release: &str,
        device: &str,
        labels: &Labels,
    ) -> Result<()>;
    async fn get_deployment(&self, context: &ResolvedContext, name: &str) -> Result<Deployment>;
    async fn list_deployments(
        &self,
        context: &ResolvedContext,
        filter: &DeploymentFilter,
    ) -> Result<Vec<DeploymentSummary>>;
    async fn start_deployment(&self, context: &ResolvedContext, name: &str) -> Result<()>;
    async fn cancel_deployment(&self, context: &ResolvedContext, name: &str) -> Result<()>;
    async fn delete_deployment(&self, context: &ResolvedContext, name: &str) -> Result<()>;
    /// What the device reported, oldest first.
    async fn deployment_logs(&self, context: &ResolvedContext, name: &str)
        -> Result<Vec<LogEntry>>;
}

#[derive(Clone)]
pub struct OtaGatewayAdapterService(busybody::Service<Box<dyn OtaGatewayAdapter>>);

impl OtaGatewayAdapterService {
    pub fn new<T: OtaGatewayAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for OtaGatewayAdapterService {
    type Target = busybody::Service<Box<dyn OtaGatewayAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
