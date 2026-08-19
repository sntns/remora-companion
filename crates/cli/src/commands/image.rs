use std::path::PathBuf;

use clap::ValueEnum;
use remora_etcher_core::{
    application::image as image_app,
    model::partition_table::{BootMode, PartitionTable, TableKind},
};

use crate::output::human_size;

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

pub fn run(command: Command) -> Result<(), image_app::Error> {
    match command {
        Command::Inspect { image, boot_mode } => {
            let table = image_app::inspect(&image)?;
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
            let table = image_app::inspect(&image)?;
            print_partitions(&table, boot_mode.map(BootMode::from));
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
