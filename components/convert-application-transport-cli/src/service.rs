use std::path::PathBuf;

use error_stack::ResultExt;
use remora_convert::application::ConvertService;
use remora_progress::OperationContext;

use super::error::{Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Decode `image` (its container format detected from its own
    /// extension: `.qcow2`, `.gz`, or a plain raw copy for anything else)
    /// into a plain raw disk image at `--output`.
    ToRaw {
        image: PathBuf,

        #[arg(long)]
        output: PathBuf,
    },

    /// Encode `raw` (a plain raw disk image) into `--output`, whose own
    /// extension names the destination container format (`.qcow2`, `.gz`,
    /// or a plain raw copy for anything else).
    FromRaw {
        raw: PathBuf,

        #[arg(long)]
        output: PathBuf,
    },
}

pub async fn run(command: Command, service: &ConvertService) -> Result<()> {
    match command {
        Command::ToRaw { image, output } => {
            let (sink, stream) = remora_progress::channel();
            let follow = remora_tui::follow(stream);
            let result = service
                .to_raw(
                    &image,
                    &output,
                    &OperationContext::new(sink, remora_progress::cancelled_by_ctrl_c()),
                )
                .await;
            follow.finish(&result).await;
            result.change_context(Error::ToRaw)?;
            remora_tui::success(format!("Wrote {}", remora_tui::accent(output.display())));
            Ok(())
        }
        Command::FromRaw { raw, output } => {
            let (sink, stream) = remora_progress::channel();
            let follow = remora_tui::follow(stream);
            let result = service
                .from_raw(
                    &raw,
                    &output,
                    &OperationContext::new(sink, remora_progress::cancelled_by_ctrl_c()),
                )
                .await;
            follow.finish(&result).await;
            result.change_context(Error::FromRaw)?;
            remora_tui::success(format!("Wrote {}", remora_tui::accent(output.display())));
            Ok(())
        }
    }
}
