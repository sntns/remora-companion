use super::error::Result;
use crate::model::LastCheck;

/// Where a binary remembers when it last looked for a newer release, so
/// that it looks at most so often.
#[async_trait::async_trait]
pub trait CheckStateAdapter: Send + Sync {
    /// What `app` found last time it looked, if it ever did.
    async fn load(&self, app: &str) -> Result<Option<LastCheck>>;
    async fn save(&self, app: &str, check: &LastCheck) -> Result<()>;
}

#[derive(Clone)]
pub struct CheckStateAdapterService(busybody::Service<Box<dyn CheckStateAdapter>>);

impl CheckStateAdapterService {
    pub fn new<T: CheckStateAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for CheckStateAdapterService {
    type Target = busybody::Service<Box<dyn CheckStateAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
