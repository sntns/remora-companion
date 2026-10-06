use super::error::Result;
use crate::model::Context;

/// Where contexts and the current-context pointer live. Synchronous, like
/// this workspace's other local-file adapters: it is a few small files.
///
/// Names arrive already validated by the application layer, so an adapter
/// may use them as path components.
pub trait ContextStoreAdapter: Send + Sync {
    /// Every stored context, sorted by name.
    fn list(&self) -> Result<Vec<Context>>;
    fn get(&self, name: &str) -> Result<Option<Context>>;
    /// Creates or replaces `context`, keyed by its name.
    fn put(&self, context: &Context) -> Result<()>;
    /// Removes a context. Removing one that doesn't exist is not an error.
    fn delete(&self, name: &str) -> Result<()>;
    fn current(&self) -> Result<Option<String>>;
    fn set_current(&self, name: Option<&str>) -> Result<()>;
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
