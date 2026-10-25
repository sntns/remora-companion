use std::time::{Duration, SystemTime};

use error_stack::{Report, ResultExt};
use remora_update::{
    adapter::{
        feed::ReleaseFeedAdapterService, installer::InstallerAdapterService,
        state::CheckStateAdapterService,
    },
    application::{Error, Result, UpdateServiceInterface},
    model::{App, Installation, LastCheck, UpdateCheck},
};

/// The update vertical's use case: where releases are, how to install one,
/// and when the binary last looked.
pub struct UpdateControllerImpl {
    feed: ReleaseFeedAdapterService,
    installer: InstallerAdapterService,
    state: CheckStateAdapterService,
}

impl UpdateControllerImpl {
    pub fn new(
        feed: ReleaseFeedAdapterService,
        installer: InstallerAdapterService,
        state: CheckStateAdapterService,
    ) -> Self {
        Self {
            feed,
            installer,
            state,
        }
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

    async fn notice(&self, app: &App, every: Duration) -> Result<Option<UpdateCheck>> {
        let now = SystemTime::now();
        // Unreadable state is as good as none: look again.
        let last = self.state.load(&app.name).await.ok().flatten();
        let fresh = last.as_ref().is_some_and(|last| {
            now.duration_since(last.checked_at)
                .is_ok_and(|age| age < every)
        });
        let latest = match last {
            Some(last) if fresh => last.latest,
            last => {
                let found = self.feed.latest().await;
                // A failure counts as a look too, keeping what was known:
                // offline, every command would otherwise try again.
                let latest = match &found {
                    Ok(latest) => Some(latest.clone()),
                    Err(_) => last.and_then(|last| last.latest),
                };
                let checked = LastCheck {
                    checked_at: now,
                    latest: latest.clone(),
                };
                let _ = self.state.save(&app.name, &checked).await;
                found.change_context(Error::Check)?;
                latest
            }
        };
        let Some(latest) = latest.filter(|latest| latest.version > app.version) else {
            return Ok(None);
        };
        let installation = self
            .installer
            .installation(app)
            .await
            .change_context(Error::Check)?;
        Ok(Some(UpdateCheck {
            current: app.version.clone(),
            latest,
            installation,
        }))
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
            state::{self, CheckStateAdapter},
        },
        model::{Release, Version},
    };

    use super::*;

    /// Stubs of the network/subprocess/file ports; the GitHub and file
    /// adapters have their own tests. The feed counts how often it is
    /// asked, and fails while `offline`.
    #[derive(Default, Clone)]
    struct Feed {
        asked: Arc<Mutex<usize>>,
        offline: Arc<Mutex<bool>>,
    }

    #[async_trait::async_trait]
    impl ReleaseFeedAdapter for Feed {
        async fn latest(&self) -> feed::Result<Release> {
            *self.asked.lock().unwrap() += 1;
            if *self.offline.lock().unwrap() {
                return Err(error_stack::Report::new(feed::Error::Unreachable));
            }
            Ok(Release {
                version: Version::parse("0.4.0").unwrap(),
                tag: "v0.4.0".into(),
            })
        }
    }

    #[derive(Default, Clone)]
    struct State(Arc<Mutex<Option<LastCheck>>>);

    #[async_trait::async_trait]
    impl CheckStateAdapter for State {
        async fn load(&self, _: &str) -> state::Result<Option<LastCheck>> {
            Ok(self.0.lock().unwrap().clone())
        }

        async fn save(&self, _: &str, check: &LastCheck) -> state::Result<()> {
            *self.0.lock().unwrap() = Some(check.clone());
            Ok(())
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
        let (controller, installed, _, _) = controller_with(installation);
        (controller, installed)
    }

    fn controller_with(
        installation: Installation,
    ) -> (UpdateControllerImpl, Arc<Mutex<Vec<PathBuf>>>, Feed, State) {
        let installed = Arc::new(Mutex::new(Vec::new()));
        let (feed, state) = (Feed::default(), State::default());
        (
            UpdateControllerImpl::new(
                ReleaseFeedAdapterService::new(feed.clone()),
                InstallerAdapterService::new(Installer(installation, installed.clone())),
                CheckStateAdapterService::new(state.clone()),
            ),
            installed,
            feed,
            state,
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

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    /// The feed is asked once a day at most: in between, what it said
    /// stands -- and a newer release is told about only to an older binary.
    #[tokio::test]
    async fn notices_a_newer_release_at_most_daily() {
        let installation = Installation::Installer {
            dir: "/home/ada/.local/bin".into(),
        };
        let (update, _, feed, state) = controller_with(installation.clone());

        let notice = update.notice(&app("0.3.0"), DAY).await.unwrap().unwrap();
        assert_eq!(notice.latest.tag, "v0.4.0");
        assert_eq!(notice.installation, installation);
        assert!(update.notice(&app("0.3.0"), DAY).await.unwrap().is_some());
        assert!(update.notice(&app("0.4.0"), DAY).await.unwrap().is_none());
        assert_eq!(*feed.asked.lock().unwrap(), 1);

        // A day on, it is asked again.
        state.0.lock().unwrap().as_mut().unwrap().checked_at -= DAY;
        update.notice(&app("0.3.0"), DAY).await.unwrap();
        assert_eq!(*feed.asked.lock().unwrap(), 2);
    }

    /// Offline, the failure is a look too: not asked again before the day
    /// is out, and what was known before still stands.
    #[tokio::test]
    async fn an_offline_look_waits_a_day_too() {
        let (update, _, feed, state) = controller_with(Installation::Installer {
            dir: "/home/ada/.local/bin".into(),
        });
        update.notice(&app("0.3.0"), DAY).await.unwrap();
        state.0.lock().unwrap().as_mut().unwrap().checked_at -= DAY;
        *feed.offline.lock().unwrap() = true;

        assert!(update.notice(&app("0.3.0"), DAY).await.is_err());
        let notice = update.notice(&app("0.3.0"), DAY).await.unwrap();
        assert_eq!(notice.unwrap().latest.tag, "v0.4.0");
        assert_eq!(*feed.asked.lock().unwrap(), 2);
    }
}
