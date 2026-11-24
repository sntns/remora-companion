use error_stack::{Report, ResultExt};
use remora_context::{
    adapter::{
        credentials::CredentialStoreAdapterService, platform::PlatformSessionAdapterService,
        store::ContextStoreAdapterService,
    },
    application::{ContextServiceInterface, Error, Result},
    model::{
        is_role_urn, short_role_name, AssumedRole, Context, ContextKind, ContextOverride,
        ContextSummary, ContextUpdate, Contexts, Credentials, DefinedContext, Derivation, Endpoint,
        Migration, Principal, ResolvedContext, Selection,
    },
};

/// The context vertical's use case. Holds the three ports: where contexts
/// live, where their credentials live, and the platform that vouches for
/// those credentials.
pub struct ContextControllerImpl {
    store: ContextStoreAdapterService,
    credentials: CredentialStoreAdapterService,
    platform: PlatformSessionAdapterService,
}

/// The context an invocation selected: a stored one, or one the
/// environment defines.
enum Selected {
    Stored(Context),
    Defined(DefinedContext),
}

impl ContextControllerImpl {
    pub fn new(
        store: ContextStoreAdapterService,
        credentials: CredentialStoreAdapterService,
        platform: PlatformSessionAdapterService,
    ) -> Self {
        Self {
            store,
            credentials,
            platform,
        }
    }

    /// docker's own rule: a letter or digit, then letters, digits, `_.+-`.
    fn validate(name: &str) -> Result<()> {
        let mut chars = name.chars();
        let valid = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
            && chars.all(|c| c.is_ascii_alphanumeric() || "_.+-".contains(c));
        if valid {
            Ok(())
        } else {
            Err(Report::new(Error::InvalidName(name.to_owned())))
        }
    }

    fn validate_role(urn: &str) -> Result<()> {
        if is_role_urn(urn) {
            Ok(())
        } else {
            Err(Report::new(Error::InvalidRoleUrn(urn.to_owned())))
        }
    }

    fn load(&self) -> Result<Contexts> {
        self.store.load().change_context(Error::Store)
    }

    fn save(&self, contexts: &Contexts) -> Result<()> {
        self.store.save(contexts).change_context(Error::Store)
    }

    fn existing(contexts: &Contexts, name: &str) -> Result<Context> {
        Self::validate(name)?;
        contexts
            .get(name)
            .cloned()
            .ok_or_else(|| Report::new(Error::NotFound(name.to_owned())))
    }

    /// The endpoint a stored context reaches: its own, or its login's.
    fn endpoint_of(contexts: &Contexts, context: &Context) -> Result<Endpoint> {
        let login = context.login_name();
        contexts
            .get(login)
            .and_then(Context::endpoint)
            .cloned()
            .ok_or_else(|| Report::new(Error::NotFound(login.to_owned())))
    }

    fn logged_in(&self, login: &str) -> Result<Credentials> {
        self.credentials
            .get(login)
            .change_context(Error::Credentials)?
            .ok_or_else(|| Report::new(Error::NotLoggedIn(login.to_owned())))
    }

    /// The account `role` acts in, once the platform confirms `credentials`
    /// may assume it.
    async fn assume(
        &self,
        endpoint: &Endpoint,
        credentials: &Credentials,
        role: &str,
    ) -> Result<Option<String>> {
        self.platform
            .acting_account(endpoint, credentials, role)
            .await
            .change_context_lazy(|| Error::Assume(short_role_name(role).to_owned()))
    }

    /// The selected context itself, which must exist.
    fn select(&self, over: Option<&ContextOverride>) -> Result<(Selected, Selection)> {
        if let Some(over) = over {
            if let Some(defined) = &over.defined {
                return Ok((Selected::Defined(defined.clone()), over.source));
            }
            let contexts = self.load()?;
            return Ok((
                Selected::Stored(Self::existing(&contexts, &over.name)?),
                over.source,
            ));
        }
        let contexts = self.load()?;
        if let Some(current) = &contexts.current {
            // A current pointer left dangling by hand-editing reads as the
            // missing context it names, not as "no context".
            return Ok((
                Selected::Stored(Self::existing(&contexts, current)?),
                Selection::Current,
            ));
        }
        let mut all = contexts.contexts.into_values();
        match (all.next(), all.next()) {
            (None, _) => Err(Report::new(Error::NoContext)),
            (Some(only), None) => Ok((Selected::Stored(only), Selection::Only)),
            _ => Err(Report::new(Error::Ambiguous)),
        }
    }

    /// The selected context, to change its login: a stored one only -- a
    /// defined context has nowhere to keep a change.
    fn select_stored(&self, over: Option<&ContextOverride>) -> Result<Context> {
        match self.select(over)? {
            (Selected::Stored(context), _) => Ok(context),
            (Selected::Defined(defined), _) => Err(Report::new(Error::Defined(defined.name))),
        }
    }

