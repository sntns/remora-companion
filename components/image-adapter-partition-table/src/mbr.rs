use std::io::{Read, Seek};

use remora_image::model::{PartitionEntry, PartitionTable, TableKind};

/// Read an MBR as 512-byte sectors. Unlike GPT (whose header `gptman`
/// finds by probing LBA 1 at each sector size), an MBR records no sector
/// size, and none can be learned from a bare `Read + Seek`: an image file
/// has none, a block device's logical one needs a `BLKSSZGET` ioctl.
/// Remora images are built with 512-byte sectors and keep those offsets
/// once flashed, so this is right for every image it's meant to read; an
/// MBR partitioned natively on a 4096-byte-sector disk would read with
/// offsets 8x too small.
pub fn read<R: Read + Seek + ?Sized>(
    reader: &mut R,
) -> Result<PartitionTable, Box<dyn std::error::Error + Send + Sync>> {
    let mbr = mbrman::MBR::read_from(reader, 512)?;
    let sector_size = mbr.sector_size as u64;

    let mut partitions: Vec<PartitionEntry> = mbr
        .iter()
        .filter(|(_, p)| p.sys != 0 && p.sectors != 0)
        .map(|(i, p)| PartitionEntry {
            index: i as u32,
            label: None,
            start_bytes: p.starting_lba as u64 * sector_size,
            size_bytes: p.sectors as u64 * sector_size,
            partition_type: format!("{:02X}", p.sys),
        })
        .collect();
    partitions.sort_by_key(|p| p.index);

    Ok(PartitionTable {
        kind: TableKind::Mbr,
        sector_size,
        partitions,
    })
}
