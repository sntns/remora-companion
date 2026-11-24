use super::error::Result;
use crate::model::{
    AssumedRole, ContextOverride, ContextSummary, ContextUpdate, Credentials, Endpoint, Migration,
    Principal, ResolvedContext, Selection,
};

/// The context vertical's application-facing port: docker-context-style
/// named contexts -- logins, and roles derived from them -- the one
/// selected for an invocation, and logging in and out.
///
/// Every method that acts on "the" context takes an optional
/// [`ContextOverride`] (`--context`, `RMRA_CONTEXT`, or a context defined
/// whole by the environment); without one it is the stored current context,
/// or the only context when there is just one. Those that change a login
/// refuse a defined one ([`Error::Defined`]): it isn't stored.
///
/// [`Error::Defined`]: super::Error::Defined
#[async_trait::async_trait]
pub trait ContextServiceInterface: Send + Sync {
    async fn list(&self) -> Result<Vec<ContextSummary>>;
    async fn inspect(&self, name: &str) -> Result<ContextSummary>;
    /// Creates login context `name` at `endpoint`. `replace` overwrites an
    /// existing login of that name, keeping its credentials and the
    /// contexts derived from it.
    async fn create(
        &self,
        name: &str,
        description: Option<String>,
        endpoint: Endpoint,
        replace: bool,
    ) -> Result<()>;
    /// Creates context `name`, in which the login of `source` (or of the
    /// login `source` derives from) assumes `role`, a role URN -- verified
    /// with the platform before anything is written. `replace` overwrites
    /// an existing derived context of that name. Returns the login's
    /// principal and the role with the account it acts in.
    async fn derive(
        &self,
        source: &str,
        name: &str,
        description: Option<String>,
        role: &str,
        replace: bool,
    ) -> Result<(Principal, AssumedRole)>;
    /// Changes context `name`'s settings; a new role is verified with the
    /// platform first. Returns the role it now assumes, if it changed.
    async fn update(&self, name: &str, update: ContextUpdate) -> Result<Option<AssumedRole>>;
    /// Renames context `from` to `to`, with its login, the contexts derived
    /// from it, and its being current.
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
    /// Removes a context and its credentials, and stops it being current. A
    /// login with contexts derived from it is refused, unless `cascade`,
    /// which removes them too. Returns every context removed.
    async fn remove(&self, name: &str, cascade: bool) -> Result<Vec<String>>;
    /// Makes `name` the stored current context.
    async fn use_context(&self, name: &str) -> Result<()>;
    /// The selected context's name and how it was selected, or `None` when
    /// there's no context at all.
    async fn selected(&self, over: Option<&ContextOverride>)
        -> Result<Option<(String, Selection)>>;
    /// The selected context with its login's endpoint and credentials,
    /// ready for an adapter.
    async fn resolve(&self, over: Option<&ContextOverride>) -> Result<ResolvedContext>;
    /// Logs the selected context's login in -- its own, or the one it
    /// derives from, i.e. for every context derived from it too: verifies
    /// `credentials` with the platform, and only then stores them. Returns
    /// the login context's name and principal.
    async fn login(
        &self,
        over: Option<&ContextOverride>,
        credentials: Credentials,
    ) -> Result<(String, Principal)>;
    /// Forgets the selected context's login; returns the login context's
    /// name and whether it was logged in at all.
    async fn logout(&self, over: Option<&ContextOverride>) -> Result<(String, bool)>;
    /// Whom the selected context's login authenticates as, and, when it
    /// assumes a role, the account it lands in (`None` inside when the role
    /// may not read its account).
    async fn whoami(
        &self,
        over: Option<&ContextOverride>,
    ) -> Result<(ResolvedContext, Principal, Option<Option<String>>)>;
    /// Brings the stored contexts to the current format (see
    /// `ContextStoreAdapter::migrate`): `None` when they already are.
    async fn migrate(&self, dry_run: bool) -> Result<Option<Migration>>;
}

#[derive(Clone)]
pub struct ContextService(busybody::Service<Box<dyn ContextServiceInterface>>);

impl ContextService {
    pub fn new<T: ContextServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for ContextService {
    type Target = busybody::Service<Box<dyn ContextServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