    /// Remembers the account a derived context's role acts in, for display:
    /// best-effort, a context changed meanwhile is left as it is.
    fn remember_accounts(&self, accounts: &[(String, String, Option<String>)]) {
        let Ok(_lock) = self.store.lock() else {
            return;
        };
        let Ok(mut contexts) = self.store.load() else {
            return;
        };
        let mut changed = false;
        for (name, role, account) in accounts {
            if let Some(ContextKind::Derived(derivation)) =
                contexts.contexts.get_mut(name).map(|c| &mut c.kind)
            {
                if &derivation.role == role && &derivation.account != account {
                    derivation.account = account.clone();
                    changed = true;
                }
            }
        }
        if changed {
            let _ = self.store.save(&contexts);
        }
    }
}

#[async_trait::async_trait]
impl ContextServiceInterface for ContextControllerImpl {
    async fn list(&self) -> Result<Vec<ContextSummary>> {
        let contexts = self.load()?;
        let only = contexts.contexts.len() == 1 && contexts.current.is_none();
        contexts
            .contexts
            .values()
            .map(|context| {
                let credentials = self
                    .credentials
                    .get(context.login_name())
                    .change_context(Error::Credentials)?;
                Ok(ContextSummary {
                    current: only || contexts.current.as_deref() == Some(context.name.as_str()),
                    credentials: credentials.map(|c| c.kind()),
                    context: context.clone(),
                })
            })
            .collect()
    }

    async fn inspect(&self, name: &str) -> Result<ContextSummary> {
        let contexts = self.load()?;
        let context = Self::existing(&contexts, name)?;
        let credentials = self
            .credentials
            .get(context.login_name())
            .change_context(Error::Credentials)?;
        Ok(ContextSummary {
            current: contexts.current.as_deref() == Some(name),
            credentials: credentials.map(|c| c.kind()),
            context,
        })
    }

    async fn create(
        &self,
        name: &str,
        description: Option<String>,
        endpoint: Endpoint,
        replace: bool,
    ) -> Result<()> {
        Self::validate(name)?;
        let _lock = self.store.lock().change_context(Error::Store)?;
        let mut contexts = self.load()?;
        if contexts.get(name).is_some() && !replace {
            return Err(Report::new(Error::AlreadyExists(name.to_owned())));
        }
        // Replacing a login moves its endpoint; its credentials, keyed by
        // its name, and the contexts derived from it stay.
        contexts.insert(Context {
            name: name.to_owned(),
            description,
            kind: ContextKind::Login { endpoint },
        });
        self.save(&contexts)
    }

    async fn derive(
        &self,
        source: &str,
        name: &str,
        description: Option<String>,
        role: &str,
        replace: bool,
    ) -> Result<(Principal, AssumedRole)> {
        Self::validate(name)?;
        Self::validate_role(role)?;
        let contexts = self.load()?;
        // From the login itself: a context derived from a derived one
        // assumes its own role with the same login, never a role's role.
        let login = Self::existing(&contexts, source)?.login_name().to_owned();
        let endpoint = Self::endpoint_of(&contexts, &Self::existing(&contexts, &login)?)?;
        if name == login {
            return Err(Report::new(Error::AlreadyExists(name.to_owned())));
        }
        let credentials = self.logged_in(&login)?;
        // Verified before anything is written: the login still works, and
        // may assume the role.
        let principal = self
            .platform
            .whoami(&endpoint, &credentials)
            .await
            .change_context(Error::Verify)?;
        let account = self.assume(&endpoint, &credentials, role).await?;

        let _lock = self.store.lock().change_context(Error::Store)?;
        let mut contexts = self.load()?;
        let replaced = match contexts.get(name) {
            None => None,
            Some(_) if !replace => {
                return Err(Report::new(Error::AlreadyExists(name.to_owned())));
            }
            Some(existing) => {
                let users = contexts.derived_from(name);
                if !users.is_empty() {
                    return Err(Report::new(Error::InUse(name.to_owned(), users.join(", "))));
                }
                existing.endpoint().map(|_| name.to_owned())
            }
        };
        if contexts.get(&login).and_then(Context::endpoint).is_none() {
            return Err(Report::new(Error::NotFound(login)));
        }
        contexts.insert(Context {
            name: name.to_owned(),
            description,
            kind: ContextKind::Derived(Derivation {
                from: login,
                role: role.to_owned(),
                account: account.clone(),
            }),
        });
        self.save(&contexts)?;
        // A login it replaced has nothing left to log in.
        if let Some(replaced) = replaced {
            self.credentials
                .delete(&replaced)
                .change_context(Error::Credentials)?;
        }
        Ok((
            principal,
            AssumedRole {
                urn: role.to_owned(),
                account,
            },
        ))
    }

