use error_stack::{Report, ResultExt};
use remora_context::{
    adapter::{
        credentials::CredentialStoreAdapterService, platform::PlatformSessionAdapterService,
        store::ContextStoreAdapterService,
    },
    application::{ContextServiceInterface, Error, Result},
    model::{
        Context, ContextOverride, ContextSummary, Credentials, Principal, ResolvedContext,
        Selection,
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

    /// The selected context itself, which must exist.
    fn select(&self, over: Option<&ContextOverride>) -> Result<(Context, Selection)> {
        if let Some(over) = over {
            return Ok((self.existing(&over.name)?, over.source));
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

    async fn create(&self, context: Context, replace: bool) -> Result<()> {
        Self::validate(&context.name)?;
        if !replace
            && self
                .store
                .get(&context.name)
                .change_context(Error::Store)?
                .is_some()
        {
            return Err(Report::new(Error::AlreadyExists(context.name)));
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
        Ok(ResolvedContext {
            context,
            credentials,
            selection,
        })
    }

    async fn login(
        &self,
        over: Option<&ContextOverride>,
        credentials: Credentials,
    ) -> Result<(String, Principal)> {
        let (context, _) = self.select(over)?;
        // Verified first: storing credentials the platform refuses would
        // only move the failure to the next command, further from its cause.
        let principal = self
            .platform
            .whoami(&context, &credentials)
            .await
            .change_context(Error::Verify)?;
        self.credentials
            .put(&context.name, &credentials)
            .change_context(Error::Credentials)?;
        Ok((context.name, principal))
    }

    async fn logout(&self, over: Option<&ContextOverride>) -> Result<(String, bool)> {
        let (context, _) = self.select(over)?;
        let removed = self
            .credentials
            .delete(&context.name)
            .change_context(Error::Credentials)?;
        Ok((context.name, removed))
    }

    async fn whoami(&self, over: Option<&ContextOverride>) -> Result<(ResolvedContext, Principal)> {
        let resolved = self.resolve(over).await?;
        let principal = self
            .platform
            .whoami(&resolved.context, &resolved.credentials)
            .await
            .change_context(Error::Verify)?;
        Ok((resolved, principal))
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
        }
    }

    fn token(token: &str) -> Credentials {
        Credentials {
            secret: Secret::AccessKey {
                token: token.into(),
            },
            assume_role: None,
        }
    }

    fn flag(name: &str) -> ContextOverride {
        ContextOverride {
            name: name.into(),
            source: Selection::Flag,
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

        let report = contexts.login(None, token("bad")).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Verify));
        let report = contexts.resolve(None).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::NotLoggedIn(_)));

        let (name, principal) = contexts.login(None, token("good")).await.unwrap();
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
        contexts.login(None, token("good")).await.unwrap();

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
        contexts.login(None, token("good")).await.unwrap();

        let report = contexts.create(context("eu2"), false).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::AlreadyExists(_)));

        let mut moved = context("eu2");
        moved.endpoint.address = "elsewhere:50051".into();
        contexts.create(moved, true).await.unwrap();
        let summary = contexts.inspect("eu2").await.unwrap();
        assert_eq!(summary.context.endpoint.address, "elsewhere:50051");
        assert!(summary.credentials.is_some());
    }
}
