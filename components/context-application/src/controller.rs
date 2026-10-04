use error_stack::{Report, ResultExt};
use remora_context::{
    adapter::{
        credentials::CredentialStoreAdapterService, platform::PlatformSessionAdapterService,
        store::ContextStoreAdapterService,
    },
    application::{ContextServiceInterface, Error, Result},
    model::{
        AssumedRole, Context, ContextOverride, ContextSummary, Credentials, Principal,
        ResolvedContext, RoleOverride, RoleSummary, Selection,
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
    /// It also makes every name a safe single path component for a store.
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

    fn existing(&self, name: &str) -> Result<Context> {
        Self::validate(name)?;
        self.store
            .get(name)
            .change_context(Error::Store)?
            .ok_or_else(|| Report::new(Error::NotFound(name.to_owned())))
    }

    /// What a role choice (an alias of `context`'s roles, or a role URN)
    /// names.
    fn role_named(context: &Context, choice: &str) -> Result<AssumedRole> {
        if let Some(urn) = context.roles.get(choice) {
            return Ok(AssumedRole {
                alias: Some(choice.to_owned()),
                urn: urn.clone(),
            });
        }
        if choice.starts_with("urn:") {
            Self::validate_role_urn(choice)?;
            // A URN that has an alias reads as the alias.
            let alias = context
                .roles
                .iter()
                .find(|(_, urn)| urn.as_str() == choice)
                .map(|(alias, _)| alias.clone());
            return Ok(AssumedRole {
                alias,
                urn: choice.to_owned(),
            });
        }
        Err(Report::new(Error::UnknownRole(choice.to_owned())))
    }

    fn validate_role_urn(urn: &str) -> Result<()> {
        if urn.starts_with("urn:") && urn.contains(":role:") {
            Ok(())
        } else {
            Err(Report::new(Error::InvalidRoleUrn(urn.to_owned())))
        }
    }

    /// The role an invocation acts as: the override's, else the context's.
    fn role_for(context: &Context, over: Option<&ContextOverride>) -> Result<Option<AssumedRole>> {
        let choice = match over.map(|o| &o.role) {
            Some(RoleOverride::Drop) => None,
            Some(RoleOverride::Assume(choice)) => Some(choice.as_str()),
            Some(RoleOverride::Keep) | None => context.assumed_role.as_deref(),
        };
        choice
            .map(|choice| Self::role_named(context, choice))
            .transpose()
    }

    /// The selected context itself, which must exist.
    fn select(&self, over: Option<&ContextOverride>) -> Result<(Context, Selection)> {
        if let Some((name, source)) = over.and_then(|o| o.name.as_ref().map(|n| (n, o.source))) {
            return Ok((self.existing(name)?, source));
        }
        if let Some(current) = self.store.current().change_context(Error::Store)? {
            // A current pointer left dangling by hand-editing reads as the
            // missing context it names, not as "no context".
            return Ok((self.existing(&current)?, Selection::Current));
        }
        let mut contexts = self.store.list().change_context(Error::Store)?;
        match contexts.len() {
            0 => Err(Report::new(Error::NoContext)),
            1 => Ok((contexts.remove(0), Selection::Only)),
            _ => Err(Report::new(Error::Ambiguous)),
        }
    }
}

#[async_trait::async_trait]
impl ContextServiceInterface for ContextControllerImpl {
    async fn list(&self) -> Result<Vec<ContextSummary>> {
        let current = self.store.current().change_context(Error::Store)?;
        let contexts = self.store.list().change_context(Error::Store)?;
        let only = contexts.len() == 1 && current.is_none();
        contexts
            .into_iter()
            .map(|context| {
                let credentials = self
                    .credentials
                    .get(&context.name)
                    .change_context(Error::Credentials)?;
                Ok(ContextSummary {
                    current: only || current.as_deref() == Some(context.name.as_str()),
                    credentials: credentials.map(|c| c.kind()),
                    context,
                })
            })
            .collect()
    }

    async fn inspect(&self, name: &str) -> Result<ContextSummary> {
        let context = self.existing(name)?;
        let current = self.store.current().change_context(Error::Store)?;
        let credentials = self
            .credentials
            .get(name)
            .change_context(Error::Credentials)?;
        Ok(ContextSummary {
            current: current.as_deref() == Some(name),
            credentials: credentials.map(|c| c.kind()),
            context,
        })
    }

    async fn create(&self, mut context: Context, replace: bool) -> Result<()> {
        Self::validate(&context.name)?;
        if let Some(choice) = &context.assumed_role {
            // Checked as a reference only: no credentials yet to verify it
            // with the platform -- the login does that.
            Self::role_named(&context, choice)?;
        }
        if let Some(existing) = self.store.get(&context.name).change_context(Error::Store)? {
            if !replace {
                return Err(Report::new(Error::AlreadyExists(context.name)));
            }
            // Replacing moves the endpoint; like the login, the roles stay.
            if context.roles.is_empty() {
                context.roles = existing.roles;
            }
            if context.assumed_role.is_none() {
                context.assumed_role = existing.assumed_role;
            }
        }
        self.store.put(&context).change_context(Error::Store)
    }

    async fn remove(&self, name: &str) -> Result<()> {
        self.existing(name)?;
        self.credentials
            .delete(name)
            .change_context(Error::Credentials)?;
        self.store.delete(name).change_context(Error::Store)?;
        if self
            .store
            .current()
            .change_context(Error::Store)?
            .as_deref()
            == Some(name)
        {
            self.store.set_current(None).change_context(Error::Store)?;
        }
        Ok(())
    }

    async fn use_context(&self, name: &str) -> Result<()> {
        self.existing(name)?;
        self.store
            .set_current(Some(name))
            .change_context(Error::Store)
    }

    async fn selected(
        &self,
        over: Option<&ContextOverride>,
    ) -> Result<Option<(String, Selection)>> {
        match self.select(over) {
            Ok((context, selection)) => Ok(Some((context.name, selection))),
            Err(report) if matches!(report.current_context(), Error::NoContext) => Ok(None),
            Err(report) => Err(report),
        }
    }

    async fn resolve(&self, over: Option<&ContextOverride>) -> Result<ResolvedContext> {
        let (context, selection) = self.select(over)?;
        let credentials = self
            .credentials
            .get(&context.name)
            .change_context(Error::Credentials)?
            .ok_or_else(|| Report::new(Error::NotLoggedIn(context.name.clone())))?;
        let role = Self::role_for(&context, over)?;
        Ok(ResolvedContext {
            context,
            credentials,
            selection,
            role,
        })
    }

    async fn login(
        &self,
        over: Option<&ContextOverride>,
        credentials: Credentials,
        role: RoleOverride,
    ) -> Result<(String, Principal, Option<(AssumedRole, Option<String>)>)> {
        let (mut context, _) = self.select(over)?;
        // Verified first: storing credentials the platform refuses would
        // only move the failure to the next command, further from its cause.
        let principal = self
            .platform
            .whoami(&context, &credentials)
            .await
            .change_context(Error::Verify)?;
        // The context's role is part of what logging in establishes: checked
        // with these credentials too, before anything is stored.
        let choice = match role {
            RoleOverride::Keep => context.assumed_role.clone(),
            RoleOverride::Assume(choice) => Some(choice),
            RoleOverride::Drop => None,
        };
        let assumed = match &choice {
            None => None,
            Some(choice) => {
                let role = Self::role_named(&context, choice)?;
                let account = self
                    .platform
                    .acting_account(&context, &credentials, &role.urn)
                    .await
                    .change_context_lazy(|| Error::Assume(role.display_name().to_owned()))?;
                Some((role, account))
            }
        };
        self.credentials
            .put(&context.name, &credentials)
            .change_context(Error::Credentials)?;
        if context.assumed_role != choice {
            context.assumed_role = choice;
            self.store.put(&context).change_context(Error::Store)?;
        }
        Ok((context.name, principal, assumed))
    }

    async fn logout(&self, over: Option<&ContextOverride>) -> Result<(String, bool)> {
        let (context, _) = self.select(over)?;
        let removed = self
            .credentials
            .delete(&context.name)
            .change_context(Error::Credentials)?;
        Ok((context.name, removed))
    }

    async fn whoami(
        &self,
        over: Option<&ContextOverride>,
    ) -> Result<(ResolvedContext, Principal, Option<Option<String>>)> {
        let resolved = self.resolve(over).await?;
        let principal = self
            .platform
            .whoami(&resolved.context, &resolved.credentials)
            .await
            .change_context(Error::Verify)?;
        let acting = match &resolved.role {
            None => None,
            Some(role) => Some(
                self.platform
                    .acting_account(&resolved.context, &resolved.credentials, &role.urn)
                    .await
                    .change_context_lazy(|| Error::Assume(role.display_name().to_owned()))?,
            ),
        };
        Ok((resolved, principal, acting))
    }

    async fn roles(&self, over: Option<&ContextOverride>) -> Result<(String, Vec<RoleSummary>)> {
        let (context, _) = self.select(over)?;
        let assumed = context
            .assumed_role
            .as_deref()
            .and_then(|choice| Self::role_named(&context, choice).ok());
        let roles = context
            .roles
            .iter()
            .map(|(alias, urn)| RoleSummary {
                alias: alias.clone(),
                urn: urn.clone(),
                assumed: assumed.as_ref().is_some_and(|role| &role.urn == urn),
            })
            .collect();
        Ok((context.name, roles))
    }

    async fn add_role(&self, over: Option<&ContextOverride>, alias: &str, urn: &str) -> Result<()> {
        // Aliases follow context names' rule, and can't look like a URN.
        Self::validate(alias)?;
        Self::validate_role_urn(urn)?;
        let (mut context, _) = self.select(over)?;
        if context.roles.contains_key(alias) {
            return Err(Report::new(Error::RoleExists(alias.to_owned())));
        }
        context.roles.insert(alias.to_owned(), urn.to_owned());
        self.store.put(&context).change_context(Error::Store)
    }

    async fn remove_role(&self, over: Option<&ContextOverride>, alias: &str) -> Result<()> {
        let (mut context, _) = self.select(over)?;
        let Some(urn) = context.roles.remove(alias) else {
            return Err(Report::new(Error::UnknownRole(alias.to_owned())));
        };
        if matches!(context.assumed_role.as_deref(), Some(a) if a == alias || a == urn) {
            context.assumed_role = None;
        }
        self.store.put(&context).change_context(Error::Store)
    }

    async fn assume_role(
        &self,
        over: Option<&ContextOverride>,
        choice: &str,
    ) -> Result<(String, AssumedRole, Option<String>)> {
        let resolved = self.resolve(over).await?;
        let role = Self::role_named(&resolved.context, choice)?;
        // Verified first, like a login: storing a role the platform refuses
        // would only fail every later command instead.
        let account = self
            .platform
            .acting_account(&resolved.context, &resolved.credentials, &role.urn)
            .await
            .change_context_lazy(|| Error::Assume(role.display_name().to_owned()))?;
        let mut context = resolved.context;
        context.assumed_role = Some(choice.to_owned());
        self.store.put(&context).change_context(Error::Store)?;
        Ok((context.name, role, account))
    }

    async fn drop_role(&self, over: Option<&ContextOverride>) -> Result<(String, Option<String>)> {
        let (mut context, _) = self.select(over)?;
        let dropped = context.assumed_role.take();
        if dropped.is_some() {
            self.store.put(&context).change_context(Error::Store)?;
        }
        Ok((context.name, dropped))
    }
}

#[cfg(test)]
mod tests {
    use remora_context::{
        adapter::platform::{self, PlatformSessionAdapter},
        model::{Endpoint, Secret, Tls},
    };
    use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};

    use super::*;

    /// Stands in for the platform: accepts exactly one token. A hand-written
    /// stub of the one network port, the real file adapters for the rest.
    struct Platform;

    #[async_trait::async_trait]
    impl PlatformSessionAdapter for Platform {
        async fn whoami(
            &self,
            _: &Context,
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
            _: &Context,
            _: &Credentials,
            role: &str,
        ) -> platform::Result<Option<String>> {
            match role {
                "urn:sntns:iam:eu2:other:role:ops" => Ok(Some("other".into())),
                _ => Err(Report::new(platform::Error::RoleRefused)),
            }
        }
    }

    fn controller(root: &std::path::Path) -> ContextControllerImpl {
        ContextControllerImpl::new(
            ContextStoreAdapterService::new(FileContextStoreImpl::new(root)),
            CredentialStoreAdapterService::new(FileCredentialStoreImpl::new(root)),
            PlatformSessionAdapterService::new(Platform),
        )
    }

    fn context(name: &str) -> Context {
        Context {
            name: name.into(),
            description: None,
            endpoint: Endpoint {
                address: format!("{name}.example:50051"),
                tls: Tls::default(),
            },
            roles: Default::default(),
            assumed_role: None,
        }
    }

    fn token(token: &str) -> Credentials {
        Credentials {
            secret: Secret::AccessKey {
                token: token.into(),
            },
        }
    }

    fn flag(name: &str) -> ContextOverride {
        ContextOverride {
            name: Some(name.into()),
            source: Selection::Flag,
            role: RoleOverride::Keep,
        }
    }

    fn role(role: RoleOverride) -> ContextOverride {
        ContextOverride {
            name: None,
            source: Selection::Flag,
            role,
        }
    }

    #[tokio::test]
    async fn rejects_names_that_are_not_docker_names() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        for bad in ["", "-x", "../etc", "a/b", "é"] {
            let report = contexts.create(context(bad), false).await.unwrap_err();
            assert!(
                matches!(report.current_context(), Error::InvalidName(_)),
                "{bad:?}"
            );
        }
        contexts
            .create(context("eu2-prod.v1"), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn selection_follows_flag_then_current_then_only() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        assert!(contexts.selected(None).await.unwrap().is_none());

        contexts.create(context("eu2"), false).await.unwrap();
        assert_eq!(
            contexts.selected(None).await.unwrap(),
            Some(("eu2".into(), Selection::Only))
        );

        contexts.create(context("dev"), false).await.unwrap();
        let report = contexts.selected(None).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Ambiguous));

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
        assert!(matches!(report.current_context(), Error::NotFound(_)));
    }

    #[tokio::test]
    async fn login_stores_only_verified_credentials() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        contexts.create(context("eu2"), false).await.unwrap();

        let report = contexts
            .login(None, token("bad"), RoleOverride::Keep)
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Verify));
        let report = contexts.resolve(None).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::NotLoggedIn(_)));

        let (name, principal, _) = contexts
            .login(None, token("good"), RoleOverride::Keep)
            .await
            .unwrap();
        assert_eq!(
            (name.as_str(), principal.user_name.as_str()),
            ("eu2", "ada")
        );
        let resolved = contexts.resolve(None).await.unwrap();
        assert!(resolved.credentials == token("good"));
        assert_eq!(
            contexts
                .whoami(None)
                .await
                .unwrap()
                .1
                .account_name
                .as_deref(),
            Some("acme")
        );

        assert_eq!(contexts.logout(None).await.unwrap(), ("eu2".into(), true));
        assert_eq!(contexts.logout(None).await.unwrap(), ("eu2".into(), false));
    }

    #[tokio::test]
    async fn remove_forgets_credentials_and_current() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        contexts.create(context("eu2"), false).await.unwrap();
        contexts.use_context("eu2").await.unwrap();
        contexts
            .login(None, token("good"), RoleOverride::Keep)
            .await
            .unwrap();

        contexts.remove("eu2").await.unwrap();
        assert!(contexts.list().await.unwrap().is_empty());
        assert!(contexts.selected(None).await.unwrap().is_none());

        // Re-creating the name must not resurrect the old login.
        contexts.create(context("eu2"), false).await.unwrap();
        let report = contexts.resolve(None).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::NotLoggedIn(_)));
    }

    #[tokio::test]
    async fn replacing_a_context_keeps_its_login() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        contexts.create(context("eu2"), false).await.unwrap();
        contexts
            .login(None, token("good"), RoleOverride::Keep)
            .await
            .unwrap();

        let report = contexts.create(context("eu2"), false).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::AlreadyExists(_)));

        let mut moved = context("eu2");
        moved.endpoint.address = "elsewhere:50051".into();
        contexts.create(moved, true).await.unwrap();
        let summary = contexts.inspect("eu2").await.unwrap();
        assert_eq!(summary.context.endpoint.address, "elsewhere:50051");
        assert!(summary.credentials.is_some());
    }

    const OPS: &str = "urn:sntns:iam:eu2:other:role:ops";
    const FORBIDDEN: &str = "urn:sntns:iam:eu2:other:role:forbidden";

    #[tokio::test]
    async fn roles_are_remembered_and_validated() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        contexts.create(context("eu2"), false).await.unwrap();

        contexts.add_role(None, "ops", OPS).await.unwrap();
        let report = contexts.add_role(None, "ops", OPS).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::RoleExists(_)));
        let report = contexts
            .add_role(None, "bad", "not-a-urn")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::InvalidRoleUrn(_)));
        let report = contexts.add_role(None, "urn:x", OPS).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::InvalidName(_)));

        let (name, roles) = contexts.roles(None).await.unwrap();
        assert_eq!(name, "eu2");
        assert_eq!(roles.len(), 1);
        assert!(!roles[0].assumed);

        // Replacing the context keeps its roles, like its login.
        contexts.create(context("eu2"), true).await.unwrap();
        assert_eq!(contexts.roles(None).await.unwrap().1.len(), 1);

        contexts.remove_role(None, "ops").await.unwrap();
        assert!(contexts.roles(None).await.unwrap().1.is_empty());
    }

    #[tokio::test]
    async fn an_assumed_role_applies_until_dropped_and_overrides_win() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        contexts.create(context("eu2"), false).await.unwrap();
        contexts
            .login(None, token("good"), RoleOverride::Keep)
            .await
            .unwrap();
        contexts.add_role(None, "ops", OPS).await.unwrap();
        assert_eq!(contexts.resolve(None).await.unwrap().role, None);

        // Refused by the platform: nothing is stored.
        let report = contexts.assume_role(None, FORBIDDEN).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Assume(_)));
        let report = contexts.assume_role(None, "nope").await.unwrap_err();
        assert!(matches!(report.current_context(), Error::UnknownRole(_)));
        assert_eq!(contexts.resolve(None).await.unwrap().role, None);

        let (_, assumed, account) = contexts.assume_role(None, "ops").await.unwrap();
        assert_eq!(
            (assumed.alias.as_deref(), assumed.urn.as_str()),
            (Some("ops"), OPS)
        );
        assert_eq!(account.as_deref(), Some("other"));
        let resolved = contexts.resolve(None).await.unwrap();
        assert_eq!(resolved.role.as_ref().map(|r| r.urn.as_str()), Some(OPS));
        assert!(contexts.roles(None).await.unwrap().1[0].assumed);
        let (_, principal, acting) = contexts.whoami(None).await.unwrap();
        assert_eq!(principal.user_name, "ada");
        assert_eq!(acting, Some(Some("other".into())));

        // Per invocation: drop it, or name another (a URN works too).
        let dropped = contexts
            .resolve(Some(&role(RoleOverride::Drop)))
            .await
            .unwrap();
        assert_eq!(dropped.role, None);
        let by_urn = contexts
            .resolve(Some(&role(RoleOverride::Assume(OPS.into()))))
            .await
            .unwrap();
        assert_eq!(by_urn.role.unwrap().alias.as_deref(), Some("ops"));

        assert_eq!(
            contexts.drop_role(None).await.unwrap(),
            ("eu2".into(), Some("ops".into()))
        );
        assert_eq!(contexts.resolve(None).await.unwrap().role, None);
    }

    #[tokio::test]
    async fn the_role_is_part_of_the_login() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        contexts.create(context("eu2"), false).await.unwrap();
        contexts.add_role(None, "ops", OPS).await.unwrap();

        // A refused role refuses the whole login: nothing is stored.
        let report = contexts
            .login(None, token("good"), RoleOverride::Assume(FORBIDDEN.into()))
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Assume(_)));
        let report = contexts.resolve(None).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::NotLoggedIn(_)));

        let (_, _, assumed) = contexts
            .login(None, token("good"), RoleOverride::Assume("ops".into()))
            .await
            .unwrap();
        let (role, account) = assumed.unwrap();
        assert_eq!(
            (role.urn.as_str(), account.as_deref()),
            (OPS, Some("other"))
        );
        assert_eq!(contexts.resolve(None).await.unwrap().role.unwrap().urn, OPS);

        // Logging in again keeps the context's role, verifying it anew...
        let (_, _, assumed) = contexts
            .login(None, token("good"), RoleOverride::Keep)
            .await
            .unwrap();
        assert!(assumed.is_some());
        // ...or drops it.
        let (_, _, assumed) = contexts
            .login(None, token("good"), RoleOverride::Drop)
            .await
            .unwrap();
        assert!(assumed.is_none());
        assert_eq!(contexts.resolve(None).await.unwrap().role, None);
    }

    #[tokio::test]
    async fn a_context_can_be_created_with_its_role() {
        let root = tempfile::tempdir().unwrap();
        let contexts = controller(root.path());
        let mut with_role = context("acme");
        with_role.assumed_role = Some(OPS.into());
        contexts.create(with_role, false).await.unwrap();
        let mut unknown = context("bad");
        unknown.assumed_role = Some("no-such-alias".into());
        let report = contexts.create(unknown, false).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::UnknownRole(_)));

        // The login verifies it.
        let (_, _, assumed) = contexts
            .login(None, token("good"), RoleOverride::Keep)
            .await
            .unwrap();
        assert_eq!(assumed.unwrap().1.as_deref(), Some("other"));
    }
}