    async fn update(&self, name: &str, update: ContextUpdate) -> Result<Option<AssumedRole>> {
        let contexts = self.load()?;
        let context = Self::existing(&contexts, name)?;
        if let (true, Some(derivation)) = (update.touches_endpoint(), context.derivation()) {
            return Err(Report::new(Error::NoEndpoint(
                name.to_owned(),
                derivation.from.clone(),
            )));
        }
        let role = match &update.role {
            None => None,
            Some(_) if context.derivation().is_none() => {
                return Err(Report::new(Error::NotDerived(name.to_owned())));
            }
            Some(role) => {
                Self::validate_role(role)?;
                let endpoint = Self::endpoint_of(&contexts, &context)?;
                let credentials = self.logged_in(context.login_name())?;
                let account = self.assume(&endpoint, &credentials, role).await?;
                Some(AssumedRole {
                    urn: role.clone(),
                    account,
                })
            }
        };

        let _lock = self.store.lock().change_context(Error::Store)?;
        let mut contexts = self.load()?;
        let mut context = Self::existing(&contexts, name)?;
        if let Some(description) = update.description {
            context.description = Some(description).filter(|d| !d.is_empty());
        }
        match &mut context.kind {
            ContextKind::Login { endpoint } => {
                if let Some(address) = update.address {
                    endpoint.address = address;
                }
                if let Some(plaintext) = update.plaintext {
                    endpoint.tls.disabled = plaintext;
                }
                if let Some(authorities) = update.authorities {
                    endpoint.tls.authorities = authorities;
                }
                if let Some(server_name) = update.server_name {
                    endpoint.tls.server_name = Some(server_name).filter(|s| !s.is_empty());
                }
            }
            ContextKind::Derived(derivation) => {
                if let Some(role) = &role {
                    derivation.role = role.urn.clone();
                    derivation.account = role.account.clone();
                }
            }
        }
        contexts.insert(context);
        self.save(&contexts)?;
        Ok(role)
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        Self::validate(to)?;
        let _lock = self.store.lock().change_context(Error::Store)?;
        let mut contexts = self.load()?;
        let mut context = Self::existing(&contexts, from)?;
        if contexts.get(to).is_some() {
            return Err(Report::new(Error::AlreadyExists(to.to_owned())));
        }
        let is_login = context.endpoint().is_some();
        // Its login moves first, the old one removed last: a failure
        // halfway leaves credentials too many, never a login lost.
        let credentials = if is_login {
            self.credentials
                .get(from)
                .change_context(Error::Credentials)?
        } else {
            None
        };
        if let Some(credentials) = &credentials {
            self.credentials
                .put(to, credentials)
                .change_context(Error::Credentials)?;
        }
        contexts.contexts.remove(from);
        context.name = to.to_owned();
        contexts.insert(context);
        if is_login {
            for other in contexts.contexts.values_mut() {
                if let ContextKind::Derived(derivation) = &mut other.kind {
                    if derivation.from == from {
                        derivation.from = to.to_owned();
                    }
                }
            }
        }
        if contexts.current.as_deref() == Some(from) {
            contexts.current = Some(to.to_owned());
        }
        self.save(&contexts)?;
        if credentials.is_some() {
            self.credentials
                .delete(from)
                .change_context(Error::Credentials)?;
        }
        Ok(())
    }

    async fn remove(&self, name: &str, cascade: bool) -> Result<Vec<String>> {
        let _lock = self.store.lock().change_context(Error::Store)?;
        let mut contexts = self.load()?;
        let context = Self::existing(&contexts, name)?;
        let mut removed = contexts.derived_from(name);
        if !removed.is_empty() && !cascade {
            return Err(Report::new(Error::InUse(
                name.to_owned(),
                removed.join(", "),
            )));
        }
        removed.push(name.to_owned());
        for gone in &removed {
            contexts.contexts.remove(gone);
        }
        if contexts
            .current
            .as_ref()
            .is_some_and(|current| removed.contains(current))
        {
            contexts.current = None;
        }
        self.save(&contexts)?;
        if context.endpoint().is_some() {
            self.credentials
                .delete(name)
                .change_context(Error::Credentials)?;
        }
        Ok(removed)
    }

    async fn use_context(&self, name: &str) -> Result<()> {
        let _lock = self.store.lock().change_context(Error::Store)?;
        let mut contexts = self.load()?;
        Self::existing(&contexts, name)?;
        contexts.current = Some(name.to_owned());
        self.save(&contexts)
    }

