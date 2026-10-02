mod service;

pub use service::Command;

use remora_disk::{application::DiskService, model::DiskInfo};
use remora_format::human_size;

pub async fn run(command: Command, service: &DiskService) -> remora_disk::application::Result<()> {
    service::run(command, service).await
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
