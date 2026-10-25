use std::time::SystemTime;

use remora_context::model::ResolvedContext;

use super::error::Result;
use crate::model::{
    Deployment, DeploymentFilter, DeploymentStatus, DeploymentSummary, Labels, LogEntry, Release,
    ReleaseSummary,
};

/// The opening of an artifact upload.
#[derive(Debug, Clone)]
pub struct InitialUpload {
    pub release: String,
    pub file_name: String,
    pub content_type: String,
    pub tag_condition: String,
    pub content_id: String,
    /// The whole artifact's length and SHA-256 (hex): the platform commits
    /// only what matches them.
    pub content_length: u64,
    pub checksum_sha256: String,
}

/// An artifact upload in flight: chunks go in order, from wherever the
/// platform got to (`upload_offset`); `finish` commits it.
///
/// Whatever reaches the platform is kept for a resume with the same content
/// id; only `finish` commits, and only an artifact of the announced length
/// and checksum. An upload that stops short ends with
/// [`ArtifactSink::abort`] (or by dropping the sink, which does the same
/// without waiting).
#[async_trait::async_trait]
pub trait ArtifactSink: Send {
    async fn send(&mut self, chunk: Vec<u8>) -> Result<()>;
    /// Delivers what is left, then commits the artifact.
    async fn finish(self: Box<Self>) -> Result<()>;
    /// Stops sending: the platform keeps what it received for a resume
    /// with the same content id, and commits nothing.
    async fn abort(self: Box<Self>);
}

/// An artifact download in flight: its bytes, in order, a chunk at a time.
/// Dropping it cancels the call.
#[async_trait::async_trait]
pub trait ArtifactChunks: Send {
    /// The next chunk; `None` once the requested bytes are all in.
    async fn next(&mut self) -> Result<Option<Vec<u8>>>;
}

/// What a deployment's live feed says.
#[derive(Debug, Clone)]
pub enum DeploymentEvent {
    /// Where it is now: the first event, then each change.
    Status {
        status: DeploymentStatus,
        details: Labels,
        updated_at: Option<SystemTime>,
    },
    /// A report of the device's, new since the feed opened.
    Log(LogEntry),
}

/// A deployment's live feed: its events as they happen, `None` once it is
/// settled and the platform ended the feed. A deployment deleted meanwhile
/// is a [`super::Error::NotFound`]; a feed cut short, an
/// [`super::Error::Unreachable`] -- open another.
#[async_trait::async_trait]
pub trait DeploymentFeed: Send {
    async fn next(&mut self) -> Result<Option<DeploymentEvent>>;
}

/// sntns-platform's remora gateway, as far as over-the-air updates need it:
/// releases, their artifacts, and deployments. Devices to target come from
/// the device vertical.
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

    /// A fresh resume token for a new upload, in the form the platform
    /// expects. Here rather than in the use case: what a token looks like
    /// is the platform's contract, and making one takes randomness.
    fn new_content_id(&self) -> String;
    /// How many bytes the platform already holds for an upload's resume token.
    async fn upload_offset(&self, context: &ResolvedContext, content_id: &str) -> Result<u64>;
    /// Starts sending an artifact's bytes from `offset`: what the platform
    /// holds for its content id (see `upload_offset`).
    async fn begin_upload(
        &self,
        context: &ResolvedContext,
        upload: InitialUpload,
        offset: u64,
    ) -> Result<Box<dyn ArtifactSink>>;
    /// Streams `file_name` of `release`, from byte `offset` to its end.
    async fn download(
        &self,
        context: &ResolvedContext,
        release: &str,
        file_name: &str,
        offset: u64,
    ) -> Result<Box<dyn ArtifactChunks>>;

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
    /// Opens a deployment's live feed. Reports from before it opened are
    /// not in it: read them with `deployment_logs`.
    async fn follow_deployment(
        &self,
        context: &ResolvedContext,
        name: &str,
    ) -> Result<Box<dyn DeploymentFeed>>;
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
