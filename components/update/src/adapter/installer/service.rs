use std::path::Path;

use super::error::Result;
use crate::model::{App, Installation, Release};

/// Installs releases of an app, the way its own installers do.
#[async_trait::async_trait]
pub trait InstallerAdapter: Send + Sync {
    /// How the running `app` was installed.
    async fn installation(&self, app: &App) -> Result<Installation>;
    /// Installs `release` of `app` into `dir`, replacing what is there.
    async fn install(&self, app: &App, release: &Release, dir: &Path) -> Result<()>;
}

#[derive(Clone)]
pub struct InstallerAdapterService(busybody::Service<Box<dyn InstallerAdapter>>);

impl InstallerAdapterService {
    pub fn new<T: InstallerAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for InstallerAdapterService {
    type Target = busybody::Service<Box<dyn InstallerAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
