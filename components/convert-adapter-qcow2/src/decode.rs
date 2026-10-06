use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use error_stack::{Report, ResultExt};
use remora_convert::adapter::{Error, Result};

use crate::header::{
    Header, L2E_COMPRESSED, L2E_ZERO, MAX_CLUSTER_BITS, MIN_CLUSTER_BITS, OFFSET_MASK,
    V3_HEADER_LEN,
};

/// Decode `input` (a qcow2 image) into a plain raw disk image at
/// `output_raw`, sized to the qcow2's own virtual disk size. Unallocated
/// (and explicitly-zero) regions are simply never written, so on any
/// filesystem that supports sparse files, `output_raw` naturally comes out
/// sparse too.
pub(crate) fn decode(input: &Path, output_raw: &Path) -> Result<()> {
    let mut infile =
        File::open(input).change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;

    let mut head_buf = Vec::new();
    (&mut infile)
        .take(V3_HEADER_LEN as u64)
        .read_to_end(&mut head_buf)
        .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
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
    if header.nb_snapshots != 0 {
        return Err(Report::new(Error::UnsupportedFeature(
            input.to_path_buf(),
            "snapshots".into(),
        )));
    }

    // Everything below sizes buffers from the header: bound it by what a
    // real image can hold before allocating anything.
    if !(MIN_CLUSTER_BITS..=MAX_CLUSTER_BITS).contains(&header.cluster_bits) {
        return Err(
            Report::new(Error::InvalidHeader(input.to_path_buf())).attach(format!(
                "cluster_bits {} outside {MIN_CLUSTER_BITS}..={MAX_CLUSTER_BITS}",
                header.cluster_bits
            )),
        );
    }
    let input_len = infile
        .metadata()
        .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?
        .len();
    // The L1 table is small even for huge disks (a 64 GiB image at 64 KiB
    // clusters needs a 1024-entry, 8 KiB L1 table) -- read it whole, once
    // it's known to lie inside the file.
    let l1_bytes = header.l1_size as u64 * 8;
    if header
        .l1_table_offset
        .checked_add(l1_bytes)
        .is_none_or(|end| end > input_len)
    {
        return Err(
            Report::new(Error::InvalidHeader(input.to_path_buf())).attach(format!(
                "L1 table of {} entries at offset {} past the end of the {input_len}-byte file",
                header.l1_size, header.l1_table_offset
            )),
        );
    }

    let cluster_size = header.cluster_size();
    let l2_entries_per_cluster = cluster_size / 8;

    let outfile = File::create(output_raw)
        .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))?;
    outfile
        .set_len(header.size)
        .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))?;
    let mut outfile = outfile;

    let mut l1_buf = vec![0u8; l1_bytes as usize];
    if l1_bytes > 0 {
        infile
            .seek(SeekFrom::Start(header.l1_table_offset))
            .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
        infile
            .read_exact(&mut l1_buf)
            .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
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
            .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
        infile
            .read_exact(&mut l2_buf)
            .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;

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
                .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
            infile
                .read_exact(&mut data_buf)
                .change_context_lazy(|| Error::ReadFile(input.to_path_buf()))?;
            outfile
                .seek(SeekFrom::Start(logical_offset))
                .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))?;
            outfile
                .write_all(&data_buf[..write_len])
                .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A v3 image of a valid header, as `tamper` leaves it, then an empty
    /// L1 table.
    fn crafted(name: &str, tamper: impl FnOnce(&mut Header)) -> (PathBuf, PathBuf) {
        let mut header = Header {
            version: 3,
            backing_file_offset: 0,
            cluster_bits: 16,
            size: 1024 * 1024,
            crypt_method: 0,
            l1_size: 1,
            l1_table_offset: 65536,
            refcount_table_offset: 0,
            refcount_table_clusters: 0,
            nb_snapshots: 0,
            incompatible_features: 0,
            refcount_order: 4,
        };
        tamper(&mut header);
        let mut bytes = header.serialize();
        // The header cluster, then an all-zero (unallocated) L1 table.
        bytes.resize(2 * 65536, 0);

        let dir = std::env::temp_dir();
        let tag = format!("remora-qcow2-crafted-{name}-{}", std::process::id());
        let input = dir.join(format!("{tag}.qcow2"));
        std::fs::write(&input, bytes).unwrap();
        (input, dir.join(format!("{tag}.raw")))
    }

    #[test]
    fn decodes_the_untampered_header() {
        let (input, output) = crafted("ok", |_| {});
        decode(&input, &output).unwrap();
        assert_eq!(std::fs::metadata(&output).unwrap().len(), 1024 * 1024);
    }

    #[test]
    fn refuses_a_cluster_size_qemu_would_not_write() {
        for (name, bits) in [("tiny", 8), ("huge", 22), ("absurd", 63), ("overflow", 64)] {
            let (input, output) = crafted(name, |h| h.cluster_bits = bits);
            let err = decode(&input, &output).unwrap_err();
            assert!(
                matches!(err.current_context(), Error::InvalidHeader(_)),
                "{bits}: {err:?}"
            );
        }
    }

    #[test]
    fn refuses_an_l1_table_past_the_end_of_the_file() {
        for (name, l1_size, offset) in [
            ("huge-l1", u32::MAX, 65536),
            ("far-l1", 1, 1 << 40),
            ("wrapping-l1", 1, u64::MAX - 4),
        ] {
            let (input, output) = crafted(name, |h| {
                h.l1_size = l1_size;
                h.l1_table_offset = offset;
            });
            let err = decode(&input, &output).unwrap_err();
            assert!(
                matches!(err.current_context(), Error::InvalidHeader(_)),
                "{name}: {err:?}"
            );
        }
    }

    #[test]
    fn refuses_snapshots() {
        let (input, output) = crafted("snapshots", |h| h.nb_snapshots = 1);
        let err = decode(&input, &output).unwrap_err();
        assert!(matches!(
            err.current_context(),
            Error::UnsupportedFeature(_, feature) if feature == "snapshots"
        ));
    }
}