    async fn selected(
        &self,
        over: Option<&ContextOverride>,
    ) -> Result<Option<(String, Selection)>> {
        match self.select(over) {
            Ok((Selected::Stored(context), selection)) => Ok(Some((context.name, selection))),
            Ok((Selected::Defined(defined), selection)) => Ok(Some((defined.name, selection))),
            Err(report) if matches!(report.current_context(), Error::NoContext) => Ok(None),
            Err(report) => Err(report),
        }
    }

    async fn resolve(&self, over: Option<&ContextOverride>) -> Result<ResolvedContext> {
        match self.select(over)? {
            // Its role is verified by the first call that assumes it, as
            // nothing was logged in before.
            (Selected::Defined(defined), selection) => Ok(ResolvedContext {
                login: defined.name.clone(),
                name: defined.name,
                endpoint: defined.endpoint,
                credentials: defined.credentials,
                selection,
                role: defined.role.map(|urn| AssumedRole { urn, account: None }),
            }),
            (Selected::Stored(context), selection) => {
                let contexts = self.load()?;
                let endpoint = Self::endpoint_of(&contexts, &context)?;
                let login = context.login_name().to_owned();
                let credentials = self.logged_in(&login)?;
                let role = context.derivation().map(|derivation| AssumedRole {
                    urn: derivation.role.clone(),
                    account: derivation.account.clone(),
                });
                Ok(ResolvedContext {
                    name: context.name,
                    login,
                    endpoint,
                    credentials,
                    selection,
                    role,
                })
            }
        }
    }

    async fn login(
        &self,
        over: Option<&ContextOverride>,
        credentials: Credentials,
    ) -> Result<(String, Principal)> {
        let context = self.select_stored(over)?;
        let contexts = self.load()?;
        let endpoint = Self::endpoint_of(&contexts, &context)?;
        let login = context.login_name().to_owned();
        // Verified first: storing credentials the platform refuses would
        // only move the failure to the next command, further from its cause.
        let principal = self
            .platform
            .whoami(&endpoint, &credentials)
            .await
            .change_context(Error::Verify)?;
        {
            let _lock = self.store.lock().change_context(Error::Store)?;
            self.credentials
                .put(&login, &credentials)
                .change_context(Error::Credentials)?;
        }
        // The accounts its derived contexts act in, where not known yet
        // (a migration doesn't call the platform): for display only.
        let mut accounts = Vec::new();
        for name in contexts.derived_from(&login) {
            let Some(derivation) = contexts.get(&name).and_then(Context::derivation) else {
                continue;
            };
            if derivation.account.is_none() {
                if let Ok(Some(account)) = self
                    .platform
                    .acting_account(&endpoint, &credentials, &derivation.role)
                    .await
                {
                    accounts.push((name, derivation.role.clone(), Some(account)));
                }
            }
        }
        if !accounts.is_empty() {
            self.remember_accounts(&accounts);
        }
        Ok((login, principal))
    }

    async fn logout(&self, over: Option<&ContextOverride>) -> Result<(String, bool)> {
        let context = self.select_stored(over)?;
        let login = context.login_name().to_owned();
        let _lock = self.store.lock().change_context(Error::Store)?;
        let removed = self
            .credentials
            .delete(&login)
            .change_context(Error::Credentials)?;
        Ok((login, removed))
    }

    async fn whoami(
        &self,
        over: Option<&ContextOverride>,
    ) -> Result<(ResolvedContext, Principal, Option<Option<String>>)> {
        let resolved = self.resolve(over).await?;
        let principal = self
            .platform
            .whoami(&resolved.endpoint, &resolved.credentials)
            .await
            .change_context(Error::Verify)?;
        let acting = match &resolved.role {
            None => None,
            Some(role) => {
                let account = self
                    .assume(&resolved.endpoint, &resolved.credentials, &role.urn)
                    .await?;
                if resolved.selection != Selection::Defined && account != role.account {
                    self.remember_accounts(&[(
                        resolved.name.clone(),
                        role.urn.clone(),
                        account.clone(),
                    )]);
                }
                Some(account)
            }
        };
        Ok((resolved, principal, acting))
    }

    async fn migrate(&self, dry_run: bool) -> Result<Option<Migration>> {
        self.store.migrate(dry_run).change_context(Error::Store)
    }
}

