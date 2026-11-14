use std::time::SystemTime;

use remora_context::model::ContextOverride;

use super::error::Result;
use crate::model::Document;

/// The monitor vertical's application-facing port. `bucket` is where the
/// platform stores what devices send it (see
/// [`DEFAULT_BUCKET`](crate::model::DEFAULT_BUCKET)).
#[async_trait::async_trait]
pub trait MonitorServiceInterface: Send + Sync {
    /// The latest of every monitoring document `device` reports, sorted
    /// by name.
    async fn latest(
        &self,
        over: Option<&ContextOverride>,
        bucket: &str,
        device: &str,
    ) -> Result<Vec<Document>>;

    /// Every `document` (`system`, `network`...) `device` reported since
    /// `from`, oldest first.
    async fn history(
        &self,
        over: Option<&ContextOverride>,
        bucket: &str,
        device: &str,
        document: &str,
        from: SystemTime,
    ) -> Result<Vec<Document>>;
}

#[derive(Clone)]
pub struct MonitorService(busybody::Service<Box<dyn MonitorServiceInterface>>);

impl MonitorService {
    pub fn new<T: MonitorServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for MonitorService {
    type Target = busybody::Service<Box<dyn MonitorServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
