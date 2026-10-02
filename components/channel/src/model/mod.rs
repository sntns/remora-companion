mod ssh;

pub use ssh::{
    PreparedSsh, ProxyCommandBuilder, ScpRequest, SshCertificate, SshCommand, SshRequest, SshRole,
};

/// What a channel profile resolved to on the device's side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    /// A byte stream (a TCP connection at the device): `ssh`.
    Stream,
    /// One message is one datagram. No profile is one yet.
    Datagram,
}

/// What the gateway says once the device has accepted a channel.
#[derive(Debug, Clone)]
pub struct OpenedChannel {
    /// The device's URN, as the platform names it.
    pub device_urn: String,
    pub kind: ChannelKind,
}

/// The profile every ssh session opens.
pub const SSH_PROFILE: &str = "ssh";
