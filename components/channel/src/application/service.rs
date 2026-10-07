use remora_context::model::ContextOverride;
use remora_progress::OperationContext;

use super::error::Result;
use crate::{
    adapter::gateway::Channel,
    model::{ExecOutcome, ExecRequest, PreparedSsh, ScpRequest, SshRequest},
};

/// The channel vertical's application-facing port.
#[async_trait::async_trait]
pub trait ChannelServiceInterface: Send + Sync {
    /// Opens `profile` on `device` under the selected context. Only a
    /// stream channel: a datagram one is refused, as nothing relays it yet.
    async fn open(
        &self,
        over: Option<&ContextOverride>,
        device: &str,
        profile: &str,
    ) -> Result<Channel>;

    /// Everything before ssh runs: checks the arguments (refusing those that
    /// would bypass the channel or the host pinning), generates a throwaway
    /// key and has it certified. Split from [`Self::run_ssh`] so a transport
    /// can report these steps before handing the terminal to ssh.
    async fn prepare_ssh(&self, request: SshRequest) -> Result<PreparedSsh>;

    /// [`Self::prepare_ssh`]'s counterpart for scp: same checks, same
    /// throwaway certified key and pinning, with the remote operands
    /// (`[user@]DEVICE:path`) rewritten to the device's pinned name. All
    /// remote operands must name the same device.
    async fn prepare_scp(&self, request: ScpRequest) -> Result<PreparedSsh>;

    /// Runs a prepared ssh or scp session; returns the client's exit code.
    /// The key material is on disk only while the client runs.
    async fn run_ssh(&self, prepared: PreparedSsh) -> Result<i32>;

    /// Runs a command on a device without a terminal (see
    /// [`ExecRequest`]), with the same pinning as `ssh`. A certified key
    /// is reused for the commands that follow on the same device, as the
    /// same role and context, while its certificate is still fresh: an
    /// operation running several steps certifies once.
    async fn exec(&self, request: ExecRequest, ctx: &OperationContext) -> Result<ExecOutcome>;
}

#[derive(Clone)]
pub struct ChannelService(busybody::Service<Box<dyn ChannelServiceInterface>>);

impl ChannelService {
    pub fn new<T: ChannelServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for ChannelService {
    type Target = busybody::Service<Box<dyn ChannelServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
