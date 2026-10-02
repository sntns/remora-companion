use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use error_stack::Report;
use remora_convert::adapter::{Error, Result};

use crate::header::{ceil_div, Header, L1E_COPIED, L2E_COPIED};

/// 64 KiB clusters, matching `qemu-img`'s own default -- and the only
/// cluster size this encoder ever produces (the decoder still reads
/// whatever `cluster_bits` an input header names).
const CLUSTER_BITS: u32 = 16;
const CLUSTER_SIZE: u64 = 1 << CLUSTER_BITS;
/// 16-bit refcounts (`refcount_order` 4), matching `qemu-img`'s own
/// default. Every cluster this encoder allocates gets refcount exactly 1
/// (no snapshots, no backing file, no sharing -- nothing here is ever
/// referenced twice), so a smaller refcount width would work just as well;
/// this one simply matches what real qcow2 files already look like.
const REFCOUNT_BYTES: u64 = 2;

/// Encode `input_raw` (a plain raw disk image) as a qcow2 v3 image at
/// `output`. Every all-zero cluster of the input is left unallocated
/// (matching `qemu-img convert`'s own sparse-output behavior) rather than
/// written out, so a mostly-empty raw image doesn't balloon into a
/// fully-allocated qcow2 file.
pub(crate) fn encode(input_raw: &Path, output: &Path) -> Result<()> {
    let virtual_size = std::fs::metadata(input_raw)
        .map_err(|_| Report::new(Error::ReadFile(input_raw.to_path_buf())))?
        .len();

    let l2_entries = CLUSTER_SIZE / 8; // 8192 entries/table
    let n_clusters = ceil_div(virtual_size, CLUSTER_SIZE);
    let l1_size = ceil_div(n_clusters.max(1), l2_entries).max(1);

    let is_nonzero = classify_clusters(input_raw, n_clusters, CLUSTER_SIZE)?;

    let mut l1_needs_l2 = vec![false; l1_size as usize];
    for i in 0..n_clusters {
        if is_nonzero[i as usize] {
            l1_needs_l2[(i / l2_entries) as usize] = true;
        }
    }
    let n_l2_tables = l1_needs_l2.iter().filter(|&&b| b).count() as u64;
    let n_data_clusters = is_nonzero.iter().filter(|&&b| b).count() as u64;

    let header_clusters = 1u64;
    let l1_clusters = ceil_div(l1_size * 8, CLUSTER_SIZE).max(1);

    // The refcount table+blocks must themselves be refcounted, so their
    // size depends on the total cluster count, which depends on their own
    // size -- solve by iterating to a fixed point (a handful of iterations
    // always suffice: refcount blocks are tiny relative to real disk
    // images, one block covers 2^16 * (cluster_size/2) = 2 GiB of cluster
    // *count* space at these constants).
    let entries_per_refblock = CLUSTER_SIZE / REFCOUNT_BYTES;
    let mut refcount_blocks = 1u64;
    let mut refcount_table_clusters = 1u64;
    for _ in 0..32 {
        let total_guess = header_clusters
            + l1_clusters
            + refcount_table_clusters
            + refcount_blocks
            + n_l2_tables
            + n_data_clusters;
        let needed_blocks = ceil_div(total_guess, entries_per_refblock).max(1);
        let needed_table_clusters = ceil_div(needed_blocks * 8, CLUSTER_SIZE).max(1);
        if needed_blocks == refcount_blocks && needed_table_clusters == refcount_table_clusters {
            break;
        }
        refcount_blocks = needed_blocks;
        refcount_table_clusters = needed_table_clusters;
    }

    // Lay out every structure as consecutive cluster indices, in a fixed
    // order: header, L1 table, refcount table, refcount blocks, L2 tables,
    // data clusters.
    let header_start = 0u64;
    let l1_start = header_start + header_clusters;
    let refcount_table_start = l1_start + l1_clusters;
    let refcount_blocks_start = refcount_table_start + refcount_table_clusters;
    let l2_table_start = refcount_blocks_start + refcount_blocks;
    let data_start = l2_table_start + n_l2_tables;
    let total_clusters = data_start + n_data_clusters;

    if refcount_blocks * entries_per_refblock < total_clusters {
        // The fixed point above must guarantee this; if it somehow didn't
        // converge within budget, fail loudly rather than emit an image
        // with clusters missing a refcount.
        return Err(Report::new(Error::UnsupportedFeature(
            output.to_path_buf(),
            "refcount sizing failed to converge".into(),
        )));
    }

    let mut l2_table_index_of_group: Vec<Option<u64>> = vec![None; l1_size as usize];
    {
        let mut next = 0u64;
        for (g, needs) in l1_needs_l2.iter().enumerate() {
            if *needs {
                l2_table_index_of_group[g] = Some(l2_table_start + next);
                next += 1;
            }
        }
    }
    let mut data_cluster_index_of = vec![0u64; n_clusters as usize];
    {
        let mut next = 0u64;
        for i in 0..n_clusters as usize {
            if is_nonzero[i] {
                data_cluster_index_of[i] = data_start + next;
                next += 1;
            }
        }
    }

    let mut outfile =
        File::create(output).map_err(|_| Report::new(Error::WriteFile(output.to_path_buf())))?;
    outfile
        .set_len(total_clusters * CLUSTER_SIZE)
        .map_err(|_| Report::new(Error::WriteFile(output.to_path_buf())))?;

    write_cluster(&mut outfile, output, header_start, &{
        let header = Header {
            version: 3,
            backing_file_offset: 0,
            cluster_bits: CLUSTER_BITS,
            size: virtual_size,
            crypt_method: 0,
            l1_size: l1_size as u32,
            l1_table_offset: l1_start * CLUSTER_SIZE,
            refcount_table_offset: refcount_table_start * CLUSTER_SIZE,
            refcount_table_clusters: refcount_table_clusters as u32,
            incompatible_features: 0,
            refcount_order: 4,
        };
        let mut buf = header.serialize();
        buf.resize(CLUSTER_SIZE as usize, 0);
        buf
    })?;

    // L1 table.
    let mut l1_buf = vec![0u8; (l1_clusters * CLUSTER_SIZE) as usize];
    for (g, index) in l2_table_index_of_group.iter().enumerate() {
        if let Some(cluster_index) = index {
            let entry = (cluster_index * CLUSTER_SIZE) | L1E_COPIED;
            l1_buf[g * 8..g * 8 + 8].copy_from_slice(&entry.to_be_bytes());
        }
    }
    write_cluster(&mut outfile, output, l1_start, &l1_buf)?;

    // Refcount table: one entry per refcount block, pointing at its own
    // cluster; any remaining capacity in the table stays zero (unused).
    let mut refcount_table_buf = vec![0u8; (refcount_table_clusters * CLUSTER_SIZE) as usize];
    for b in 0..refcount_blocks {
        let entry = (refcount_blocks_start + b) * CLUSTER_SIZE;
        let o = (b * 8) as usize;
        refcount_table_buf[o..o + 8].copy_from_slice(&entry.to_be_bytes());
    }
    write_cluster(
        &mut outfile,
        output,
        refcount_table_start,
        &refcount_table_buf,
    )?;

    // Refcount blocks: every cluster in [0, total_clusters) we actually
    // allocated gets refcount 1; nothing here is ever shared or snapshotted,
    // so that's the only value that ever appears.
    let mut refcount_block_buf = vec![0u8; (refcount_blocks * CLUSTER_SIZE) as usize];
    for cluster in 0..total_clusters {
        let o = (cluster * REFCOUNT_BYTES) as usize;
        refcount_block_buf[o..o + REFCOUNT_BYTES as usize].copy_from_slice(&1u16.to_be_bytes());
    }
    write_cluster(
        &mut outfile,
        output,
        refcount_blocks_start,
        &refcount_block_buf,
    )?;

    // L2 tables.
    for (g, index) in l2_table_index_of_group.iter().enumerate() {
        let Some(cluster_index) = index else { continue };
        let mut l2_buf = vec![0u8; CLUSTER_SIZE as usize];
        for j in 0..l2_entries {
            let global = g as u64 * l2_entries + j;
            if global >= n_clusters || !is_nonzero[global as usize] {
                continue;
            }
            let entry = (data_cluster_index_of[global as usize] * CLUSTER_SIZE) | L2E_COPIED;
            let o = (j * 8) as usize;
            l2_buf[o..o + 8].copy_from_slice(&entry.to_be_bytes());
        }
        write_cluster(&mut outfile, output, *cluster_index, &l2_buf)?;
    }

    // Data clusters: copy the actual bytes from the raw input, in logical
    // order (sequential read from the source, scattered writes to their
    // assigned cluster in the destination).
    let mut infile =
        File::open(input_raw).map_err(|_| Report::new(Error::ReadFile(input_raw.to_path_buf())))?;
    let mut buf = vec![0u8; CLUSTER_SIZE as usize];
    for i in 0..n_clusters {
        if !is_nonzero[i as usize] {
            continue;
        }
        let logical_offset = i * CLUSTER_SIZE;
        let read_len = CLUSTER_SIZE.min(virtual_size - logical_offset) as usize;
        infile
            .seek(SeekFrom::Start(logical_offset))
            .map_err(|_| Report::new(Error::ReadFile(input_raw.to_path_buf())))?;
        infile
            .read_exact(&mut buf[..read_len])
            .map_err(|_| Report::new(Error::ReadFile(input_raw.to_path_buf())))?;
        if read_len < buf.len() {
            buf[read_len..].fill(0);
        }
        write_cluster(
            &mut outfile,
            output,
            data_cluster_index_of[i as usize],
            &buf,
        )?;
    }

    Ok(())
}

