use super::error::Result;
use crate::model::{PrivateKey, VerifiedCsr};

/// A freshly generated device keypair, as far as provisioning needs it: the
/// CSR to send to the platform, and the private key to keep. No `Clone`:
/// nothing past the credential it ends up in should hold a second copy.
#[derive(Debug)]
pub struct DeviceKey {
    /// DER-encoded PKCS#10 CSR, signed by `private_key`.
    pub csr_der: Vec<u8>,
    pub private_key: PrivateKey,
}

/// DI seam for `remora-factory-application`: where a device's keypair and
/// CSR come from -- the same seam `remora-identity`'s `KeygenAdapter` puts
/// around its own keys, so the use case never names a crypto crate.
#[async_trait::async_trait]
pub trait DeviceKeyAdapter: Send + Sync {
    /// A new keypair, and a CSR for it whose subject CN is `common_name`
    /// when given (the platform ignores the subject; it only keeps the
    /// artifacts readable) and empty otherwise.
    async fn generate(&self, common_name: Option<&str>) -> Result<DeviceKey>;

    /// Checks a CSR made elsewhere (a device that keeps its own key) the
    /// way the platform will: a DER PKCS#10 request for a P-256 key,
    /// self-signed by that key. Anything else is `Error::InvalidCsr`, with
    /// what was wrong attached.
    async fn verify_csr(&self, csr_der: &[u8]) -> Result<VerifiedCsr>;
}

/// Injectable handle to whatever `DeviceKeyAdapter` was wired at startup.
#[derive(Clone)]
pub struct DeviceKeyAdapterService(busybody::Service<Box<dyn DeviceKeyAdapter>>);

impl DeviceKeyAdapterService {
    pub fn new<T: DeviceKeyAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for DeviceKeyAdapterService {
    type Target = busybody::Service<Box<dyn DeviceKeyAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
