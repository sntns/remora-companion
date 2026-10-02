use super::error::Result;
use crate::model::Release;

/// Where releases are published.
#[async_trait::async_trait]
pub trait ReleaseFeedAdapter: Send + Sync {
    /// The latest published (non-draft, non-prerelease) release.
    async fn latest(&self) -> Result<Release>;
}

#[derive(Clone)]
pub struct ReleaseFeedAdapterService(busybody::Service<Box<dyn ReleaseFeedAdapter>>);

impl ReleaseFeedAdapterService {
    pub fn new<T: ReleaseFeedAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for ReleaseFeedAdapterService {
    type Target = busybody::Service<Box<dyn ReleaseFeedAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
