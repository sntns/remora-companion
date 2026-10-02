use remora_context::model::ResolvedContext;

use super::error::Result;
use crate::model::{OpenedChannel, SshCertificate, SshRole};

/// The sending half of an open channel.
#[async_trait::async_trait]
pub trait ChannelSender: Send {
    /// Sends bytes to the device. Waits while the transport's flow-control
    /// window is full, so a slow device slows this end down.
    async fn send(&mut self, chunk: Vec<u8>) -> Result<()>;
    /// Ends this direction only (shutdown(SHUT_WR)): the device reads its
    /// end of input and may still answer.
    async fn close_write(&mut self) -> Result<()>;
}

/// The receiving half of an open channel.
#[async_trait::async_trait]
pub trait ChannelReceiver: Send {
    /// The device's next bytes, or `None` once it has said it will send no
    /// more (or the channel ended). Sticky: `None` stays `None`.
    async fn recv(&mut self) -> Result<Option<Vec<u8>>>;
}

/// An open channel to a device, split into its two directions so each can
/// be driven by its own task.
pub struct Channel {
    pub opened: OpenedChannel,
    pub sender: Box<dyn ChannelSender>,
    pub receiver: Box<dyn ChannelReceiver>,
}

/// sntns-platform's remora-channel gateway, as seen by an operator.
#[async_trait::async_trait]
pub trait ChannelGatewayAdapter: Send + Sync {
    /// Opens `profile` on `device` and returns once the device accepted.
    async fn open(&self, context: &ResolvedContext, device: &str, profile: &str)
        -> Result<Channel>;

    /// Has the platform certify `public_key` (authorized-keys format) to
    /// log into `device` as `role`.
    async fn sign_ssh_certificate(
        &self,
        context: &ResolvedContext,
        device: &str,
        public_key: &str,
        role: SshRole,
    ) -> Result<SshCertificate>;
}

#[derive(Clone)]
pub struct ChannelGatewayAdapterService(busybody::Service<Box<dyn ChannelGatewayAdapter>>);

impl ChannelGatewayAdapterService {
    pub fn new<T: ChannelGatewayAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for ChannelGatewayAdapterService {
    type Target = busybody::Service<Box<dyn ChannelGatewayAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
