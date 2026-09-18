use std::path::PathBuf;

use remora_etcher_convert::application::{ConvertService, Result};

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

pub fn run(command: Command, service: &ConvertService) -> Result<()> {
    match command {
        Command::ToRaw { image, output } => {
            service.to_raw(&image, &output)?;
            println!("wrote {}", output.display());
            Ok(())
        }
        Command::FromRaw { raw, output } => {
            service.from_raw(&raw, &output)?;
            println!("wrote {}", output.display());
            Ok(())
        }
    }
}
