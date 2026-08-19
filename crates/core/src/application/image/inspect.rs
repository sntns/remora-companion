use std::path::Path;

use crate::{adapter::partition_table, model::partition_table::PartitionTable};

use super::error::Error;

/// Read the partition table of an image file or block device. Table kind
/// (MBR vs. GPT) is auto-detected — see `adapter::partition_table`.
pub fn inspect(path: &Path) -> Result<PartitionTable, Error> {
    Ok(partition_table::read(path)?)
}
