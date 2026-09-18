use std::path::PathBuf;

use remora_etcher_disk::application::{DiskService, Result};

use crate::format_disk_line;

#[derive(clap::Subcommand)]
pub enum Command {
    /// List candidate disks.
    List {
        /// Show every disk, not just removable ones.
        #[arg(long)]
        all: bool,
    },

    /// Show what we know about one disk.
    Info {
        /// Device path, e.g. /dev/sdb.
        device: PathBuf,
    },
}

pub async fn run(command: Command, service: &DiskService) -> Result<()> {
    match command {
        Command::List { all } => {
            let disks = service.list().await?;
            for info in disks.into_iter().filter(|d| all || d.is_removable) {
                println!("{}", format_disk_line(&info));
            }
            Ok(())
        }
        Command::Info { device } => {
            let info = service.info(&device).await?;
            println!("{}", format_disk_line(&info));
            Ok(())
        }
    }
}
