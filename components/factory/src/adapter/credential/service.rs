use std::path::Path;

use super::error::Result;
use crate::model::FactoryCredential;

/// DI seam for `remora-factory-application`: turning a credential into the
/// `remora-factory.yaml` a device reads at first boot, on disk. One port for
/// both rendering and writing because the file's format and how securely it
/// lands are one concern: it carries the device's private key.
#[async_trait::async_trait]
pub trait CredentialWriterAdapter: Send + Sync {
    /// Renders `credential`, with `access_url` as the URL the device phones
    /// home to, and writes it to `output`, replacing whatever is there.
    /// Readable by its owner only, and never visible half-written.
    async fn write(
        &self,
        credential: &FactoryCredential,
        access_url: &str,
        output: &Path,
    ) -> Result<()>;
}

/// Injectable handle to whatever `CredentialWriterAdapter` was wired at
/// startup.
#[derive(Clone)]
pub struct CredentialWriterAdapterService(busybody::Service<Box<dyn CredentialWriterAdapter>>);

impl CredentialWriterAdapterService {
    pub fn new<T: CredentialWriterAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for CredentialWriterAdapterService {
    type Target = busybody::Service<Box<dyn CredentialWriterAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
