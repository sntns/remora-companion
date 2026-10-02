use error_stack::{Report, ResultExt};
use remora_update::{
    adapter::{feed::ReleaseFeedAdapterService, installer::InstallerAdapterService},
    application::{Error, Result, UpdateServiceInterface},
    model::{App, Installation, UpdateCheck},
};

/// The update vertical's use case: where releases are, and how to install one.
pub struct UpdateControllerImpl {
    feed: ReleaseFeedAdapterService,
    installer: InstallerAdapterService,
}

impl UpdateControllerImpl {
    pub fn new(feed: ReleaseFeedAdapterService, installer: InstallerAdapterService) -> Self {
        Self { feed, installer }
    }
}

#[async_trait::async_trait]
impl UpdateServiceInterface for UpdateControllerImpl {
    async fn check(&self, app: &App) -> Result<UpdateCheck> {
        let latest = self.feed.latest().await.change_context(Error::Check)?;
        let installation = self
            .installer
            .installation(app)
            .await
            .change_context(Error::Check)?;
        Ok(UpdateCheck {
            current: app.version.clone(),
            latest,
            installation,
        })
    }

    async fn update(&self, app: &App, check: &UpdateCheck, force: bool) -> Result<()> {
        let dir = match &check.installation {
            Installation::Homebrew { formula } => {
                return Err(Report::new(Error::Homebrew(formula.clone())))
            }
            Installation::Unmanaged { .. } if !force => {
                return Err(Report::new(Error::Unmanaged(app.name.clone())))
            }
            Installation::Installer { dir } | Installation::Unmanaged { dir } => dir,
        };
        self.installer
            .install(app, &check.latest, dir)
            .await
            .change_context_lazy(|| {
                Error::Install(format!("{} {}", app.name, check.latest.version))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use remora_update::{
        adapter::{
            feed::{self, ReleaseFeedAdapter},
            installer::{self, InstallerAdapter},
        },
        model::{Release, Version},
    };

    use super::*;

    /// Stubs of the two network/subprocess ports; the GitHub adapter has
    /// its own tests against a local HTTP server.
    struct Feed;

    #[async_trait::async_trait]
    impl ReleaseFeedAdapter for Feed {
        async fn latest(&self) -> feed::Result<Release> {
            Ok(Release {
                version: Version::parse("0.4.0").unwrap(),
                tag: "v0.4.0".into(),
            })
        }
    }

    struct Installer(Installation, Arc<Mutex<Vec<PathBuf>>>);

    #[async_trait::async_trait]
    impl InstallerAdapter for Installer {
        async fn installation(&self, _: &App) -> installer::Result<Installation> {
            Ok(self.0.clone())
        }

        async fn install(&self, _: &App, _: &Release, dir: &Path) -> installer::Result<()> {
            self.1.lock().unwrap().push(dir.to_path_buf());
            Ok(())
        }
    }

    fn controller(installation: Installation) -> (UpdateControllerImpl, Arc<Mutex<Vec<PathBuf>>>) {
        let installed = Arc::new(Mutex::new(Vec::new()));
        (
            UpdateControllerImpl::new(
                ReleaseFeedAdapterService::new(Feed),
                InstallerAdapterService::new(Installer(installation, installed.clone())),
            ),
            installed,
        )
    }

    fn app(version: &str) -> App {
        App {
            name: "rmra".into(),
            version: Version::parse(version).unwrap(),
            executable: "/home/ada/.local/bin/rmra".into(),
        }
    }

    #[tokio::test]
    async fn updates_an_installer_installation_in_place() {
        let dir = PathBuf::from("/home/ada/.local/bin");
        let (update, installed) = controller(Installation::Installer { dir: dir.clone() });
        let check = update.check(&app("0.3.0")).await.unwrap();
        assert!(check.available());
        update.update(&app("0.3.0"), &check, false).await.unwrap();
        assert_eq!(*installed.lock().unwrap(), [dir]);

        assert!(!update.check(&app("0.4.0")).await.unwrap().available());
        assert!(!update.check(&app("0.5.0-rc.1")).await.unwrap().available());
    }

    #[tokio::test]
    async fn defers_to_homebrew_and_needs_force_otherwise() {
        let (update, installed) = controller(Installation::Homebrew {
            formula: "sntns/tap/rmra".into(),
        });
        let check = update.check(&app("0.3.0")).await.unwrap();
        let report = update
            .update(&app("0.3.0"), &check, true)
            .await
            .unwrap_err();
        assert!(report.to_string().contains("brew upgrade sntns/tap/rmra"));

        let (update, _) = controller(Installation::Unmanaged {
            dir: "/opt/rmra".into(),
        });
        let check = update.check(&app("0.3.0")).await.unwrap();
        let report = update
            .update(&app("0.3.0"), &check, false)
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Unmanaged(_)));
        update.update(&app("0.3.0"), &check, true).await.unwrap();
        assert!(installed.lock().unwrap().is_empty());
    }
}
