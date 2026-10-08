use std::path::PathBuf;

use error_stack::ResultExt;
use remora_format::human_size;
use remora_installer::{application::InstallerService, model::PackRequest};
use remora_progress::OperationContext;

use super::error::{Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Make an installer flash another disk image: `installer` (as
    /// meta-remora builds it, e.g. remora-installer-f3apl.wic.bmaptar) with
    /// its payload replaced by `--image`, written to `--output`. The image
    /// is any format `convert` reads (a `.bmaptar` as is, a raw `.wic` just
    /// provisioned with `image`/`identity`/`config`...); the output's
    /// format is the one its extension names. Then flash the output to a
    /// USB stick: booted on a device, it installs the image on its disk.
    Pack {
        /// The installer to start from.
        installer: PathBuf,

        /// The disk image the new installer flashes.
        #[arg(long)]
        image: PathBuf,

        /// The new installer, e.g. my-installer-f3apl.wic.bmaptar.
        #[arg(long)]
        output: PathBuf,
    },
}

pub async fn run(command: Command, service: &InstallerService) -> Result<()> {
    match command {
        Command::Pack {
            installer,
            image,
            output,
        } => {
            let (sink, stream) = remora_progress::channel();
            let follow = remora_tui::follow(stream);
            let result = service
                .pack(
                    &PackRequest {
                        installer,
                        image,
                        output: output.clone(),
                    },
                    &OperationContext::new(sink, remora_progress::cancelled_by_ctrl_c()),
                )
                .await;
            follow.finish(&result).await;
            let outcome = result.change_context(Error::Pack)?;
            remora_tui::success(format!(
                "Wrote {}: a {} payload{} in partition #{} ({})",
                remora_tui::accent(output.display()),
                human_size(outcome.payload_bytes),
                if outcome.bundled {
                    ", bundled as a .bmaptar,"
                } else {
                    ""
                },
                outcome.partition_index,
                human_size(outcome.partition_bytes),
            ));
            Ok(())
        }
    }
}
