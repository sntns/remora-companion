use super::error::Result;
use crate::model::{App, UpdateCheck};

/// The update vertical's application-facing port.
#[async_trait::async_trait]
pub trait UpdateServiceInterface: Send + Sync {
    /// Whether a newer release than `app`'s exists, and how `app` was
    /// installed.
    async fn check(&self, app: &App) -> Result<UpdateCheck>;
    /// Installs `check`'s latest release over `app`, where it is installed.
    /// `force` also replaces a binary its installer didn't put there (never
    /// a Homebrew one: brew would not know).
    async fn update(&self, app: &App, check: &UpdateCheck, force: bool) -> Result<()>;
}

#[derive(Clone)]
pub struct UpdateService(busybody::Service<Box<dyn UpdateServiceInterface>>);

impl UpdateService {
    pub fn new<T: UpdateServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for UpdateService {
    type Target = busybody::Service<Box<dyn UpdateServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
