use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

use error_stack::{Report, ResultExt};
use remora_disk::{application::DiskService, model::DiskInfo};
use remora_flash::{
    adapter::BmapAdapterService,
    application::{Error, FlashServiceInterface, Result},
    model::{FlashOutcome, FlashRequest},
};
use remora_progress::OperationContext;

/// The flash vertical's use case. The actual safety guard (`preflight`) lives
/// here, not in the adapter — so nothing, CLI or a future GUI, can reach the
/// raw copy without going through it first. It asks the disk vertical about
/// the target itself rather than trusting a description of it.
pub struct FlashControllerImpl {
    disk: DiskService,
    bmap: BmapAdapterService,
}

impl FlashControllerImpl {
    pub fn new(disk: DiskService, bmap: BmapAdapterService) -> Self {
        Self { disk, bmap }
    }
}

#[async_trait::async_trait]
impl FlashServiceInterface for FlashControllerImpl {
    async fn preflight(&self, device: &Path, force: bool) -> Result<DiskInfo> {
        let info = self
            .disk
            .info(device)
            .await
            .change_context_lazy(|| Error::Disk(device.to_path_buf()))?;
        if info.is_system_disk {
            return Err(Report::new(Error::UnsafeTarget {
                path: info.path.clone(),
                reason: "this looks like the system disk",
            }));
        }
        if !info.is_removable && !force {
            return Err(Report::new(Error::UnsafeTarget {
                path: info.path.clone(),
                reason: "target is not marked removable; pass --force to override",
            }));
        }
        Ok(info)
    }

    async fn flash(&self, request: &FlashRequest, ctx: &OperationContext) -> Result<FlashOutcome> {
        let info = self.preflight(&request.device, request.force).await?;

        if ctx.cancel.is_cancelled() {
            return Err(Report::new(Error::Cancelled));
        }
        ctx.sink.phase("flashing");

        // The actual copy is one long blocking call into `bmap_parser`
        // (see FlashServiceInterface's doc comment for why it can't report
        // fine-grained progress or be interrupted mid-copy) -- run it on a
        // blocking-pool thread so it doesn't stall the async runtime for
        // however long that takes.
        let bmap = self.bmap.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || run_flash(&bmap, &request, &info))
            .await
            .expect("flash worker panicked")
    }
}

fn run_flash(
    bmap: &BmapAdapterService,
    request: &FlashRequest,
    checked: &DiskInfo,
) -> Result<FlashOutcome> {
    let mut image = File::open(&request.image)
        .change_context_lazy(|| Error::OpenImage(request.image.clone()))?;

    let mut device = open_checked_device(&request.device, checked)?;

    if let Some(bmap_path) = &request.bmap {
        let xml = fs::read_to_string(bmap_path)
            .change_context_lazy(|| Error::OpenBmap(bmap_path.clone()))?;
        let map = bmap.parse(&xml).change_context(Error::Bmap)?;
        let bytes_written = map.total_mapped_size();
        bmap.copy_with_bmap(&mut image, &mut device, &map)
            .change_context(Error::Bmap)?;
        Ok(FlashOutcome {
            bytes_written,
            used_bmap: true,
        })
    } else {
        let bytes_written = image
            .metadata()
            .change_context_lazy(|| Error::OpenImage(request.image.clone()))?
            .len();
        bmap.copy_without_bmap(&mut image, &mut device)
            .change_context(Error::Bmap)?;
        Ok(FlashOutcome {
            bytes_written,
            used_bmap: false,
        })
    }
}

/// Open the disk the guard checked, and only if `device` still resolves to
/// it: the check was made on `checked.path`, the canonical node, so a
/// `device` resolving anywhere else (a symlink retargeted since, a disk
/// service describing another node) is refused rather than written.
fn open_checked_device(device: &Path, checked: &DiskInfo) -> Result<File> {
    let resolved =
        fs::canonicalize(device).change_context_lazy(|| Error::OpenDevice(device.to_path_buf()))?;
    if resolved != checked.path {
        return Err(Report::new(Error::TargetMismatch {
            device: device.to_path_buf(),
            resolved,
            checked: checked.path.clone(),
        }));
    }
    OpenOptions::new()
        .write(true)
        .open(&checked.path)
        .change_context_lazy(|| Error::OpenDevice(checked.path.clone()))
}
