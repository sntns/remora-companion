use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt};
use remora_disk::application::DiskService;
use remora_flash::{
    application::FlashService,
    model::{BmapSource, FlashRequest},
};
use remora_format::human_size;

use super::error::{Error, Result};

#[derive(Debug, clap::Args)]
pub struct Command {
    /// Source disk image: a `.bmaptar` bundle (the image and its .bmap in
    /// one tar, as meta-remora builds them), or a plain image, raw or
    /// compressed (bzip2, gzip).
    #[arg(long)]
    image: PathBuf,

    /// .bmap file describing the image's mapped block ranges. Defaults to
    /// the one a `.bmaptar` bundle carries; for a plain image, to
    /// `<image>.bmap`, else to the image's name without its compression
    /// extension plus `.bmap` (`x.wic.bz2` -> `x.wic.bmap`), whichever
    /// exists (same convention as bmaptool); pass --no-bmap to skip this.
    #[arg(long)]
    bmap: Option<PathBuf>,

    /// Copy the whole image verbatim (no sparse skip, no checksum
    /// verification), ignoring any .bmap, bundled or next to the image.
    #[arg(long, conflicts_with = "bmap")]
    no_bmap: bool,

    /// Destination device path, e.g. /dev/sdb. Its *whole disk* is
    /// overwritten — not a single partition.
    #[arg(long)]
    #[arg(add = remora_completion::values(remora_completion::Kind::Disk))]
    device: PathBuf,

    /// Allow flashing a disk that isn't marked removable. Never overrides
    /// the system-disk refusal.
    #[arg(long)]
    force: bool,

    /// Skip the interactive confirmation prompt (for scripted use; required
    /// when no one is at the terminal to answer it).
    #[arg(long)]
    yes: bool,
}

pub async fn run(command: Command, disk: &DiskService, flash: &FlashService) -> Result<()> {
    let Command {
        image,
        bmap,
        no_bmap,
        device,
        force,
        yes,
    } = command;

    let bmap = match bmap {
        Some(path) => BmapSource::File(path),
        None if no_bmap => BmapSource::None,
        None => BmapSource::Auto,
    };

    // Shown before the guard runs, so a refused target is still visible --
    // as the disk it resolves to, not the name it was given.
    let info = disk.info(&device).await.change_context(Error::Disk)?;
    remora_tui::note("Target", disk_line(&info));

    // The real guard (removable / not-the-system-disk) lives in
    // FlashServiceInterface::preflight, which `flash` runs again on its own
    // lookup and cannot be bypassed from here; this confirmation is an
    // extra CLI-only usability layer on top of it.
    flash
        .preflight(&device, force)
        .await
        .change_context(Error::Flash)?;
    if !yes {
        if !remora_tui::interactive() {
            return Err(Report::new(Error::Unattended));
        }
        if !confirm(&device)? {
            remora_tui::info(format!(
                "Left {} untouched",
                remora_tui::accent(device.display())
            ));
            return Ok(());
        }
    }

    let request = FlashRequest {
        image,
        bmap,
        device,
        force,
    };
    let (sink, stream) = remora_progress::channel();
    let follow = remora_tui::follow(stream);
    let ctx = remora_progress::OperationContext::new(sink, remora_progress::cancelled_by_ctrl_c());
    let outcome = flash.flash(&request, &ctx).await;
    drop(ctx);
    follow.finish(&outcome).await;
    let outcome = outcome.change_context(Error::Flash)?;
    remora_tui::success(format!(
        "Wrote {} to {} {}",
        human_size(outcome.bytes_written),
        remora_tui::accent(info.path.display()),
        remora_tui::dim(if outcome.used_bmap {
            "(bmap-verified)"
        } else {
            "(full copy, no bmap)"
        })
    ));
    Ok(())
}

fn disk_line(info: &remora_disk::model::DiskInfo) -> String {
    let yes_no = |on: bool| if on { "yes" } else { "no" };
    format!(
        "{}  {}\nsize: {}  removable: {}  system: {}",
        remora_tui::accent(info.path.display()),
        info.model.as_deref().unwrap_or("-"),
        human_size(info.size_bytes),
        yes_no(info.is_removable),
        yes_no(info.is_system_disk),
    )
}

/// Require the user to type the device path back, so a `--force`d flash of a
/// non-removable disk (or any flash at all) isn't one careless Enter away
/// from wiping the wrong disk. Anything else typed (or Esc) leaves it alone.
fn confirm(device: &Path) -> Result<bool> {
    let expected = device.to_string_lossy().into_owned();
    let typed: String = remora_tui::input(format!(
        "Type {} to overwrite it",
        remora_tui::accent(&expected)
    ))
    .interact()
    .or_else(|e| {
        if e.kind() == std::io::ErrorKind::Interrupted {
            Ok(String::new())
        } else {
            Err(e)
        }
    })
    .change_context(Error::Confirm)?;
    Ok(typed.trim() == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_bmap_conflicts_with_bmap_on_the_command_line() {
        use clap::Parser;

        #[derive(Debug, clap::Parser)]
        struct TestCli {
            #[command(flatten)]
            command: Command,
        }

        let err = TestCli::try_parse_from([
            "remora-etcher",
            "--image",
            "disk.img",
            "--device",
            "/dev/sdx",
            "--bmap",
            "disk.img.bmap",
            "--no-bmap",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
