use std::{
    io::{self, Write},
    path::PathBuf,
};

use error_stack::ResultExt;
use remora_etcher_disk::application::DiskService;
use remora_etcher_flash::{application::FlashService, model::FlashRequest};
use remora_etcher_format::human_size;

use super::error::{Error, Result};

#[derive(clap::Args)]
pub struct Command {
    /// Source disk image.
    #[arg(long)]
    image: PathBuf,

    /// .bmap file describing the image's mapped block ranges. Skipping this
    /// copies the whole image verbatim (no sparse skip, no checksum
    /// verification).
    #[arg(long)]
    bmap: Option<PathBuf>,

    /// Destination device path, e.g. /dev/sdb. Its *whole disk* is
    /// overwritten — not a single partition.
    #[arg(long)]
    device: PathBuf,

    /// Allow flashing a disk that isn't marked removable. Never overrides
    /// the system-disk refusal.
    #[arg(long)]
    force: bool,

    /// Skip the interactive confirmation prompt (for scripted use).
    #[arg(long)]
    yes: bool,
}

pub fn run(command: Command, disk: &DiskService, flash: &FlashService) -> Result<()> {
    let Command {
        image,
        bmap,
        device,
        force,
        yes,
    } = command;

    let info = disk.info(&device).change_context(Error::Disk)?;
    println!("target: {}", disk_line(&info));

    // The real guard (removable / not-the-system-disk) lives in
    // FlashServiceInterface::preflight and cannot be bypassed from here;
    // this confirmation is an extra CLI-only usability layer on top of it.
    flash.preflight(&info, force).change_context(Error::Flash)?;
    if !yes && !confirm(&device).change_context(Error::Confirm)? {
        println!("aborted: device path did not match");
        return Ok(());
    }

    let request = FlashRequest {
        image,
        bmap,
        device,
        force,
    };
    let outcome = flash.flash(&request, &info).change_context(Error::Flash)?;
    println!(
        "wrote {} to {} ({})",
        human_size(outcome.bytes_written),
        info.path.display(),
        if outcome.used_bmap {
            "bmap-verified"
        } else {
            "full copy, no bmap"
        }
    );
    Ok(())
}

fn disk_line(info: &remora_etcher_disk::model::DiskInfo) -> String {
    format!(
        "{:<12} {:>10}  removable={:<5} system={:<5} {}",
        info.path.display().to_string(),
        human_size(info.size_bytes),
        info.is_removable,
        info.is_system_disk,
        info.model.as_deref().unwrap_or("-"),
    )
}

/// Require the user to type the device path back, so a `--force`d flash of a
/// non-removable disk (or any flash at all) isn't one careless Enter away
/// from wiping the wrong disk.
fn confirm(device: &std::path::Path) -> io::Result<bool> {
    print!(
        "Type the device path ({}) to confirm overwriting it: ",
        device.display()
    );
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim() == device.to_string_lossy())
}
