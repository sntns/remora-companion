//! The values remora-etcher's dynamic completion offers (see
//! remora-completion): local disks, for the arguments naming a flash
//! target. Paths complete on their own.
//!
//! Runs inside the shell's Tab: it must be fast and never fail loudly.

use remora_completion::{Candidate, Kind};
use remora_format::human_size;

use crate::bootstrap;

pub fn provide(kind: Kind) -> Vec<Candidate> {
    if kind != Kind::Disk {
        return Vec::new();
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Vec::new();
    };
    runtime.block_on(async {
        let services = bootstrap::wire().await;
        let Ok(disks) = services.disk.list().await else {
            return Vec::new();
        };
        // Never the system disk (flash refuses it anyway), nor loop devices
        // (snaps, mostly: noise); removable disks first, they're what gets
        // flashed.
        let (removable, fixed): (Vec<_>, Vec<_>) = disks
            .into_iter()
            .filter(|disk| {
                !disk.is_system_disk && !disk.path.to_string_lossy().starts_with("/dev/loop")
            })
            .partition(|disk| disk.is_removable);
        removable
            .into_iter()
            .chain(fixed)
            .map(|disk| {
                let mut help = human_size(disk.size_bytes);
                if let Some(model) = &disk.model {
                    help.push_str(&format!(" {model}"));
                }
                if !disk.is_removable {
                    help.push_str(" (not removable)");
                }
                Candidate::new(disk.path.display().to_string()).help(help)
            })
            .collect()
    })
}
