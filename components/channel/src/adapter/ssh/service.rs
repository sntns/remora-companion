use super::error::Result;
use crate::model::SshCommand;

/// Runs the operator's ssh client. Behind a port because it is a
/// subprocess with the terminal handed over to it.
#[async_trait::async_trait]
pub trait SshClientAdapter: Send + Sync {
    /// Runs `command` on this process's terminal and returns its exit code
    /// (128 + n when killed by signal n, as a shell reports it). Signals
    /// that would end this process are relayed to ssh instead, so the
    /// caller always gets to clean up after it.
    async fn run(&self, command: &SshCommand) -> Result<i32>;
}

#[derive(Clone)]
pub struct SshClientAdapterService(busybody::Service<Box<dyn SshClientAdapter>>);

impl SshClientAdapterService {
    pub fn new<T: SshClientAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for SshClientAdapterService {
    type Target = busybody::Service<Box<dyn SshClientAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
