use std::path::Path;

use remora_etcher_progress::OperationContext;

use super::error::Result;

/// The factory vertical's application-facing port: manufacture a device --
/// generate a local keypair + CSR, request a factory-device credential
/// from `sntns-platform`, and inject everything the device needs to
/// self-enroll at first boot into `image`'s shared partition.
///
/// This is a network operation against a specific platform environment
/// with per-unit output (a new key and serial each time) -- unlike this
/// workspace's other verticals it cannot be part of a `batch` recipe
/// (batch steps are meant to be local and reproducible offline) and is
/// deliberately a standalone CLI command instead.
#[async_trait::async_trait]
pub trait FactoryServiceInterface: Send + Sync {
    /// `device_name` is the durable hardware serial -- the identity that
    /// matters here, since it survives a change of owner, unlike any
    /// resource name scoped to whoever currently owns the device.
    /// `access_url` is embedded into the image as-is, for the device's
    /// own later runtime use talking to the access tier; this call itself
    /// never uses it. `gateway_url`/`api_key` configure the platform
    /// request (see `FactoryProvisioningAdapter`'s doc comment for why
    /// they're passed here rather than fixed at wiring time).
    #[allow(clippy::too_many_arguments)]
    async fn provision(
        &self,
        device_name: &str,
        access_url: &str,
        gateway_url: &str,
        api_key: &str,
        image: &Path,
        ctx: &OperationContext,
    ) -> Result<()>;
}

/// Injectable handle to whatever `FactoryServiceInterface` implementation
/// was wired at startup (normally `remora-etcher-factory-application`'s
/// `FactoryControllerImpl`).
#[derive(Clone)]
pub struct FactoryService(busybody::Service<Box<dyn FactoryServiceInterface>>);

impl FactoryService {
    pub fn new<T: FactoryServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for FactoryService {
    type Target = busybody::Service<Box<dyn FactoryServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
