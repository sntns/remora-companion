use std::path::PathBuf;

use clap::ValueEnum;
use error_stack::ResultExt;
use remora_format::human_size;
use remora_image::{
    application::ImageService,
    model::{
        BootMode, CpDirRequest, InjectRequest, MkdirRequest, PartitionRole, PartitionSelector,
        PartitionTable, TableKind,
    },
};

use super::error::{Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Show the partition table kind and a summary of an image or device.
    Inspect {
        /// Image file or block device path.
        image: PathBuf,

        /// Annotate each partition with its Remora role (shared/efi/slotA/
        /// slotB/data) for this boot mode.
        #[arg(long, value_enum)]
        boot_mode: Option<BootModeArg>,
    },

    /// List, inspect, and manipulate partitions.
    #[command(subcommand)]
    Partition(PartitionCommand),
}

#[derive(clap::Subcommand)]
pub enum PartitionCommand {
    /// List the partitions of an image or device.
    List {
        image: PathBuf,

        #[arg(long, value_enum)]
        boot_mode: Option<BootModeArg>,
    },

    /// Copy a local file or directory into one partition's filesystem. For
    /// a file, `dest_path` is its destination path and its parent directory
    /// must already exist (this does not create intermediate directories).
    /// For a directory, everything inside is copied recursively, preserving
    /// relative paths and each entry's host file mode (`--mode` is ignored
    /// in that case); `dest_path` itself must already exist. Symlinks
    /// inside a source directory are rejected.
    Cp {
        /// Local file or directory to copy in.
        src: PathBuf,

        /// Destination path (file) or destination directory (directory)
        /// inside the partition's filesystem, e.g.
        /// `/play/tplst-app-config/config.json` or `/play/tplst-app-config`.
        dest_path: String,

        /// Image file or block device to modify.
        #[arg(long)]
        image: PathBuf,

        /// Which partition: a raw index (as printed by `partition list`),
        /// or a Remora role name (shared/efi/slota/slotb/data — requires
        /// --boot-mode).
        #[arg(long)]
        partition: String,

        #[arg(long, value_enum)]
        boot_mode: Option<BootModeArg>,

        /// File mode (permission bits) for the created file. Ignored when
        /// `src` is a directory (each entry keeps its own host mode).
        #[arg(long, default_value_t = 0o644)]
        mode: u16,
    },

