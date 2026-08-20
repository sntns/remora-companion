use std::path::Path;

use crate::model::partition_table::{FsKind, PartitionTable};

use super::error::Error;
use super::service;

/// DI seam for `application::image`: swap the real GPT/MBR/signature reader
/// for a test double without touching the partition-selection use cases.
pub trait PartitionTableAdapter {
    fn read(&self, path: &Path) -> Result<PartitionTable, Error>;
    fn detect_fs_kind(&self, image: &Path, partition_offset: u64) -> Result<FsKind, Error>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PartitionTableReader;

impl PartitionTableAdapter for PartitionTableReader {
    fn read(&self, path: &Path) -> Result<PartitionTable, Error> {
        service::read(path)
    }

    fn detect_fs_kind(&self, image: &Path, partition_offset: u64) -> Result<FsKind, Error> {
        service::detect_fs_kind(image, partition_offset)
    }
}
