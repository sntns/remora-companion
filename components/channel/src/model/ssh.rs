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
/// context and role the session resolved to -- the same identity whose key
/// was just certified -- and the device (which, for scp, is only known once
/// the operands are read).
pub struct ProxyTarget<'a> {
    pub context: &'a str,
    /// The assumed role's URN, `None` for the login itself.
    pub role: Option<&'a str>,
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

/// An ssh session ready to run: certified, its key material written to a
/// private temporary directory that is removed when this is dropped --
/// however the session ends.
pub struct PreparedSsh {
    pub context: String,
    pub device: String,
    pub login: String,
    pub host_key_alias: String,
    pub role: SshRole,
    pub command: SshCommand,
    workdir: tempfile::TempDir,
}

impl PreparedSsh {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        context: String,
        device: String,
        login: String,
        host_key_alias: String,
        role: SshRole,
        command: SshCommand,
        workdir: tempfile::TempDir,
    ) -> Self {
        Self {
            context,
            device,
            login,
            host_key_alias,
            role,
            command,
            workdir,
        }
    }

    pub fn workdir(&self) -> &std::path::Path {
        self.workdir.path()
    }
}
