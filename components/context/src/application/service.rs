use super::error::Result;
use crate::model::{
    AssumedRole, Context, ContextOverride, ContextSummary, Credentials, Principal, ResolvedContext,
    RoleSummary, Selection,
};

/// The context vertical's application-facing port: docker-context-style
/// named endpoints, the one selected for an invocation, and logging in and
/// out of them.
///
/// Every method that acts on "the" context takes an optional
/// [`ContextOverride`] (`--context`, `RMRA_CONTEXT`); without one it is the
/// stored current context, or the only context when there is just one.
#[async_trait::async_trait]
pub trait ContextServiceInterface: Send + Sync {
    async fn list(&self) -> Result<Vec<ContextSummary>>;
    async fn inspect(&self, name: &str) -> Result<ContextSummary>;
    /// Creates `context`; `replace` overwrites an existing one of that name
    /// (keeping its credentials, since the endpoint may just have moved).
    async fn create(&self, context: Context, replace: bool) -> Result<()>;
    /// Removes a context and its credentials, and stops it being current.
    async fn remove(&self, name: &str) -> Result<()>;
    /// Makes `name` the stored current context.
    async fn use_context(&self, name: &str) -> Result<()>;
    /// The selected context's name and how it was selected, or `None` when
    /// there's no context at all.
    async fn selected(&self, over: Option<&ContextOverride>)
        -> Result<Option<(String, Selection)>>;
    /// The selected context with its credentials, ready for an adapter.
    async fn resolve(&self, over: Option<&ContextOverride>) -> Result<ResolvedContext>;
    /// Verifies `credentials` against the selected context's platform and,
    /// only if they work, stores them for it.
    async fn login(
        &self,
        over: Option<&ContextOverride>,
        credentials: Credentials,
    ) -> Result<(String, Principal)>;
    /// Forgets the selected context's credentials; returns the context's
    /// name and whether it was logged in at all.
    async fn logout(&self, over: Option<&ContextOverride>) -> Result<(String, bool)>;
    /// Whom the selected context's stored credentials authenticate as, and,
    /// when a role is assumed, the account acting as it lands in (`None`
    /// inside when the role may not read its account).
    async fn whoami(
        &self,
        over: Option<&ContextOverride>,
    ) -> Result<(ResolvedContext, Principal, Option<Option<String>>)>;

    /// The selected context's name and its roles, sorted by alias.
    async fn roles(&self, over: Option<&ContextOverride>) -> Result<(String, Vec<RoleSummary>)>;
    /// Remembers `urn` under `alias` in the selected context.
    async fn add_role(&self, over: Option<&ContextOverride>, alias: &str, urn: &str) -> Result<()>;
    /// Forgets a role alias (and stops assuming it if it was).
    async fn remove_role(&self, over: Option<&ContextOverride>, alias: &str) -> Result<()>;
    /// Assumes `role` (an alias, or a role URN) for every later call of the
    /// selected context, once the platform has confirmed it may be; returns
    /// it with the account it acts in.
    async fn assume_role(
        &self,
        over: Option<&ContextOverride>,
        role: &str,
    ) -> Result<(String, AssumedRole, Option<String>)>;
    /// Stops assuming a role; returns the context and the role dropped.
    async fn drop_role(&self, over: Option<&ContextOverride>) -> Result<(String, Option<String>)>;
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
