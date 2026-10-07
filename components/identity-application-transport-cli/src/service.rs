use std::path::PathBuf;

use error_stack::ResultExt;
use remora_format::human_size;
use remora_identity::application::IdentityService;

use super::error::{Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Build identity.squashfs from arbitrary input files/directories plus
    /// hostname/machine-id/an ed25519 SSH host keypair (generated if not
    /// given, or not already present among the inputs).
    Build {
        /// Input files and/or directories to bundle into the identity
        /// squashfs, same semantics as `squashfs build`.
        inputs: Vec<PathBuf>,

        #[arg(short, long)]
        output: PathBuf,

        #[arg(long)]
        hostname: Option<String>,

        #[arg(long)]
        machine_id: Option<String>,
    },

    /// Build identity.squashfs (like `identity build`) and inject it into
    /// `Shared:/remora/identity` of `image` in one shot. The shared
    /// partition and its filesystem kind (vfat/ext4) are detected from the
    /// image itself.
    Create {
        inputs: Vec<PathBuf>,

        #[arg(long)]
        image: PathBuf,

        #[arg(long)]
        hostname: Option<String>,

        #[arg(long)]
        machine_id: Option<String>,
    },
}

pub async fn run(command: Command, service: &IdentityService) -> Result<()> {
    match command {
        Command::Build {
            inputs,
            output,
            hostname,
            machine_id,
        } => {
            let summary = service
                .build(&inputs, hostname.as_deref(), machine_id.as_deref(), &output)
                .await
                .change_context(Error::Build)?;
            remora_tui::success(format!(
                "Wrote {} {}",
                remora_tui::accent(output.display()),
                remora_tui::dim(format!(
                    "({}, {} entries)",
                    human_size(summary.bytes_written),
                    summary.entry_count
                ))
            ));
            Ok(())
        }
        Command::Create {
            inputs,
            image,
            hostname,
            machine_id,
        } => {
            let bytes_written = service
                .create(&inputs, hostname.as_deref(), machine_id.as_deref(), &image)
                .await
                .change_context(Error::Create)?;
            remora_tui::success(format!(
                "Wrote identity.squashfs to {} {}",
                remora_tui::accent(format!("{}:/remora/identity", image.display())),
                remora_tui::dim(format!("({})", human_size(bytes_written)))
            ));
            Ok(())
        }
    }
}
