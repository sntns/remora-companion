use remora_context::model::ResolvedContext;

use super::error::Result;
use crate::model::Labels;

/// sntns-platform's remora gateway, as far as devices go.
#[async_trait::async_trait]
pub trait DeviceGatewayAdapter: Send + Sync {
    /// The names of the devices carrying all of `labels`, sorted.
    async fn list(&self, context: &ResolvedContext, labels: &Labels) -> Result<Vec<String>>;
    /// One device's labels.
    async fn labels(&self, context: &ResolvedContext, name: &str) -> Result<Labels>;
}

#[derive(Clone)]
pub struct DeviceGatewayAdapterService(busybody::Service<Box<dyn DeviceGatewayAdapter>>);

impl DeviceGatewayAdapterService {
    pub fn new<T: DeviceGatewayAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for DeviceGatewayAdapterService {
    type Target = busybody::Service<Box<dyn DeviceGatewayAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
