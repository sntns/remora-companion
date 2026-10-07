use std::path::{Path, PathBuf};

use remora_squashfs::application::BuildSummary;

use super::error::Result;

/// The identity vertical's application-facing port: what every transport
/// (CLI today, anything else later) calls into.
#[async_trait::async_trait]
pub trait IdentityServiceInterface: Send + Sync {
    /// Build `identity.squashfs` from `inputs` (arbitrary user-supplied
    /// files and/or directories, same semantics as the squashfs vertical's
    /// own `build`) plus `hostname`, `machine-id`, and an ed25519 SSH host
    /// keypair — matching `bootstrap-localdev.bb`'s `IDENTITY_DIR`
    /// convention (`hostname`, `machine-id`, `ssh_host_ed25519_key`[`.pub`]).
    ///
    /// `inputs` is not limited to those fields — a device's identity can
    /// carry whatever additional files the caller supplies. If `inputs`
    /// already provides a root-level file at one of those paths (or, for
    /// the SSH keypair, both halves of it), that file wins and nothing is
    /// generated for it. Only one half of the keypair is an error.
    async fn build(
        &self,
        inputs: &[PathBuf],
        hostname: Option<&str>,
        machine_id: Option<&str>,
        output: &Path,
    ) -> Result<BuildSummary>;

    /// Build `identity.squashfs` from `inputs`/`hostname`/`machine_id` (see
    /// `build`) and inject it into `Shared:/remora/identity` of `image` in
    /// one shot. The shared partition and its filesystem kind (vfat/ext4)
    /// are detected from the image itself.
    async fn create(
        &self,
        inputs: &[PathBuf],
        hostname: Option<&str>,
        machine_id: Option<&str>,
        image: &Path,
    ) -> Result<u64>;
}

/// Injectable handle to whatever `IdentityServiceInterface` implementation
/// was wired at startup (normally `remora-identity-application`'s
/// `IdentityControllerImpl`).
#[derive(Clone)]
pub struct IdentityService(busybody::Service<Box<dyn IdentityServiceInterface>>);

impl IdentityService {
    pub fn new<T: IdentityServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for IdentityService {
    type Target = busybody::Service<Box<dyn IdentityServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
