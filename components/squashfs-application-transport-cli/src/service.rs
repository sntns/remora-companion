use std::path::PathBuf;

use clap::ValueEnum;
use remora_format::human_size;
use remora_squashfs::{
    application::{Result, SquashfsService},
    model::{BuildOptions, Compression},
};

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
    Lzma,
    Lzo,
    Xz,
    Lz4,
    Zstd,
}

impl From<CompressionArg> for Compression {
    fn from(value: CompressionArg) -> Self {
        match value {
            CompressionArg::Gzip => Compression::Gzip,
            CompressionArg::Lzma => Compression::Lzma,
            CompressionArg::Lzo => Compression::Lzo,
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
                tokio_util::sync::CancellationToken::new(),
            );
            let summary = service.build(&inputs, &output, &options, &ctx).await;
            drop(ctx);
            follow.finish(&summary).await;
            let summary = summary?;
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
            let entries = service.inspect(&image).await?;
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
