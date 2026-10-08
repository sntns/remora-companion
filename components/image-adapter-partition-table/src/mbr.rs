use std::{
    fs::File,
    io::{Read, Seek},
};

use crate::gpt::ResizeError;

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

/// Resize primary partition `index`, the last one on the disk, to
/// `size_bytes`, and `file` to end right where it does: an MBR has no
/// backup table to move. The boot code before the table (grub's, say) and
/// whatever sits between it and the first partition are left as they were;
/// so are the partition's CHS fields, which nothing reading Remora images
/// goes by (LBA only).
pub fn resize_last(file: &mut File, index: u32, size_bytes: u64) -> Result<(), ResizeError> {
    let mut mbr =
        mbrman::MBR::read_from(file, 512).map_err(|e| ResizeError::NotPartitioned(Box::new(e)))?;
    let sector_size = u64::from(mbr.sector_size);
    if size_bytes == 0 || !size_bytes.is_multiple_of(sector_size) {
        return Err(ResizeError::Unaligned { sector_size });
    }
    let entry = mbr
        .header
        .get(index as usize)
        .filter(|p| p.is_used())
        .cloned()
        .ok_or(ResizeError::NoSuchPartition)?;
    let after = |p: &mbrman::MBRPartitionEntry| p.is_used() && p.starting_lba > entry.starting_lba;
    if entry.is_extended()
        || !mbr.logical_partitions.is_empty()
        || mbr
            .header
            .iter()
            .any(|(i, p)| i != index as usize && after(p))
    {
        return Err(ResizeError::NotLast);
    }

    let sectors = u32::try_from(size_bytes / sector_size).map_err(|_| ResizeError::TooBig)?;
    let end = u64::from(entry.starting_lba) + u64::from(sectors);
    if end > u64::from(u32::MAX) {
        return Err(ResizeError::TooBig);
    }
    file.set_len(end * sector_size)?;
    if let Some(partition) = mbr.header.get_mut(index as usize) {
        partition.sectors = sectors;
    }
    mbr.header
        .write_into(file)
        .map_err(|e| ResizeError::Io(Box::new(e)))?;
    file.sync_all()?;
    Ok(())
}
