use super::error::Result;
use crate::model::Credentials;

/// Where each context's credentials live, keyed by context name. Its own
/// port, apart from `ContextStoreAdapter`, so the secrets can move to an
/// OS keyring later by swapping this one adapter.
pub trait CredentialStoreAdapter: Send + Sync {
    fn get(&self, context: &str) -> Result<Option<Credentials>>;
    fn put(&self, context: &str, credentials: &Credentials) -> Result<()>;
    /// Whether there was anything to remove.
    fn delete(&self, context: &str) -> Result<bool>;
}

#[derive(Clone)]
pub struct CredentialStoreAdapterService(busybody::Service<Box<dyn CredentialStoreAdapter>>);

impl CredentialStoreAdapterService {
    pub fn new<T: CredentialStoreAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for CredentialStoreAdapterService {
    type Target = busybody::Service<Box<dyn CredentialStoreAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
