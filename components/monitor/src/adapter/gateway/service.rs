use std::time::SystemTime;

use remora_context::model::ResolvedContext;

use super::error::Result;
use crate::model::Sample;

/// sntns-platform's time series store (tbss), as far as what devices
/// report goes: a bucket holds, per device and schema (a kind of
/// message), the messages over time and the latest one.
#[async_trait::async_trait]
pub trait MonitorGatewayAdapter: Send + Sync {
    /// The schemas `device` has a latest message of, as the store names
    /// them; none for a device it knows nothing of.
    async fn schemas(
        &self,
        context: &ResolvedContext,
        bucket: &str,
        device: &str,
    ) -> Result<Vec<String>>;
    /// The latest message of `device` in `schema` (as [`schemas`] names
    /// it), if any.
    ///
    /// [`schemas`]: MonitorGatewayAdapter::schemas
    async fn latest(
        &self,
        context: &ResolvedContext,
        bucket: &str,
        device: &str,
        schema: &str,
    ) -> Result<Option<Sample>>;
    /// The messages of `device` in `schema` stored between `from` and
    /// `to`, oldest first.
    async fn points(
        &self,
        context: &ResolvedContext,
        bucket: &str,
        device: &str,
        schema: &str,
        from: SystemTime,
        to: SystemTime,
    ) -> Result<Vec<Sample>>;
}

#[derive(Clone)]
pub struct MonitorGatewayAdapterService(busybody::Service<Box<dyn MonitorGatewayAdapter>>);

impl MonitorGatewayAdapterService {
    pub fn new<T: MonitorGatewayAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for MonitorGatewayAdapterService {
    type Target = busybody::Service<Box<dyn MonitorGatewayAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
