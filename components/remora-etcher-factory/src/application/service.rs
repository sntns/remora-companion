use std::path::Path;

use remora_etcher_progress::OperationContext;

use super::error::Result;

/// The factory vertical's application-facing port: manufacture a device --
/// generate a local keypair + CSR, request a factory-device credential
/// from `sntns-platform`, and write `remora-factory.yaml` to `output`.
///
/// Deliberately produces a standalone file rather than touching an image
/// directly, mirroring `identity build`/`config build`: the caller is
/// responsible for bundling that file into an image's identity (via
/// `identity create`'s `inputs`), exactly like any other identity input.
/// This also sidesteps a real conflict a direct-injection design would
/// have hit -- `identity create` already owns "build one identity.squashfs
/// from a merged set of inputs and inject it," and there is no
/// "add one more file to an *existing* identity.squashfs" operation to
/// call into instead without either duplicating that logic or clobbering
/// whatever an earlier `identity create` step already wrote.
///
/// This is a network operation against a specific platform environment
/// with per-unit output (a new key and serial each time) -- unlike this
/// workspace's other verticals it cannot be part of a `batch` recipe in
/// the sense of being locally replayable, though it can still be one
/// *step* of a recipe (see `remora-etcher-batch`).
#[async_trait::async_trait]
pub trait FactoryServiceInterface: Send + Sync {
    /// `device_name` is the durable hardware serial -- the identity that
    /// matters here, since it survives a change of owner, unlike any
    /// resource name scoped to whoever currently owns the device.
    ///
    /// `access_url_override` is an escape hatch, not the normal path: the
    /// platform's response always carries the access-tier URL the device
    /// should use, chosen by the platform (a factory tool cannot be
    /// trusted to choose which server a device obeys). It's only
    /// consulted when that response comes back empty, which happens on a
    /// deployment that hasn't configured one yet -- silently writing an
    /// empty URL into the credential would produce a device that can
    /// never phone home, discovered at the worst possible moment, so this
    /// call fails instead unless an override is given for that case.
    async fn provision(
        &self,
        device_name: &str,
        api_url: &str,
        api_key: &str,
        access_url_override: Option<&str>,
        output: &Path,
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
