use super::error::Result;
use crate::model::{
    AssumedRole, Context, ContextOverride, ContextSummary, Credentials, Principal, ResolvedContext,
    RoleOverride, RoleSummary, Selection,
};

/// The context vertical's application-facing port: docker-context-style
/// named endpoints, the one selected for an invocation, and logging in and
/// out of them.
///
/// Every method that acts on "the" context takes an optional
/// [`ContextOverride`] (`--context`, `RMRA_CONTEXT`, or a context defined
/// whole by the environment); without one it is the stored current context,
/// or the only context when there is just one. Those that change a context
/// or its login refuse a defined one ([`Error::Defined`]): it isn't stored.
///
/// [`Error::Defined`]: super::Error::Defined
#[async_trait::async_trait]
pub trait ContextServiceInterface: Send + Sync {
    async fn list(&self) -> Result<Vec<ContextSummary>>;
    async fn inspect(&self, name: &str) -> Result<ContextSummary>;
    /// Creates `context`, acting as `role` (`Keep`: the role of the context
    /// it replaces, if any) -- checked against its roles, but only verified
    /// with the platform at login, there being no credentials yet. `replace`
    /// overwrites an existing context of that name, keeping what isn't the
    /// endpoint: its login (its own credentials, or the one it shares) and
    /// its roles, unless `context` names roles of its own.
    async fn create(&self, context: Context, role: RoleOverride, replace: bool) -> Result<()>;
    /// Creates context `name` from context `source`: its endpoint, role
    /// aliases and login, acting as `role` (`Keep` takes the source's role,
    /// `Drop` none). The role is verified with the platform under the
    /// source's login before anything is created; `replace` overwrites an
    /// existing `name`. Returns the login's principal and the role assumed
    /// with the account it acts in.
    async fn derive(
        &self,
        source: &str,
        name: &str,
        description: Option<String>,
        role: RoleOverride,
        replace: bool,
    ) -> Result<(Principal, Option<(AssumedRole, Option<String>)>)>;
    /// Renames context `from` to `to`, with its login, the contexts declined
    /// from it, and its being current.
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
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
    /// Logs the selected context in: verifies `credentials` against its
    /// platform, then the role the context will act as -- `role` chosen now
    /// (`Assume`, `Drop`), or the one it already has (`Keep`) -- and only
    /// if both work, stores the credentials and the role in the context.
    /// Returns the context, the login's principal, and the role assumed
    /// with the account it acts in.
    async fn login(
        &self,
        over: Option<&ContextOverride>,
        credentials: Credentials,
        role: RoleOverride,
    ) -> Result<(String, Principal, Option<(AssumedRole, Option<String>)>)>;
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

    /// The selected context's name, its roles sorted by alias, and the role
    /// it acts as -- which may have no alias (a URN given as is).
    async fn roles(
        &self,
        over: Option<&ContextOverride>,
    ) -> Result<(String, Vec<RoleSummary>, Option<AssumedRole>)>;
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
