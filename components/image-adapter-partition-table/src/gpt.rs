use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

use remora_image::model::{PartitionEntry, PartitionTable, TableKind};

pub fn read<R: Read + Seek>(
    reader: &mut R,
) -> Result<PartitionTable, Box<dyn std::error::Error + Send + Sync>> {
    let gpt = gptman::GPT::find_from(reader)?;
    let sector_size = gpt.sector_size;

    let mut partitions = gpt
        .iter()
        .filter(|(_, p)| p.is_used())
        .map(|(i, p)| {
            // An entry ending before it starts is a corrupt table, not an
            // empty partition to write into.
            Ok(PartitionEntry {
                index: i,
                label: Some(p.partition_name.as_str().to_string()).filter(|s| !s.is_empty()),
                start_bytes: p.starting_lba * sector_size,
                size_bytes: p.size()? * sector_size,
                partition_type: format_type_guid(&p.partition_type_guid),
            })
        })
        .collect::<Result<Vec<_>, gptman::Error>>()?;
    partitions.sort_by_key(|p| p.index);

    Ok(PartitionTable {
        kind: TableKind::Gpt,
        sector_size,
        partitions,
    })
}

/// Why a table's `resize_last` refused or failed.
pub enum ResizeError {
    /// Not this table kind: the other one is tried.
    NotPartitioned(Box<dyn std::error::Error + Send + Sync>),
    NoSuchPartition,
    NotLast,
    Unaligned {
        sector_size: u64,
    },
    /// Beyond what an MBR addresses (2 TiB at 512-byte sectors).
    TooBig,
    Io(Box<dyn std::error::Error + Send + Sync>),
}

impl From<std::io::Error> for ResizeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(Box::new(e))
    }
}

impl From<gptman::Error> for ResizeError {
    fn from(e: gptman::Error) -> Self {
        Self::Io(Box::new(e))
    }
}

/// Where the protective MBR's one partition entry starts: its first byte
/// is the boot indicator.
const PMBR_ENTRY_OFFSET: u64 = 446;
const PMBR_BOOTABLE: u8 = 0x80;

/// Resize GPT partition `index`, the last one on the disk, to `size_bytes`,
/// and `file` to end right after it with the backup GPT: the backup's
/// partition array, then its header, as `gptman`'s `update_from` lays them
/// out. The protective MBR is rewritten for the new length, bootable if it
/// was.
pub fn resize_last(file: &mut File, index: u32, size_bytes: u64) -> Result<(), ResizeError> {
    let mut gpt =
        gptman::GPT::find_from(file).map_err(|e| ResizeError::NotPartitioned(Box::new(e)))?;
    let sector_size = gpt.sector_size;
    if size_bytes == 0 || !size_bytes.is_multiple_of(sector_size) {
        return Err(ResizeError::Unaligned { sector_size });
    }
    let used = |i: u32| {
        gpt.iter()
            .find(|(n, p)| *n == i && p.is_used())
            .map(|(_, p)| p.clone())
    };
    let entry = used(index).ok_or(ResizeError::NoSuchPartition)?;
    if gpt
        .iter()
        .any(|(n, p)| n != index && p.is_used() && p.starting_lba > entry.starting_lba)
    {
        return Err(ResizeError::NotLast);
    }

    let ending_lba = entry.starting_lba + size_bytes / sector_size - 1;
    let entries_bytes = u64::from(gpt.header.number_of_partition_entries)
        * u64::from(gpt.header.size_of_partition_entry);
    let array_sectors = entries_bytes.div_ceil(sector_size);
    // The partition, the backup's partition array, then its header.
    let disk_sectors = ending_lba + 1 + array_sectors + 1;

    let mut indicator = [0u8; 1];
    file.seek(SeekFrom::Start(PMBR_ENTRY_OFFSET))?;
    file.read_exact(&mut indicator)?;

    file.set_len(disk_sectors * sector_size)?;
    gpt.header.update_from(file, sector_size)?;
    gpt[index].ending_lba = ending_lba;
    gpt.write_into(file)?;
    if indicator[0] == PMBR_BOOTABLE {
        gptman::GPT::write_bootable_protective_mbr_into(file, sector_size)?;
    } else {
        gptman::GPT::write_protective_mbr_into(file, sector_size)?;
    }
    file.sync_all()?;
    Ok(())
}

/// Format a GPT type GUID's raw on-disk bytes in the usual mixed-endian
/// textual form (e.g. the EFI System Partition type reads back as
/// `C12A7328-F81F-11D2-BA4B-00A0C93EC93B`, matching what a `.wks.in`'s
/// `--part-type=` expects).
fn format_type_guid(bytes: &[u8; 16]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[3], bytes[2], bytes[1], bytes[0],
        bytes[5], bytes[4],
        bytes[7], bytes[6],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}
