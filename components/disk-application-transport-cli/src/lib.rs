mod service;

pub use service::Command;

use remora_disk::{application::DiskService, model::DiskInfo};
use remora_format::human_size;

pub async fn run(command: Command, service: &DiskService) -> remora_disk::application::Result<()> {
    service::run(command, service).await
}

pub(crate) fn format_disk_line(info: &DiskInfo) -> String {
    format!(
        "{:<12} {:>10}  removable={:<5} system={:<5} {}",
        info.path.display().to_string(),
        human_size(info.size_bytes),
        info.is_removable,
        info.is_system_disk,
        info.model.as_deref().unwrap_or("-"),
    )
}
