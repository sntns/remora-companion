use super::error::Result;
use crate::model::DeviceSerial;

/// What the platform's factory-device call hands back, before the
/// locally-generated private key is added at the application layer (see
/// `crate::model::FactoryCredential`).
#[derive(Debug, Clone)]
pub struct ProvisionedIdentity {
    /// The serial actually issued -- allocated by the platform for
    /// `DeviceSerial::FromPolicy`, echoed back for `Explicit`. Also the
    /// certificate's CN.
    pub serial_number: String,
    /// The device's URN, carried in the certificate's URI SAN.
    pub factory_device_name: String,
    pub certificate_der: Vec<u8>,
    pub certificate_authority_der: Vec<u8>,
    pub server_certificate_authority_der: Vec<u8>,
    /// `certificateReference.urn` on the wire -- the keyid the device puts
    /// on its own RFC 9421 signatures later, not derivable from the
    /// certificate itself.
    pub key_id: String,
    /// The access-tier URL this device should phone home to, chosen
    /// entirely by the platform (never by the caller -- a factory tool
    /// choosing which server a device obeys would defeat the point).
    /// Empty when a deployment hasn't configured an access-url yet; the
    /// application layer treats that as a hard failure, not a default.
    pub access_url: String,
}

/// DI seam for `remora-factory-application`: the actual network
/// call to `sntns-platform`'s factory-device provisioning endpoint.
///
/// Unlike this workspace's other adapters (which wrap blocking local file
/// I/O and stay synchronous by design -- see CLAUDE.md's DI convention),
/// this one is a real network call and is async-native rather than
/// sync-wrapped for uniformity.
///
/// `api_url`/`api_key` are passed per call rather than fixed at
/// construction time: they're CLI-level configuration (`factory provision
/// --api-url ... --api-key ...`), known only once the subcommand's own
/// arguments are parsed, well after the composition root has already
/// wired every service -- keeping the adapter itself stateless (beyond a
/// reused HTTP client) avoids coupling wiring order to that.
#[async_trait::async_trait]
pub trait FactoryProvisioningAdapter: Send + Sync {
    /// Request a factory device credential under `serial`, presenting
    /// `csr_der` -- a DER-encoded PKCS#10 CSR whose subject is ignored
    /// server-side; identity comes from `serial` alone.
    ///
    /// An explicit device name that was already manufactured is refused
    /// with `Error::AlreadyExists` unless its `force` is set, in which case
    /// the platform re-signs it and revokes the previous IDevID.
    async fn provision(
        &self,
        api_url: &str,
        api_key: &str,
        serial: &DeviceSerial,
        csr_der: &[u8],
    ) -> Result<ProvisionedIdentity>;
}

/// Injectable handle to whatever `FactoryProvisioningAdapter` was wired at
/// startup.
#[derive(Clone)]
pub struct FactoryProvisioningAdapterService(
    busybody::Service<Box<dyn FactoryProvisioningAdapter>>,
);

impl FactoryProvisioningAdapterService {
    pub fn new<T: FactoryProvisioningAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for FactoryProvisioningAdapterService {
    type Target = busybody::Service<Box<dyn FactoryProvisioningAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
