use std::path::PathBuf;

use remora_disk::application::{DiskService, Result};

use crate::print_disks;

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
        #[arg(add = remora_completion::values(remora_completion::Kind::Disk))]
        device: PathBuf,
    },
}

pub async fn run(command: Command, service: &DiskService) -> Result<()> {
    match command {
        Command::List { all } => {
            let disks: Vec<_> = service
                .list()
                .await?
                .into_iter()
                .filter(|d| all || d.is_removable)
                .collect();
            if disks.is_empty() {
                remora_tui::info(if all {
                    "No disk found".to_string()
                } else {
                    format!(
                        "No removable disk found {}",
                        remora_tui::dim("(--all shows every disk)")
                    )
                });
            } else {
                print_disks(&disks);
            }
            Ok(())
        }
        Command::Info { device } => {
            let info = service.info(&device).await?;
            print_disks(&[info]);
            Ok(())
        }
    }
}
