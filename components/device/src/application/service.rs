use remora_context::model::ContextOverride;

use super::error::Result;
use crate::model::{Device, FactoryDevice, Labels};

/// The device vertical's application-facing port.
#[async_trait::async_trait]
pub trait DeviceServiceInterface: Send + Sync {
    /// The account's devices carrying all of `labels` (all of them when
    /// empty), sorted by name; with their own labels and when they enrolled
    /// when `with_labels` (a call per device).
    async fn list(
        &self,
        over: Option<&ContextOverride>,
        labels: &Labels,
        with_labels: bool,
    ) -> Result<Vec<Device>>;

    /// One of the account's devices, with its labels and when it enrolled.
    async fn get(&self, over: Option<&ContextOverride>, name: &str) -> Result<Device>;

    /// Deletes a device from the platform -- its operational record, not
    /// its factory one: it is no longer one of the account's devices, and
    /// can no longer reach the platform as one, until it enrolls again.
    async fn delete(&self, over: Option<&ContextOverride>, name: &str) -> Result<()>;

    /// The devices the account manufactured carrying all of `labels` (all
    /// of them when empty), sorted by name, with their owner. With
    /// `without_device`, only those no device of the account has the name
    /// of yet: made, but not enrolled (or deleted since) -- whatever their
    /// labels, a device of that name excludes its factory record.
    async fn list_factory(
        &self,
        over: Option<&ContextOverride>,
        labels: &Labels,
        without_device: bool,
    ) -> Result<Vec<FactoryDevice>>;

    /// One manufactured device, with its labels and certificate.
    async fn get_factory(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
    ) -> Result<FactoryDevice>;

    /// Deletes a manufactured device's factory record and revokes the
    /// certificate it was built with -- for hardware that never shipped:
    /// it can no longer enroll. Its operational record, if any, stays.
    /// Returns the URNs of the certificates revoked.
    async fn delete_factory(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
    ) -> Result<Vec<String>>;
}

#[derive(Clone)]
pub struct DeviceService(busybody::Service<Box<dyn DeviceServiceInterface>>);

impl DeviceService {
    pub fn new<T: DeviceServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for DeviceService {
    type Target = busybody::Service<Box<dyn DeviceServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
