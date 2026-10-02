use remora_context::model::ContextOverride;

use super::error::Result;
use crate::model::{Device, Labels};

/// The device vertical's application-facing port.
#[async_trait::async_trait]
pub trait DeviceServiceInterface: Send + Sync {
    /// The account's devices carrying all of `labels` (all of them when
    /// empty), sorted by name; with their own labels when `with_labels`.
    async fn list(
        &self,
        over: Option<&ContextOverride>,
        labels: &Labels,
        with_labels: bool,
    ) -> Result<Vec<Device>>;
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
