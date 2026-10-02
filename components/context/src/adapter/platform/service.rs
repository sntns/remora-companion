use super::error::Result;
use crate::model::{Context, Credentials, Principal};

/// The platform's view of a set of credentials: whom they authenticate as.
/// What `rmra login` checks before storing anything, and `rmra whoami`.
#[async_trait::async_trait]
pub trait PlatformSessionAdapter: Send + Sync {
    async fn whoami(&self, context: &Context, credentials: &Credentials) -> Result<Principal>;
}

#[derive(Clone)]
pub struct PlatformSessionAdapterService(busybody::Service<Box<dyn PlatformSessionAdapter>>);

impl PlatformSessionAdapterService {
    pub fn new<T: PlatformSessionAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for PlatformSessionAdapterService {
    type Target = busybody::Service<Box<dyn PlatformSessionAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
