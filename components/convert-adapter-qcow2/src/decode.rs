use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use error_stack::Report;
use remora_convert::adapter::{Error, Result};

use crate::header::{Header, L2E_COMPRESSED, L2E_ZERO, OFFSET_MASK, V3_HEADER_LEN};

/// Decode `input` (a qcow2 image) into a plain raw disk image at
/// `output_raw`, sized to the qcow2's own virtual disk size. Unallocated
/// (and explicitly-zero) regions are simply never written, so on any
/// filesystem that supports sparse files, `output_raw` naturally comes out
/// sparse too.
pub(crate) fn decode(input: &Path, output_raw: &Path) -> Result<()> {
    let mut infile =
        File::open(input).map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;

    let mut head_buf = Vec::new();
    (&mut infile)
        .take(V3_HEADER_LEN as u64)
        .read_to_end(&mut head_buf)
        .map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;
    let header = Header::parse(&head_buf)
        .ok_or_else(|| Report::new(Error::InvalidHeader(input.to_path_buf())))?;

    if header.backing_file_offset != 0 {
        return Err(Report::new(Error::UnsupportedFeature(
            input.to_path_buf(),
            "backing file".into(),
        )));
    }
    if header.crypt_method != 0 {
        return Err(Report::new(Error::UnsupportedFeature(
            input.to_path_buf(),
            "encryption".into(),
        )));
    }
    if header.incompatible_features != 0 {
        return Err(Report::new(Error::UnsupportedFeature(
            input.to_path_buf(),
            format!("incompatible_features=0x{:x}", header.incompatible_features),
        )));
    }

    let cluster_size = header.cluster_size();
    let l2_entries_per_cluster = cluster_size / 8;

    let outfile = File::create(output_raw)
        .map_err(|_| Report::new(Error::WriteFile(output_raw.to_path_buf())))?;
    outfile
        .set_len(header.size)
        .map_err(|_| Report::new(Error::WriteFile(output_raw.to_path_buf())))?;
    let mut outfile = outfile;

    // The L1 table is small even for huge disks (a 64 GiB image at 64 KiB
    // clusters needs a 1024-entry, 8 KiB L1 table) -- read it whole.
    let l1_bytes = header.l1_size as u64 * 8;
    let mut l1_buf = vec![0u8; l1_bytes as usize];
    if l1_bytes > 0 {
        infile
            .seek(SeekFrom::Start(header.l1_table_offset))
            .map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;
        infile
            .read_exact(&mut l1_buf)
            .map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;
    }

    let mut l2_buf = vec![0u8; cluster_size as usize];
    let mut data_buf = vec![0u8; cluster_size as usize];

    for group in 0..header.l1_size as u64 {
        let l1_entry = u64::from_be_bytes(
            l1_buf[(group * 8) as usize..(group * 8 + 8) as usize]
                .try_into()
                .unwrap(),
        );
        let l2_offset = l1_entry & OFFSET_MASK;
        if l2_offset == 0 {
            continue;
        }
        infile
            .seek(SeekFrom::Start(l2_offset))
            .map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;
        infile
            .read_exact(&mut l2_buf)
            .map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;

        for j in 0..l2_entries_per_cluster {
            let global_cluster = group * l2_entries_per_cluster + j;
            let logical_offset = global_cluster * cluster_size;
            if logical_offset >= header.size {
                break;
            }

            let entry = u64::from_be_bytes(
                l2_buf[(j * 8) as usize..(j * 8 + 8) as usize]
                    .try_into()
                    .unwrap(),
            );
            if entry & L2E_COMPRESSED != 0 {
                return Err(Report::new(Error::UnsupportedFeature(
                    input.to_path_buf(),
                    "compressed cluster".into(),
                )));
            }
            if entry & L2E_ZERO != 0 {
                continue;
            }
            let data_offset = entry & OFFSET_MASK;
            if data_offset == 0 {
                continue;
            }

            let write_len = cluster_size.min(header.size - logical_offset) as usize;
            infile
                .seek(SeekFrom::Start(data_offset))
                .map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;
            infile
                .read_exact(&mut data_buf)
                .map_err(|_| Report::new(Error::ReadFile(input.to_path_buf())))?;
            outfile
                .seek(SeekFrom::Start(logical_offset))
                .map_err(|_| Report::new(Error::WriteFile(output_raw.to_path_buf())))?;
            outfile
                .write_all(&data_buf[..write_len])
                .map_err(|_| Report::new(Error::WriteFile(output_raw.to_path_buf())))?;
        }
    }

    Ok(())
}
