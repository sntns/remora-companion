use std::fs::{self, File, OpenOptions};

use error_stack::{Report, ResultExt};
use remora_etcher_disk::model::DiskInfo;
use remora_etcher_flash::{
    adapter::BmapAdapterService,
    application::{Error, FlashServiceInterface, Result},
    model::{FlashOutcome, FlashRequest},
};

/// The flash vertical's use case. The actual safety guard (`preflight`) lives
/// here, not in the adapter — so nothing, CLI or a future GUI, can reach the
/// raw copy without going through it first.
pub struct FlashControllerImpl {
    bmap: BmapAdapterService,
}

impl FlashControllerImpl {
    pub fn new(bmap: BmapAdapterService) -> Self {
        Self { bmap }
    }
}

impl FlashServiceInterface for FlashControllerImpl {
    fn preflight(&self, info: &DiskInfo, force: bool) -> Result<()> {
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
        Ok(())
    }

    fn flash(&self, request: &FlashRequest, info: &DiskInfo) -> Result<FlashOutcome> {
        self.preflight(info, request.force)?;

        let mut image = File::open(&request.image)
            .change_context_lazy(|| Error::OpenImage(request.image.clone()))?;

        let mut device = OpenOptions::new()
            .write(true)
            .open(&request.device)
            .change_context_lazy(|| Error::OpenDevice(request.device.clone()))?;

        if let Some(bmap_path) = &request.bmap {
            let xml = fs::read_to_string(bmap_path)
                .change_context_lazy(|| Error::OpenBmap(bmap_path.clone()))?;
            let map = self.bmap.parse(&xml).change_context(Error::Bmap)?;
            let bytes_written = map.total_mapped_size();
            self.bmap
                .copy_with_bmap(&mut image, &mut device, &map)
                .change_context(Error::Bmap)?;
            Ok(FlashOutcome {
                bytes_written,
                used_bmap: true,
            })
        } else {
            let bytes_written = image.metadata().map(|m| m.len()).unwrap_or(0);
            self.bmap
                .copy_without_bmap(&mut image, &mut device)
                .change_context(Error::Bmap)?;
            Ok(FlashOutcome {
                bytes_written,
                used_bmap: false,
            })
        }
    }
}
