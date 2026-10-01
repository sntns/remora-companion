use std::path::PathBuf;

use remora_convert::application::{ConvertService, Result};
use remora_progress::OperationContext;
use tokio_util::sync::CancellationToken;

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
            let printer = remora_progress::print_to_stderr(stream);
            let result = service
                .to_raw(
                    &image,
                    &output,
                    &OperationContext::new(sink, CancellationToken::new()),
                )
                .await;
            let _ = printer.await;
            result?;
            println!("wrote {}", output.display());
            Ok(())
        }
        Command::FromRaw { raw, output } => {
            let (sink, stream) = remora_progress::channel();
            let printer = remora_progress::print_to_stderr(stream);
            let result = service
                .from_raw(
                    &raw,
                    &output,
                    &OperationContext::new(sink, CancellationToken::new()),
                )
                .await;
            let _ = printer.await;
            result?;
            println!("wrote {}", output.display());
            Ok(())
        }
    }
}