#[cfg(test)]
mod tests {
    use remora_context::{
        adapter::platform::{self, PlatformSessionAdapter},
        model::Secret,
    };
    use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};

    use super::*;

    /// Stands in for the platform: accepts exactly one token, and lets it
    /// assume one role. A hand-written stub of the one network port, the
    /// real file adapters for the rest.
    struct Platform;

    #[async_trait::async_trait]
    impl PlatformSessionAdapter for Platform {
        async fn whoami(
            &self,
            _: &Endpoint,
            credentials: &Credentials,
        ) -> platform::Result<Principal> {
            match &credentials.secret {
                Secret::AccessKey { token } if token == "good" => Ok(Principal {
                    user_urn: "urn:user:ada".into(),
                    user_name: "ada".into(),
                    account_name: Some("acme".into()),
                }),
                _ => Err(Report::new(platform::Error::Unauthenticated)),
            }
        }

        async fn acting_account(
            &self,
            _: &Endpoint,
            _: &Credentials,
            role: &str,
        ) -> platform::Result<Option<String>> {
            match role {
                OPS => Ok(Some("other".into())),
                SILENT => Ok(None),
                _ => Err(Report::new(platform::Error::RoleRefused)),
            }
        }
    }

    const OPS: &str = "urn:sntns:iam:eu2:other:role:ops";
    const SILENT: &str = "urn:sntns:iam:eu2:other:role:silent";
    const FORBIDDEN: &str = "urn:sntns:iam:eu2:other:role:forbidden";

    fn controller(root: &std::path::Path) -> ContextControllerImpl {
        ContextControllerImpl::new(
            ContextStoreAdapterService::new(FileContextStoreImpl::new(root)),
            CredentialStoreAdapterService::new(FileCredentialStoreImpl::new(root)),
            PlatformSessionAdapterService::new(Platform),
        )
    }

    fn endpoint(name: &str) -> Endpoint {
        Endpoint::new(format!("{name}.example"))
    }

    async fn create(contexts: &ContextControllerImpl, name: &str) {
        contexts
            .create(name, None, endpoint(name), false)
            .await
            .unwrap();
    }

    fn token(token: &str) -> Credentials {
        Credentials {
            secret: Secret::AccessKey {
                token: token.into(),
            },
        }
    }

    fn flag(name: &str) -> ContextOverride {
        ContextOverride::named(name, Selection::Flag)
    }

    /// The environment's context: an endpoint and a token, maybe a role.
    fn defined(token_: &str, role: Option<&str>) -> ContextOverride {
        ContextOverride::defined(DefinedContext {
            name: "env".into(),
            endpoint: endpoint("env"),
            credentials: token(token_),
            role: role.map(Into::into),
        })
    }

    fn kind(report: &Report<Error>) -> &Error {
        report.current_context()
    }

    #[tokio::test]
    async fn rejects_names_that_are_not_docker_names() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        for bad in ["", "-x", "../etc", "a/b", "é"] {
            let report = contexts
                .create(bad, None, endpoint("x"), false)
                .await
                .unwrap_err();
            assert!(matches!(kind(&report), Error::InvalidName(_)), "{bad:?}");
        }
        create(&contexts, "eu2-prod.v1").await;
    }

    #[tokio::test]
    async fn selection_follows_flag_then_current_then_only() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        assert!(contexts.selected(None).await.unwrap().is_none());

        create(&contexts, "eu2").await;
        assert_eq!(
            contexts.selected(None).await.unwrap(),
            Some(("eu2".into(), Selection::Only))
        );

        create(&contexts, "dev").await;
        let report = contexts.selected(None).await.unwrap_err();
        assert!(matches!(kind(&report), Error::Ambiguous));

        contexts.use_context("dev").await.unwrap();
        assert_eq!(
            contexts.selected(None).await.unwrap(),
            Some(("dev".into(), Selection::Current))
        );
        assert_eq!(
            contexts.selected(Some(&flag("eu2"))).await.unwrap(),
            Some(("eu2".into(), Selection::Flag))
        );
        let report = contexts.selected(Some(&flag("nope"))).await.unwrap_err();
        assert!(matches!(kind(&report), Error::NotFound(_)));
    }

    #[tokio::test]
    async fn login_stores_only_verified_credentials() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;

        let report = contexts.login(None, token("bad")).await.unwrap_err();
        assert!(matches!(kind(&report), Error::Verify));
        let report = contexts.resolve(None).await.unwrap_err();
        assert!(matches!(kind(&report), Error::NotLoggedIn(_)));

        let (name, principal) = contexts.login(None, token("good")).await.unwrap();
        assert_eq!(
            (name.as_str(), principal.user_name.as_str()),
            ("eu2", "ada")
        );
        let resolved = contexts.resolve(None).await.unwrap();
        assert!(resolved.credentials == token("good"));
        assert_eq!(resolved.role, None);
        assert_eq!(resolved.endpoint, endpoint("eu2"));
        let (_, principal, acting) = contexts.whoami(None).await.unwrap();
        assert_eq!(
            (principal.account_name.as_deref(), acting),
            (Some("acme"), None)
        );

        assert_eq!(contexts.logout(None).await.unwrap(), ("eu2".into(), true));
        assert_eq!(contexts.logout(None).await.unwrap(), ("eu2".into(), false));
    }

    #[tokio::test]
    async fn remove_forgets_credentials_and_current() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;
        contexts.use_context("eu2").await.unwrap();
        contexts.login(None, token("good")).await.unwrap();

        assert_eq!(contexts.remove("eu2", false).await.unwrap(), ["eu2"]);
        assert!(contexts.list().await.unwrap().is_empty());
        assert!(contexts.selected(None).await.unwrap().is_none());

        // Re-creating the name must not resurrect the old login.
        create(&contexts, "eu2").await;
        let report = contexts.resolve(None).await.unwrap_err();
        assert!(matches!(kind(&report), Error::NotLoggedIn(_)));
    }

    #[tokio::test]
    async fn replacing_a_login_keeps_its_credentials() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;
        contexts.login(None, token("good")).await.unwrap();

        let report = contexts
            .create("eu2", None, endpoint("eu2"), false)
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::AlreadyExists(_)));

        contexts
            .create("eu2", None, Endpoint::new("elsewhere"), true)
            .await
            .unwrap();
        let summary = contexts.inspect("eu2").await.unwrap();
        assert_eq!(
            summary.context.endpoint(),
            Some(&Endpoint::new("elsewhere"))
        );
        assert!(summary.credentials.is_some());
    }

    #[tokio::test]
    async fn a_context_derives_from_a_login_with_a_role() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;

        // The login must be logged in: it's what the role is assumed with.
        let report = contexts
            .derive("eu2", "acme", None, OPS, false)
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::NotLoggedIn(_)));
        contexts.login(None, token("good")).await.unwrap();

        // A refused role, or no URN, creates nothing.
        let report = contexts
            .derive("eu2", "acme", None, FORBIDDEN, false)
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::Assume(_)));
        let report = contexts
            .derive("eu2", "acme", None, "ops", false)
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::InvalidRoleUrn(_)));
        assert!(contexts.inspect("acme").await.is_err());

        let (principal, role) = contexts
            .derive("eu2", "acme", Some("Acme's hubs".into()), OPS, false)
            .await
            .unwrap();
        assert_eq!(principal.user_name, "ada");
        assert_eq!(role.account.as_deref(), Some("other"));

        // The login's endpoint and credentials, its own role; the login
        // itself still acts as itself.
        let acme = flag("acme");
        let resolved = contexts.resolve(Some(&acme)).await.unwrap();
        assert_eq!(
            (resolved.name.as_str(), resolved.login.as_str()),
            ("acme", "eu2")
        );
        assert_eq!(resolved.endpoint, endpoint("eu2"));
        assert_eq!(resolved.role.unwrap().urn, OPS);
        assert_eq!(
            contexts.resolve(Some(&flag("eu2"))).await.unwrap().role,
            None
        );
        assert!(contexts
            .inspect("acme")
            .await
            .unwrap()
            .credentials
            .is_some());

        // Nothing copied: the login moving moves it too.
        contexts
            .create("eu2", None, Endpoint::new("moved"), true)
            .await
            .unwrap();
        assert_eq!(
            contexts.resolve(Some(&acme)).await.unwrap().endpoint,
            Endpoint::new("moved")
        );

        // Derived from the derived one: from the same login, never a chain.
        contexts
            .derive("acme", "acme-quiet", None, SILENT, false)
            .await
            .unwrap();
        let quiet = contexts.inspect("acme-quiet").await.unwrap().context;
        assert_eq!(quiet.login_name(), "eu2");

        // One login: out of any is out of all, in again too.
        assert_eq!(
            contexts.logout(Some(&acme)).await.unwrap(),
            ("eu2".into(), true)
        );
        let report = contexts.resolve(Some(&flag("eu2"))).await.unwrap_err();
        assert!(matches!(kind(&report), Error::NotLoggedIn(login) if login == "eu2"));
        assert_eq!(
            contexts.login(Some(&acme), token("good")).await.unwrap().0,
            "eu2"
        );
        assert!(contexts.resolve(Some(&flag("eu2"))).await.is_ok());
    }

    #[tokio::test]
    async fn a_login_never_becomes_derived_by_accident() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;
        contexts.login(None, token("good")).await.unwrap();
        contexts
            .derive("eu2", "acme", None, OPS, false)
            .await
            .unwrap();

        // Not over itself, not over a login in use, not without --force.
        let report = contexts
            .derive("eu2", "eu2", None, OPS, true)
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::AlreadyExists(_)));
        create(&contexts, "dev").await;
        let report = contexts
            .derive("eu2", "dev", None, OPS, false)
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::AlreadyExists(_)));

        // Replacing a login with a derived context: its login goes.
        contexts
            .login(Some(&flag("dev")), token("good"))
            .await
            .unwrap();
        contexts
            .derive("eu2", "dev", None, OPS, true)
            .await
            .unwrap();
        assert_eq!(
            contexts.inspect("dev").await.unwrap().context.login_name(),
            "eu2"
        );
        let report = contexts
            .derive("acme", "eu2-again", None, OPS, false)
            .await
            .map(|_| ());
        assert!(report.is_ok());
    }

    #[tokio::test]
    async fn update_changes_what_each_kind_has() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;
        contexts.login(None, token("good")).await.unwrap();
        contexts
            .derive("eu2", "acme", None, SILENT, false)
            .await
            .unwrap();

        // A login has no role; a derived context no endpoint.
        let report = contexts
            .update(
                "eu2",
                ContextUpdate {
                    role: Some(OPS.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::NotDerived(_)));
        let report = contexts
            .update(
                "acme",
                ContextUpdate {
                    address: Some("x".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::NoEndpoint(..)));

        // A refused role changes nothing.
        let report = contexts
            .update(
                "acme",
                ContextUpdate {
                    role: Some(FORBIDDEN.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::Assume(_)));
        assert_eq!(
            contexts
                .resolve(Some(&flag("acme")))
                .await
                .unwrap()
                .role
                .unwrap()
                .urn,
            SILENT
        );

        let role = contexts
            .update(
                "acme",
                ContextUpdate {
                    role: Some(OPS.into()),
                    description: Some("ops".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(role.account.as_deref(), Some("other"));
        let acme = contexts.inspect("acme").await.unwrap().context;
        assert_eq!(acme.description.as_deref(), Some("ops"));
        assert_eq!(acme.derivation().unwrap().role, OPS);

        contexts
            .update(
                "eu2",
                ContextUpdate {
                    address: Some("moved".into()),
                    plaintext: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let eu2 = contexts
            .resolve(Some(&flag("acme")))
            .await
            .unwrap()
            .endpoint;
        assert_eq!((eu2.address.as_str(), eu2.tls.disabled), ("moved", true));
    }

    #[tokio::test]
    async fn a_login_goes_with_its_derived_contexts_only_when_asked() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;
        contexts.login(None, token("good")).await.unwrap();
        contexts
            .derive("eu2", "acme", None, OPS, false)
            .await
            .unwrap();
        contexts.use_context("acme").await.unwrap();

        let report = contexts.remove("eu2", false).await.unwrap_err();
        assert!(matches!(kind(&report), Error::InUse(..)));
        // A derived context goes alone, the login's credentials stay.
        create(&contexts, "dev").await;
        contexts
            .derive("eu2", "acme2", None, OPS, false)
            .await
            .unwrap();
        assert_eq!(contexts.remove("acme2", false).await.unwrap(), ["acme2"]);
        assert!(contexts.resolve(Some(&flag("eu2"))).await.is_ok());

        assert_eq!(contexts.remove("eu2", true).await.unwrap(), ["acme", "eu2"]);
        assert!(contexts.inspect("acme").await.is_err());
        assert_eq!(
            contexts.selected(None).await.unwrap(),
            Some(("dev".into(), Selection::Only))
        );
        // The login is gone: a new eu2 is logged out.
        create(&contexts, "eu2").await;
        let report = contexts.resolve(Some(&flag("eu2"))).await.unwrap_err();
        assert!(matches!(kind(&report), Error::NotLoggedIn(_)));
    }

    #[tokio::test]
    async fn renames_a_login_with_its_credentials_and_derived_contexts() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;
        create(&contexts, "dev").await;
        contexts.use_context("eu2").await.unwrap();
        contexts.login(None, token("good")).await.unwrap();
        contexts
            .derive("eu2", "acme", None, OPS, false)
            .await
            .unwrap();

        let report = contexts.rename("eu2", "dev").await.unwrap_err();
        assert!(matches!(kind(&report), Error::AlreadyExists(_)));
        let report = contexts.rename("eu2", "../x").await.unwrap_err();
        assert!(matches!(kind(&report), Error::InvalidName(_)));
        let report = contexts.rename("nope", "x").await.unwrap_err();
        assert!(matches!(kind(&report), Error::NotFound(_)));

        contexts.rename("eu2", "prod").await.unwrap();
        assert!(matches!(
            kind(&contexts.inspect("eu2").await.unwrap_err()),
            Error::NotFound(_)
        ));
        // Still current, still logged in, and acme follows it.
        assert_eq!(
            contexts.selected(None).await.unwrap(),
            Some(("prod".into(), Selection::Current))
        );
        assert!(contexts.resolve(None).await.is_ok());
        assert_eq!(
            contexts.resolve(Some(&flag("acme"))).await.unwrap().login,
            "prod"
        );

        // A derived context renames without touching its login.
        contexts.rename("acme", "acme-ops").await.unwrap();
        assert!(contexts.resolve(Some(&flag("acme-ops"))).await.is_ok());
        assert!(contexts.resolve(None).await.is_ok());
    }

    async fn account(contexts: &ContextControllerImpl) -> Option<String> {
        contexts
            .inspect("acme")
            .await
            .unwrap()
            .context
            .derivation()
            .unwrap()
            .account
            .clone()
    }

    #[tokio::test]
    async fn login_and_whoami_learn_the_accounts_roles_act_in() {
        let root = tempfile::tempdir().unwrap();
        let store = FileContextStoreImpl::new(root.path());
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;
        // As a migration leaves it: no account known.
        {
            use remora_context::adapter::store::ContextStoreAdapter;
            let mut all = store.load().unwrap();
            all.insert(Context {
                name: "acme".into(),
                description: None,
                kind: ContextKind::Derived(Derivation {
                    from: "eu2".into(),
                    role: OPS.into(),
                    account: None,
                }),
            });
            store.save(&all).unwrap();
        }
        assert_eq!(account(&contexts).await, None);
        contexts
            .login(Some(&flag("eu2")), token("good"))
            .await
            .unwrap();
        assert_eq!(account(&contexts).await.as_deref(), Some("other"));
        let (_, _, acting) = contexts.whoami(Some(&flag("acme"))).await.unwrap();
        assert_eq!(acting, Some(Some("other".into())));
    }

    #[tokio::test]
    async fn a_defined_context_needs_nothing_stored_and_stores_nothing() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        let env = defined("good", None);

        assert_eq!(
            contexts.selected(Some(&env)).await.unwrap(),
            Some(("env".into(), Selection::Defined))
        );
        let resolved = contexts.resolve(Some(&env)).await.unwrap();
        assert_eq!(resolved.selection, Selection::Defined);
        assert!(resolved.credentials == token("good"));
        let (_, principal, acting) = contexts.whoami(Some(&env)).await.unwrap();
        assert_eq!((principal.user_name.as_str(), acting), ("ada", None));
        assert!(
            std::fs::read_dir(root.path()).unwrap().next().is_none(),
            "a defined context wrote to the store"
        );

        // Refused credentials fail at the first call, as a login would.
        let report = contexts
            .whoami(Some(&defined("bad", None)))
            .await
            .unwrap_err();
        assert!(matches!(kind(&report), Error::Verify));
    }

    #[tokio::test]
    async fn a_defined_context_wins_over_the_stored_current_one() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;
        contexts.use_context("eu2").await.unwrap();

        // Not logged in to eu2: the defined context's own token is used.
        let resolved = contexts
            .resolve(Some(&defined("good", None)))
            .await
            .unwrap();
        assert_eq!(resolved.name, "env");
        assert_eq!(resolved.endpoint, endpoint("env"));
        let report = contexts.resolve(None).await.unwrap_err();
        assert!(matches!(kind(&report), Error::NotLoggedIn(_)));
    }

    #[tokio::test]
    async fn a_defined_context_s_role_is_verified_when_used() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());

        let ops = defined("good", Some(OPS));
        let role = contexts.resolve(Some(&ops)).await.unwrap().role.unwrap();
        assert_eq!((role.urn.as_str(), role.account), (OPS, None));
        let (_, _, acting) = contexts.whoami(Some(&ops)).await.unwrap();
        assert_eq!(acting, Some(Some("other".into())));

        // Nothing verified it beforehand: the first call that assumes it
        // fails, like a stored context whose role was revoked.
        let forbidden = defined("good", Some(FORBIDDEN));
        assert!(contexts.resolve(Some(&forbidden)).await.is_ok());
        let report = contexts.whoami(Some(&forbidden)).await.unwrap_err();
        assert!(matches!(kind(&report), Error::Assume(_)));
    }

    #[tokio::test]
    async fn a_defined_context_cannot_be_logged_in_or_out() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        create(&contexts, "eu2").await;
        let env = defined("good", None);
        let env = Some(&env);

        let refused = |report: Report<Error>| {
            assert!(
                matches!(kind(&report), Error::Defined(name) if name == "env"),
                "{report:?}"
            )
        };
        refused(contexts.login(env, token("good")).await.unwrap_err());
        refused(contexts.logout(env).await.unwrap_err());

        // Naming a stored context (--context) still logs it in.
        let eu2 = flag("eu2");
        contexts.login(Some(&eu2), token("good")).await.unwrap();
        assert_eq!(
            contexts.logout(Some(&eu2)).await.unwrap(),
            ("eu2".into(), true)
        );
    }
}
