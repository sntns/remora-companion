use super::error::Result;
use crate::model::{Contexts, Migration};

/// Where the contexts and the current-context pointer live: one document,
/// read and written whole, so that a change touching several contexts is
/// one write. Synchronous, like this workspace's other local-file
/// adapters: it is a small file.
///
/// Names arrive already validated by the application layer.
pub trait ContextStoreAdapter: Send + Sync {
    /// Holds the store -- and the credential store beside it -- for one
    /// read-modify-write: until the guard drops, no other process changes
    /// either. Reading needs no lock: every write replaces a file whole.
    fn lock(&self) -> Result<StoreLock>;
    fn load(&self) -> Result<Contexts>;
    fn save(&self, contexts: &Contexts) -> Result<()>;
    /// Brings contexts stored in a previous format to the current one, or,
    /// with `dry_run`, only says what that would change. `None` when they
    /// already are. `load` does it too, silently: this is to say so.
    fn migrate(&self, dry_run: bool) -> Result<Option<Migration>>;
}

/// Held while a change is being made: see [`ContextStoreAdapter::lock`].
pub struct StoreLock(#[allow(dead_code)] Box<dyn Send>);

impl StoreLock {
    pub fn new(guard: impl Send + 'static) -> Self {
        Self(Box::new(guard))
    }
}

#[derive(Clone)]
pub struct ContextStoreAdapterService(busybody::Service<Box<dyn ContextStoreAdapter>>);

impl ContextStoreAdapterService {
    pub fn new<T: ContextStoreAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for ContextStoreAdapterService {
    type Target = busybody::Service<Box<dyn ContextStoreAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
