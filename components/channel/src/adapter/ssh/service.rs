use remora_progress::OperationContext;

use super::error::Result;
use crate::model::{ExecInput, ExecOutcome, SshCommand, SshKeys};

/// Runs the operator's ssh client. Behind a port because it is a
/// subprocess with the terminal handed over to it.
#[async_trait::async_trait]
pub trait SshClientAdapter: Send + Sync {
    /// Runs `command` on this process's terminal with `keys` as its
    /// identity, certificate and known hosts, and returns its exit code
    /// (128 + n when killed by signal n, as a shell reports it). The keys
    /// are only on disk -- privately -- while the client runs: removed
    /// however it ends. Signals that would end this process are relayed to
    /// ssh instead, so the caller always gets to clean up after it.
    async fn run(&self, command: &SshCommand, keys: &SshKeys) -> Result<i32>;

    /// Runs `command` without a terminal: `input`, if any, fed to its stdin
    /// from its offset (the bytes sent reported through `ctx` as
    /// `progress_bytes`, out of the file's size), its stdout and stderr
    /// captured -- and, with `echo`, each line logged through `ctx` as it
    /// comes. Cancelling `ctx` kills it ([`super::Error::Cancelled`]).
    async fn exec(
        &self,
        command: &SshCommand,
        keys: &SshKeys,
        input: Option<&ExecInput>,
        echo: bool,
        ctx: &OperationContext,
    ) -> Result<ExecOutcome>;
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