    /// Create a directory inside one partition's filesystem (the parent of
    /// `dest_path` must already exist — not recursive).
    Mkdir {
        /// Destination path inside the partition's filesystem, e.g.
        /// `/play/tplst-app-config`.
        dest_path: String,

        /// Image file or block device to modify.
        #[arg(long)]
        image: PathBuf,

        /// Which partition: a raw index (as printed by `partition list`),
        /// or a Remora role name (shared/efi/slota/slotb/data — requires
        /// --boot-mode).
        #[arg(long)]
        partition: String,

        #[arg(long, value_enum)]
        boot_mode: Option<BootModeArg>,

        /// Directory mode (permission bits) for the created directory.
        #[arg(long, default_value_t = 0o755)]
        mode: u16,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum BootModeArg {
    Efi,
    Bios,
    Uboot,
    Rpi,
}

impl From<BootModeArg> for BootMode {
    fn from(value: BootModeArg) -> Self {
        match value {
            BootModeArg::Efi => BootMode::Efi,
            BootModeArg::Bios => BootMode::Bios,
            BootModeArg::Uboot => BootMode::Uboot,
            BootModeArg::Rpi => BootMode::Rpi,
        }
    }
}

fn parse_partition_selector(s: &str) -> Result<PartitionSelector> {
    if let Ok(index) = s.parse::<u32>() {
        return Ok(PartitionSelector::Index(index));
    }
    let role = match s.to_ascii_lowercase().as_str() {
        "shared" => PartitionRole::Shared,
        "efi" => PartitionRole::Efi,
        "slota" => PartitionRole::SlotA,
        "slotb" => PartitionRole::SlotB,
        "data" => PartitionRole::Data,
        _ => {
            return Err(error_stack::Report::new(Error::ParseSelector(
                s.to_string(),
            )))
        }
    };
    Ok(PartitionSelector::Role(role))
}

pub async fn run(command: Command, service: &ImageService) -> Result<()> {
    match command {
        Command::Inspect { image, boot_mode } => {
            let table = service.inspect(&image).await.change_context(Error::Image)?;
            println!(
                "{}: {} table, {} bytes/sector, {} partition(s)",
                image.display(),
                match table.kind {
                    TableKind::Mbr => "MBR",
                    TableKind::Gpt => "GPT",
                },
                table.sector_size,
                table.partitions.len(),
            );
            print_partitions(&table, boot_mode.map(BootMode::from));
            Ok(())
        }
        Command::Partition(PartitionCommand::List { image, boot_mode }) => {
            let table = service.inspect(&image).await.change_context(Error::Image)?;
            print_partitions(&table, boot_mode.map(BootMode::from));
            Ok(())
        }
        Command::Partition(PartitionCommand::Cp {
            src,
            dest_path,
            image,
            partition,
            boot_mode,
            mode,
        }) => {
            let selector = parse_partition_selector(&partition)?;
            if src.is_dir() {
                let request = CpDirRequest {
                    image: image.clone(),
                    source_dir: src,
                    dest_path: dest_path.clone(),
                    partition: selector,
                    boot_mode: boot_mode.map(BootMode::from),
                };
                let (sink, stream) = remora_progress::channel();
                let printer = remora_progress::print_to_stderr(stream);
                let ctx = remora_progress::OperationContext::new(
                    sink,
                    tokio_util::sync::CancellationToken::new(),
                );
                let result = service.cp_dir(&request, &ctx).await;
                drop(ctx);
                let _ = printer.await;
                result.change_context(Error::Image)?;
                println!("copied into {dest_path} inside {}", image.display());
            } else {
                let request = InjectRequest {
                    image: image.clone(),
                    source: src,
                    dest_path: dest_path.clone(),
                    partition: selector,
                    boot_mode: boot_mode.map(BootMode::from),
                    mode,
                };
                service
                    .inject(&request)
                    .await
                    .change_context(Error::Image)?;
                println!("wrote {dest_path} inside {}", image.display());
            }
            Ok(())
        }
        Command::Partition(PartitionCommand::Mkdir {
            dest_path,
            image,
            partition,
            boot_mode,
            mode,
        }) => {
            let selector = parse_partition_selector(&partition)?;
            let request = MkdirRequest {
                image: image.clone(),
                dest_path: dest_path.clone(),
                partition: selector,
                boot_mode: boot_mode.map(BootMode::from),
                mode,
            };
            service.mkdir(&request).await.change_context(Error::Image)?;
            println!("created {dest_path} inside {}", image.display());
            Ok(())
        }
    }
}

fn print_partitions(table: &PartitionTable, boot_mode: Option<BootMode>) {
    for entry in &table.partitions {
        let role = boot_mode
            .and_then(|mode| mode.role_of(entry.index))
            .map(|role| format!("{role:?}"))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:>3}  {:>12}  {:>10}  {:<8}  {:<36}  {}",
            entry.index,
            entry.start_bytes,
            human_size(entry.size_bytes),
            entry.partition_type,
            entry.label.as_deref().unwrap_or("-"),
            role,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bare_number_as_an_index() {
        assert_eq!(
            parse_partition_selector("2").unwrap(),
            PartitionSelector::Index(2)
        );
    }

    #[test]
    fn parses_every_known_role_case_insensitively() {
        let cases = [
            ("shared", PartitionRole::Shared),
            ("SHARED", PartitionRole::Shared),
            ("efi", PartitionRole::Efi),
            ("slota", PartitionRole::SlotA),
            ("SlotA", PartitionRole::SlotA),
            ("slotb", PartitionRole::SlotB),
            ("data", PartitionRole::Data),
        ];
        for (input, role) in cases {
            assert_eq!(
                parse_partition_selector(input).unwrap(),
                PartitionSelector::Role(role),
                "input: {input}"
            );
        }
    }

    #[test]
    fn rejects_anything_else() {
        assert!(parse_partition_selector("slot-a").is_err());
        assert!(parse_partition_selector("").is_err());
        assert!(parse_partition_selector("-1").is_err());
    }
}
