use std::path::PathBuf;

use clap::ValueEnum;
use error_stack::ResultExt;
use remora_format::human_size;
use remora_squashfs::{
    application::SquashfsService,
    model::{BuildOptions, Compression},
};

use super::error::{Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Build a squashfs image from a list of files/directories.
    Build {
        /// Input files and/or directories. Directory contents are merged at
        /// the squashfs root; a file is placed at the root under its own name.
        inputs: Vec<PathBuf>,

        /// Output squashfs image path.
        #[arg(short, long)]
        output: PathBuf,

        /// Compression algorithm. Defaults to gzip, matching mksquashfs.
        #[arg(long, value_enum, default_value_t = CompressionArg::Gzip)]
        comp: CompressionArg,

        /// Block size in bytes (power of two, 4096..=1048576). Defaults to
        /// 128 KiB, matching mksquashfs.
        #[arg(long, default_value_t = remora_squashfs::model::DEFAULT_BLOCK_SIZE)]
        block_size: u32,

        /// Pin every entry's mtime and the image's own mtime to this value
        /// instead of each entry's real mtime.
        #[arg(long)]
        source_date_epoch: Option<u32>,

        /// Mode of the synthetic squashfs root directory.
        #[arg(long, default_value_t = 0o755)]
        root_mode: u16,
    },

    /// List the entries of an existing squashfs image.
    Inspect {
        /// Squashfs image to inspect.
        image: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum CompressionArg {
    Gzip,
    Xz,
    Lz4,
    Zstd,
}

impl From<CompressionArg> for Compression {
    fn from(value: CompressionArg) -> Self {
        match value {
            CompressionArg::Gzip => Compression::Gzip,
            CompressionArg::Xz => Compression::Xz,
            CompressionArg::Lz4 => Compression::Lz4,
            CompressionArg::Zstd => Compression::Zstd,
        }
    }
}

pub async fn run(command: Command, service: &SquashfsService) -> Result<()> {
    match command {
        Command::Build {
            inputs,
            output,
            comp,
            block_size,
            source_date_epoch,
            root_mode,
        } => {
            let options = BuildOptions {
                compression: comp.into(),
                block_size,
                source_date_epoch,
                root_mode,
            };
            let (sink, stream) = remora_progress::channel();
            let follow = remora_tui::follow(stream);
            let ctx = remora_progress::OperationContext::new(
                sink,
                remora_progress::cancelled_by_ctrl_c(),
            );
            let summary = service.build(&inputs, &output, &options, &ctx).await;
            drop(ctx);
            follow.finish(&summary).await;
            let summary = summary.change_context(Error::Build)?;
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
        Command::Inspect { image } => {
            let entries = service
                .inspect(&image)
                .await
                .change_context(Error::Inspect)?;
            for entry in entries {
                println!(
                    "{:<5} {:04o} {:>6}:{:<6} {}",
                    entry.kind,
                    entry.permissions,
                    entry.uid,
                    entry.gid,
                    entry.path.display()
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(clap::Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: Command,
    }

    fn parse(comp: &str) -> std::result::Result<TestCli, clap::Error> {
        TestCli::try_parse_from(["squashfs", "build", "in", "-o", "out", "--comp", comp])
    }

    #[test]
    fn offers_only_compressions_backhand_can_write() {
        for comp in ["gzip", "xz", "lz4", "zstd"] {
            parse(comp).unwrap_or_else(|e| panic!("{comp}: {e}"));
        }
        for comp in ["lzma", "lzo"] {
            let err = parse(comp)
                .err()
                .unwrap_or_else(|| panic!("{comp} accepted"));
            assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
        }
    }
}
