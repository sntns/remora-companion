use std::path::PathBuf;

use error_stack::ResultExt;
use remora_format::human_size;
use remora_image::{
    application::ImageService,
    model::{
        CpDirRequest, InjectRequest, MkdirRequest, PartitionRole, PartitionSelector,
        PartitionTable, TableKind,
    },
};

use super::error::{Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Show the partition table kind and a summary of an image or device,
    /// each partition annotated with the Remora role (shared/efi/slotA/
    /// slotB/data) its layout gives it.
    Inspect {
        /// Image file or block device path.
        image: PathBuf,
    },

    /// List, inspect, and manipulate partitions.
    #[command(subcommand)]
    Partition(PartitionCommand),
}

#[derive(clap::Subcommand)]
pub enum PartitionCommand {
    /// List the partitions of an image or device.
    List { image: PathBuf },

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
        /// or a Remora role name (shared/efi/slota/slotb/data), found from
        /// the image's own layout.
        #[arg(long)]
        partition: String,

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
        /// or a Remora role name (shared/efi/slota/slotb/data), found from
        /// the image's own layout.
        #[arg(long)]
        partition: String,

        /// Directory mode (permission bits) for the created directory.
        #[arg(long, default_value_t = 0o755)]
        mode: u16,
    },
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
        Command::Inspect { image } => {
            let table = service.inspect(&image).await.change_context(Error::Image)?;
            remora_tui::info(format!(
                "{}: {} table, {} bytes/sector, {} partition(s)",
                remora_tui::accent(image.display()),
                match table.kind {
                    TableKind::Mbr => "MBR",
                    TableKind::Gpt => "GPT",
                },
                table.sector_size,
                table.partitions.len(),
            ));
            print_partitions(&table);
            Ok(())
        }
        Command::Partition(PartitionCommand::List { image }) => {
            let table = service.inspect(&image).await.change_context(Error::Image)?;
            print_partitions(&table);
            Ok(())
        }
        Command::Partition(PartitionCommand::Cp {
            src,
            dest_path,
            image,
            partition,
            mode,
        }) => {
            let selector = parse_partition_selector(&partition)?;
            if src.is_dir() {
                let request = CpDirRequest {
                    image: image.clone(),
                    source_dir: src,
                    dest_path: dest_path.clone(),
                    partition: selector,
                };
                let (sink, stream) = remora_progress::channel();
                let follow = remora_tui::follow(stream);
                let ctx = remora_progress::OperationContext::new(
                    sink,
                    remora_progress::cancelled_by_ctrl_c(),
                );
                let result = service.cp_dir(&request, &ctx).await;
                drop(ctx);
                follow.finish(&result).await;
                result.change_context(Error::Image)?;
                remora_tui::success(format!(
                    "Copied into {dest_path} inside {}",
                    remora_tui::accent(image.display())
                ));
            } else {
                let request = InjectRequest {
                    image: image.clone(),
                    source: src,
                    dest_path: dest_path.clone(),
                    partition: selector,
                    mode,
                };
                service
                    .inject(&request)
                    .await
                    .change_context(Error::Image)?;
                remora_tui::success(format!(
                    "Wrote {dest_path} inside {}",
                    remora_tui::accent(image.display())
                ));
            }
            Ok(())
        }
        Command::Partition(PartitionCommand::Mkdir {
            dest_path,
            image,
            partition,
            mode,
        }) => {
            let selector = parse_partition_selector(&partition)?;
            let request = MkdirRequest {
                image: image.clone(),
                dest_path: dest_path.clone(),
                partition: selector,
                mode,
            };
            service.mkdir(&request).await.change_context(Error::Image)?;
            remora_tui::success(format!(
                "Created {dest_path} inside {}",
                remora_tui::accent(image.display())
            ));
            Ok(())
        }
    }
}

fn print_partitions(table: &PartitionTable) {
    let mut rows = remora_tui::Table::new(["#", "start", "size", "type", "label", "role"]);
    for entry in &table.partitions {
        let role = table
            .role_of(entry.index)
            .map(|role| format!("{role:?}"))
            .unwrap_or_else(|| "-".to_string());
        rows.row(
            [
                entry.index.to_string(),
                entry.start_bytes.to_string(),
                human_size(entry.size_bytes),
                entry.partition_type.to_string(),
                entry.label.clone().unwrap_or_else(|| "-".to_string()),
                role,
            ],
            false,
        );
    }
    rows.print();
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
