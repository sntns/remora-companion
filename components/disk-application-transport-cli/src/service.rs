use std::path::PathBuf;

use error_stack::ResultExt;
use remora_disk::{application::DiskService, model::DiskInfo};
use remora_format::human_size;

use super::error::{Error, Result};

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
                .await
                .change_context(Error::List)?
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
            let info = service.info(&device).await.change_context(Error::Info)?;
            print_disks(&[info]);
            Ok(())
        }
    }
}

/// Disks as a table on stdout; a system disk is drawn as a warning, since
/// it's the one never to flash.
pub fn print_disks(disks: &[DiskInfo]) {
    use remora_tui::Tone;
    let mut table = remora_tui::Table::new(["device", "size", "removable", "system", "model"]);
    for info in disks {
        let flag = |on: bool, tone: Tone| {
            (
                if on { "yes" } else { "no" }.to_string(),
                on.then_some(tone),
            )
        };
        table.row_toned([
            (info.path.display().to_string(), None),
            (human_size(info.size_bytes), None),
            flag(info.is_removable, Tone::Good),
            flag(info.is_system_disk, Tone::Warn),
            (info.model.clone().unwrap_or_else(|| "-".to_string()), None),
        ]);
    }
    table.print();
}
