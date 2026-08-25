use std::io::{Read, Seek};

use remora_etcher_image::model::{PartitionEntry, PartitionTable, TableKind};

pub fn read<R: Read + Seek>(
    reader: &mut R,
) -> Result<PartitionTable, Box<dyn std::error::Error + Send + Sync>> {
    let gpt = gptman::GPT::find_from(reader)?;
    let sector_size = gpt.sector_size;

    let mut partitions: Vec<PartitionEntry> = gpt
        .iter()
        .filter(|(_, p)| p.is_used())
        .map(|(i, p)| PartitionEntry {
            index: i,
            label: Some(p.partition_name.as_str().to_string()).filter(|s| !s.is_empty()),
            start_bytes: p.starting_lba * sector_size,
            size_bytes: p.size().unwrap_or(0) * sector_size,
            partition_type: format_type_guid(&p.partition_type_guid),
        })
        .collect();
    partitions.sort_by_key(|p| p.index);

    Ok(PartitionTable {
        kind: TableKind::Gpt,
        sector_size,
        partitions,
    })
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
