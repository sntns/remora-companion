use std::{path::PathBuf, sync::Arc};

use remora_context::model::ContextOverride;

/// What to be on the device. Each role is its own IAM action
/// (`remora-channel::sign-device-ssh-certificate-<role>`); which local
/// accounts a role may log in as is the device's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SshRole {
    #[default]
    User,
    Admin,
}

impl SshRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Admin => "admin",
        }
    }
}

/// The platform's answer to a certification request.
#[derive(Debug, Clone)]
pub struct SshCertificate {
    /// The user certificate, OpenSSH format, valid for 15 minutes.
    pub certificate: String,
    /// The device account the role logs into by default.
    pub user: String,
    /// The name the device's host certificate carries.
    pub host_key_alias: String,
    /// `@cert-authority *<suffix> <host authority>`.
    pub known_hosts: String,
}

/// What a session's ProxyCommand must open the channel as and to: the
/// context the session resolved to -- whose login and role are the identity
/// the key was just certified for -- and the device (which, for scp, is only
/// known once the operands are read).
pub struct ProxyTarget<'a> {
    pub context: &'a str,
    pub device: &'a str,
}

/// Builds the ProxyCommand that opens the device's ssh channel. Supplied by
/// the transport, which is what knows how this program is invoked.
pub type ProxyCommandBuilder = Arc<dyn Fn(&ProxyTarget) -> String + Send + Sync>;

/// Everything `ssh` needs from the operator.
pub struct SshRequest {
    pub over: Option<ContextOverride>,
    pub device: String,
    pub role: SshRole,
    /// The device account to log into, instead of the role's default. The
    /// certificate still names only the role.
    pub login: Option<String>,
    /// `-o` options, placed after the pinning ones so they can't loosen them.
    pub options: Vec<String>,
    /// ssh's own arguments: leading options, then the remote command.
    pub arguments: Vec<String>,
    /// The ssh client to run.
    pub binary: PathBuf,
    pub proxy_command: ProxyCommandBuilder,
}

/// Everything `scp` needs from the operator. The device is not a field:
/// it is the host of the remote operands, `[user@]DEVICE:path`, exactly as
/// with plain scp.
pub struct ScpRequest {
    pub over: Option<ContextOverride>,
    pub role: SshRole,
    /// The device account, when neither a `user@` operand nor the role's
    /// default should decide it.
    pub login: Option<String>,
    /// `-o` options, placed after the pinning ones so they can't loosen them.
    pub options: Vec<String>,
    /// scp's own arguments: options, then the operands.
    pub arguments: Vec<String>,
    /// The scp client to run.
    pub binary: PathBuf,
    pub proxy_command: ProxyCommandBuilder,
}

/// A program and its arguments.
#[derive(Debug, Clone)]
pub struct SshCommand {
    pub program: PathBuf,
    pub arguments: Vec<String>,
}

/// A session's key material: the throwaway private key, the certificate
/// the platform issued for it, and the device host authority to trust.
/// Values, not files: writing them where the client can read them -- and
/// removing them however the session ends -- is the ssh client port's job.
///
/// No `Debug`: it holds a private key, and error reports print with `{:?}`.
/// The key is wiped from memory when this drops.
pub struct SshKeys {
    private_key: zeroize::Zeroizing<String>,
    /// The user certificate, OpenSSH format.
    pub certificate: String,
    /// `@cert-authority` lines pinning the device's host certificate.
    pub known_hosts: String,
}

impl SshKeys {
    pub fn new(
        private_key: zeroize::Zeroizing<String>,
        certificate: String,
        known_hosts: String,
    ) -> Self {
        Self {
            private_key,
            certificate,
            known_hosts,
        }
    }

    /// The private key, OpenSSH PEM.
    pub fn private_key(&self) -> &str {
        &self.private_key
    }
}

/// An ssh session ready to run: certified, its pinned command line built.
/// The key material travels with it, for the ssh client port to hand over.
pub struct PreparedSsh {
    pub context: String,
    pub device: String,
    pub login: String,
    pub host_key_alias: String,
    pub role: SshRole,
    /// The client and its arguments, without the key material's own
    /// options (identity, certificate, known hosts): the port adds them.
    pub command: SshCommand,
    pub keys: SshKeys,
}
