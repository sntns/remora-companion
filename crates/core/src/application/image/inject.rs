use std::{fs, path::PathBuf};

use crate::{
    adapter::{ext4_fsext4, partition_table},
    model::partition_table::{BootMode, PartitionSelector},
};

use super::error::Error;

#[derive(Debug, Clone)]
pub struct InjectRequest {
    pub image: PathBuf,
    pub source: PathBuf,
    /// Absolute path inside the partition's ext4 filesystem, e.g.
    /// `/play/tplst-app-config/config.json`. Its parent directory must
    /// already exist.
    pub dest_path: String,
    pub partition: PartitionSelector,
    pub boot_mode: Option<BootMode>,
    /// File mode (permission bits only), e.g. `0o644`.
    pub mode: u16,
}

/// Inject `request.source`'s content into `request.dest_path` inside
/// whichever partition `request.partition` resolves to. The parent
/// directory of `dest_path` must already exist — this does not create
/// intermediate directories (narrow surface, see the phase-3 spike).
pub fn inject(request: &InjectRequest) -> Result<(), Error> {
    let table = partition_table::read(&request.image)?;
    let entry = table.select(request.partition, request.boot_mode)?;

    let contents = fs::read(&request.source).map_err(|source| Error::ReadSource {
        path: request.source.clone(),
        source,
    })?;

    ext4_fsext4::write_file(
        &request.image,
        entry.start_bytes,
        entry.size_bytes,
        &request.dest_path,
        &contents,
        request.mode,
    )?;

    Ok(())
}