fn write_cluster(
    outfile: &mut File,
    output: &Path,
    cluster_index: u64,
    bytes: &[u8],
) -> Result<()> {
    outfile
        .seek(SeekFrom::Start(cluster_index * CLUSTER_SIZE))
        .map_err(|_| Report::new(Error::WriteFile(output.to_path_buf())))?;
    outfile
        .write_all(bytes)
        .map_err(|_| Report::new(Error::WriteFile(output.to_path_buf())))
}

/// Reads `path` sequentially, cluster by cluster, recording which logical
/// clusters are entirely zero. Streamed rather than loaded whole, so this
/// stays cheap even for a multi-GiB disk image.
fn classify_clusters(path: &Path, n_clusters: u64, cluster_size: u64) -> Result<Vec<bool>> {
    let mut file =
        File::open(path).map_err(|_| Report::new(Error::ReadFile(path.to_path_buf())))?;
    let file_len = file
        .metadata()
        .map_err(|_| Report::new(Error::ReadFile(path.to_path_buf())))?
        .len();

    let mut result = vec![false; n_clusters as usize];
    let mut buf = vec![0u8; cluster_size as usize];
    for i in 0..n_clusters {
        let offset = i * cluster_size;
        let read_len = cluster_size.min(file_len.saturating_sub(offset)) as usize;
        if read_len == 0 {
            continue;
        }
        file.read_exact(&mut buf[..read_len])
            .map_err(|_| Report::new(Error::ReadFile(path.to_path_buf())))?;
        result[i as usize] = buf[..read_len].iter().any(|&b| b != 0);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(label: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-convert-adapter-qcow2-encode-test-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        path
    }

    /// A raw fixture spanning several clusters at `CLUSTER_SIZE`, mixing
    /// all-zero clusters (should end up unallocated) with non-zero ones,
    /// and ending mid-cluster (size not a multiple of `CLUSTER_SIZE`) --
    /// the two edge cases most likely to break a hand-rolled codec.
    fn build_mixed_raw_fixture(path: &Path) {
        let mut f = File::create(path).unwrap();
        // Cluster 0: non-zero.
        f.write_all(&vec![0xABu8; CLUSTER_SIZE as usize]).unwrap();
        // Cluster 1: all zero.
        f.write_all(&vec![0u8; CLUSTER_SIZE as usize]).unwrap();
        // Cluster 2: non-zero.
        f.write_all(&vec![0xCDu8; CLUSTER_SIZE as usize]).unwrap();
        // Cluster 3: all zero.
        f.write_all(&vec![0u8; CLUSTER_SIZE as usize]).unwrap();
        // Cluster 4: partial, non-zero.
        f.write_all(&vec![0xEFu8; (CLUSTER_SIZE / 2) as usize])
            .unwrap();
    }

    #[test]
    fn round_trips_through_our_own_encode_and_decode() {
        let raw = temp_path("raw");
        build_mixed_raw_fixture(&raw);

        let qcow2 = temp_path("out.qcow2");
        encode(&raw, &qcow2).unwrap();

        let back = temp_path("back.raw");
        decode(&qcow2, &back).unwrap();

        assert_eq!(fs::read(&raw).unwrap(), fs::read(&back).unwrap());

        for p in [raw, qcow2, back] {
            let _ = fs::remove_file(p);
        }
    }

    #[test]
    fn a_fully_zero_image_ends_up_tiny() {
        let raw = temp_path("zero.raw");
        fs::File::create(&raw)
            .unwrap()
            .set_len(8 * CLUSTER_SIZE)
            .unwrap();

        let qcow2 = temp_path("zero.qcow2");
        encode(&raw, &qcow2).unwrap();

        // Header + L1 table + refcount table + refcount block: no L2
        // tables, no data clusters. Must stay far smaller than the 8
        // allocated-clusters-worth the naive fully-allocated approach
        // would produce.
        let qcow2_size = fs::metadata(&qcow2).unwrap().len();
        assert!(qcow2_size <= 4 * CLUSTER_SIZE);

        let back = temp_path("zero-back.raw");
        decode(&qcow2, &back).unwrap();
        assert_eq!(fs::read(&raw).unwrap(), fs::read(&back).unwrap());

        for p in [raw, qcow2, back] {
            let _ = fs::remove_file(p);
        }
    }
}
