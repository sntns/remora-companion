use super::error::Result;
use crate::model::{Context, Credentials, Principal};

/// The platform's view of a set of credentials: whom they authenticate as.
/// What `rmra login` checks before storing anything, and `rmra whoami`.
#[async_trait::async_trait]
pub trait PlatformSessionAdapter: Send + Sync {
    async fn whoami(&self, context: &Context, credentials: &Credentials) -> Result<Principal>;

    /// Assumes `role` (a role URN) as the login, and says which account
    /// that acts in: `Some(name)`, or `None` when the role may not read its
    /// account -- assumed all the same. Fails with `RoleRefused` when the
    /// login may not assume it (no `iam::assume-role` permission, or the
    /// role's trust policy doesn't allow it).
    async fn acting_account(
        &self,
        context: &Context,
        credentials: &Credentials,
        role: &str,
    ) -> Result<Option<String>>;
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
