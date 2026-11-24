use remora_context::model::ResolvedContext;

use super::error::Result;
use crate::model::{Device, FactoryDevice, Labels};

/// sntns-platform's remora gateway, as far as devices go.
#[async_trait::async_trait]
pub trait DeviceGatewayAdapter: Send + Sync {
    /// The names of the devices carrying all of `labels`, sorted.
    async fn list(&self, context: &ResolvedContext, labels: &Labels) -> Result<Vec<String>>;
    /// One device, with its labels and when it enrolled.
    async fn get(&self, context: &ResolvedContext, name: &str) -> Result<Device>;
    /// Deletes the device from the account: its operational record only,
    /// never its factory one.
    async fn delete(&self, context: &ResolvedContext, name: &str) -> Result<()>;

    /// The devices the account manufactured carrying all of `labels`,
    /// with their owner, sorted by name.
    async fn list_factory(
        &self,
        context: &ResolvedContext,
        labels: &Labels,
    ) -> Result<Vec<FactoryDevice>>;
    /// One manufactured device, in full.
    async fn get_factory(&self, context: &ResolvedContext, name: &str) -> Result<FactoryDevice>;
    /// Deletes a manufactured device's factory record and revokes the
    /// certificate it was built with: the URNs of those revoked.
    async fn delete_factory(&self, context: &ResolvedContext, name: &str) -> Result<Vec<String>>;
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
