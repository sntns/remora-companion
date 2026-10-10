use remora_channel::application::ChannelService;
use remora_context::model::ContextOverride;

use crate::{certificate, error::Result, login};

/// On site, at a device's local network: what to log in with when the
/// device is reached directly rather than through the platform.
#[derive(clap::Subcommand)]
pub enum LocalCommand {
    /// Certify your own ssh key to log into devices directly on their
    /// local network, with plain ssh: writes KEY-cert.pub and a
    /// known_hosts file next to the key.
    Certificate(certificate::CertificateArgs),

    /// The code that answers the challenge a device's console login shows.
    LoginCode(login::LoginCodeArgs),

    /// Console login codes a device accepts without a challenge, each
    /// once: for when neither it nor you can reach the platform then.
    OfflineCodes(login::OfflineCodesArgs),
}

/// Runs a `local` subcommand.
pub async fn run_local(
    command: LocalCommand,
    service: &ChannelService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        LocalCommand::Certificate(args) => certificate::run(args, service, over).await,
        LocalCommand::LoginCode(args) => login::run_login_code(args, service, over).await,
        LocalCommand::OfflineCodes(args) => login::run_offline_codes(args, service, over).await,
    }
}
