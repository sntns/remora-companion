use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
};

use crate::{adapter::bmap::BlockMap, model::disk::DiskInfo};

use super::error::Error;

#[derive(Debug, Clone)]
pub struct FlashRequest {
    pub image: PathBuf,
    pub bmap: Option<PathBuf>,
    pub device: PathBuf,
    /// Bypass the removable-disk check (still refuses a disk identified as
    /// the system disk regardless of this flag).
    pub force: bool,
}

#[derive(Debug)]
pub struct FlashOutcome {
    /// Bytes actually written, i.e. the mapped size when a `.bmap` was used;
    /// the full image size otherwise.
    pub bytes_written: u64,
    pub used_bmap: bool,
}

/// The actual safety guard: refuse to overwrite what looks like the system
/// disk no matter what, and refuse a non-removable disk unless `force` is
/// set. This lives here (not in an adapter) so nothing — CLI or a future GUI
/// — can reach the raw copy without going through it first.
pub fn preflight(info: &DiskInfo, force: bool) -> Result<(), Error> {
    if info.is_system_disk {
        return Err(Error::UnsafeTarget {
            path: info.path.clone(),
            reason: "this looks like the system disk",
        });
    }
    if !info.is_removable && !force {
        return Err(Error::UnsafeTarget {
            path: info.path.clone(),
            reason: "target is not marked removable; pass --force to override",
        });
    }
    Ok(())
}

pub fn flash(request: &FlashRequest, info: &DiskInfo) -> Result<FlashOutcome, Error> {
    preflight(info, request.force)?;

    let mut image = File::open(&request.image).map_err(|source| Error::OpenImage {
        path: request.image.clone(),
        source,
    })?;

    let mut device = OpenOptions::new()
        .write(true)
        .open(&request.device)
        .map_err(|source| Error::OpenDevice {
            path: request.device.clone(),
            source,
        })?;

    if let Some(bmap_path) = &request.bmap {
        let xml = fs::read_to_string(bmap_path).map_err(|source| Error::OpenBmap {
            path: bmap_path.clone(),
            source,
        })?;
        let map = BlockMap::parse(&xml)?;
        let bytes_written = map.total_mapped_size();
        crate::adapter::bmap::copy_with_bmap(&mut image, &mut device, &map)?;
        Ok(FlashOutcome {
            bytes_written,
            used_bmap: true,
        })
    } else {
        let bytes_written = image.metadata().map(|m| m.len()).unwrap_or(0);
        crate::adapter::bmap::copy_without_bmap(&mut image, &mut device)?;
        Ok(FlashOutcome {
            bytes_written,
            used_bmap: false,
        })
    }
}
