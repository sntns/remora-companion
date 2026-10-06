use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt};
use remora_disk::application::DiskService;
use remora_flash::{application::FlashService, model::FlashRequest};
use remora_format::human_size;

use super::error::{Error, Result};

#[derive(Debug, clap::Args)]
pub struct Command {
    /// Source disk image.
    #[arg(long)]
    image: PathBuf,

    /// .bmap file describing the image's mapped block ranges. Defaults to
    /// `<image>.bmap` if that file exists next to the image (same
    /// convention as bmaptool); pass --no-bmap to skip this.
    #[arg(long)]
    bmap: Option<PathBuf>,

    /// Skip .bmap auto-discovery and copy the whole image verbatim (no
    /// sparse skip, no checksum verification), even if a `<image>.bmap`
    /// file exists next to the image.
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

    let bmap = if no_bmap {
        None
    } else {
        bmap.or_else(|| default_bmap_path(&image))
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

/// The sibling `<image>.bmap` file, if one exists — mirroring bmaptool's own
/// convention for locating a bmap next to its image, so callers don't need
/// to pass both paths on the command line.
fn default_bmap_path(image: &Path) -> Option<PathBuf> {
    let mut candidate = image.as_os_str().to_owned();
    candidate.push(".bmap");
    let candidate = PathBuf::from(candidate);
    candidate.is_file().then_some(candidate)
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
    use std::{
        fs,
        sync::atomic::{AtomicU32, Ordering},
    };

    use super::*;

    static FIXTURE_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A throwaway directory under the OS temp dir, removed on drop.
    struct TempDir {
        dir: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let id = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "remora-flash-cli-test-{}-{}",
                std::process::id(),
                id
            ));
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn default_bmap_path_finds_a_sibling_bmap_file() {
        let dir = TempDir::new();
        let image = dir.path("disk.img");
        let bmap = dir.path("disk.img.bmap");
        fs::write(&image, b"image").unwrap();
        fs::write(&bmap, b"<bmap/>").unwrap();

        assert_eq!(default_bmap_path(&image), Some(bmap));
    }

    #[test]
    fn default_bmap_path_is_none_without_a_sibling_bmap_file() {
        let dir = TempDir::new();
        let image = dir.path("disk.img");
        fs::write(&image, b"image").unwrap();

        assert_eq!(default_bmap_path(&image), None);
    }

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
