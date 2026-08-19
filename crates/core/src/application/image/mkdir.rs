use std::path::PathBuf;

use crate::{
    adapter::{ext4_fsext4, partition_table},
    model::partition_table::{BootMode, PartitionSelector},
};

use super::error::Error;

#[derive(Debug, Clone)]
pub struct MkdirRequest {
    pub image: PathBuf,
    /// Absolute path inside the partition's ext4 filesystem, e.g.
    /// `/play/tplst-app-config`. Its parent directory must already exist —
    /// not recursive, same narrow-surface rule as `inject`.
    pub dest_path: String,
    pub partition: PartitionSelector,
    pub boot_mode: Option<BootMode>,
    /// Directory mode (permission bits only), e.g. `0o755`.
    pub mode: u16,
}

/// Create `request.dest_path` inside whichever partition
/// `request.partition` resolves to.
pub fn mkdir(request: &MkdirRequest) -> Result<(), Error> {
    let table = partition_table::read(&request.image)?;
    let entry = table.select(request.partition, request.boot_mode)?;

    ext4_fsext4::create_dir(
        &request.image,
        entry.start_bytes,
        entry.size_bytes,
        &request.dest_path,
        request.mode,
    )?;

    Ok(())
}
