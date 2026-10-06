use std::path::PathBuf;

use error_stack::ResultExt;
use remora_config::application::ConfigService;
use remora_format::human_size;

use super::error::{Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Build a standalone config.ext4 image from a directory of input
    /// files, matching `remora-config.bbclass`'s `mkfs.ext4 -d CONFIG_DIR`.
    Build {
        /// Directory whose contents become the config image's root.
        input_dir: PathBuf,

        #[arg(short, long)]
        output: PathBuf,

        /// Image size, e.g. `8388608` (bytes). Defaults to 8 MiB, matching
        /// `remora-config.bbclass`.
        #[arg(long, default_value_t = 8 * 1024 * 1024)]
        size: u64,

        #[arg(long, default_value_t = 1024)]
        block_size: u32,

        #[arg(long)]
        label: Option<String>,
    },

    /// Add or update one file inside `Shared:/remora/<slot>/config` of
    /// `image` — building a fresh 8 MiB config.ext4 first if it doesn't
    /// exist yet. Boot mode and filesystem kind are auto-detected.
    Upload {
        /// Local file to upload.
        src: PathBuf,

        /// Destination path inside config.ext4, e.g. `/timezone`.
        dest_path: String,

        #[arg(long)]
        image: PathBuf,

        #[arg(long, default_value = "slot-A")]
        slot: String,

        #[arg(long, default_value_t = 0o644)]
        mode: u16,
    },
}

pub async fn run(command: Command, service: &ConfigService) -> Result<()> {
    match command {
        Command::Build {
            input_dir,
            output,
            size,
            block_size,
            label,
        } => {
            let summary = service
                .build(&input_dir, &output, size, block_size, label.as_deref())
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
        Command::Upload {
            src,
            dest_path,
            image,
            slot,
            mode,
        } => {
            service
                .upload(&image, &src, &dest_path, &slot, mode)
                .await
                .change_context(Error::Upload)?;
            remora_tui::success(format!(
                "Wrote {dest_path} into {}",
                remora_tui::accent(format!("{}:/remora/{slot}/config", image.display()))
            ));
            Ok(())
        }
    }
}
