//! The on-disk QCOW2 header, exactly as specified by QEMU's
//! `docs/interop/qcow2.txt` -- only the fields this codec actually
//! understands. A header naming any feature outside that (backing file,
//! encryption, snapshots, extended L2/subclusters, external data file,
//! non-zero incompatible_features) is rejected by `decode` before any of
//! these constants come into play for cluster addressing.

pub(crate) const MAGIC: [u8; 4] = *b"QFI\xfb";
pub(crate) const V2_HEADER_LEN: usize = 72;
pub(crate) const V3_HEADER_LEN: usize = 104;

/// L1/L2 entries reserve bit 63 (`COPIED`) and, for L2 only, bit 62
/// (`COMPRESSED`) and bit 0 (`ZERO`, v3+). The offset itself lives in bits
/// 9-55 -- bits 0-8 are mandatorily zero (clusters are at least 512-byte
/// aligned), so masking off the top byte and the low 9 bits yields the
/// offset directly, no shifting needed.
pub(crate) const OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;
pub(crate) const L1E_COPIED: u64 = 1 << 63;
pub(crate) const L2E_COPIED: u64 = 1 << 63;
pub(crate) const L2E_COMPRESSED: u64 = 1 << 62;
pub(crate) const L2E_ZERO: u64 = 1 << 0;

#[derive(Debug, Clone)]
pub(crate) struct Header {
    pub version: u32,
    pub backing_file_offset: u64,
    pub cluster_bits: u32,
    pub size: u64,
    pub crypt_method: u32,
    pub l1_size: u32,
    pub l1_table_offset: u64,
    pub refcount_table_offset: u64,
    pub refcount_table_clusters: u32,
    pub incompatible_features: u64,
    pub refcount_order: u32,
}

impl Header {
    pub fn cluster_size(&self) -> u64 {
        1u64 << self.cluster_bits
    }

    /// Parses a header from the first bytes of a qcow2 file. `bytes` must
    /// contain at least `V3_HEADER_LEN` bytes when available; a genuine v2
    /// image shorter than that (whose file body starts right after the
    /// 72-byte v2 header) is still fine to probe with a shorter buffer, as
    /// long as it's at least `V2_HEADER_LEN`.
    pub fn parse(bytes: &[u8]) -> Option<Header> {
        if bytes.len() < V2_HEADER_LEN || bytes[0..4] != MAGIC {
            return None;
        }
        let u32_at = |o: usize| u32::from_be_bytes(bytes[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_be_bytes(bytes[o..o + 8].try_into().unwrap());

        let version = u32_at(4);
        if version != 2 && version != 3 {
            return None;
        }

        let mut header = Header {
            version,
            backing_file_offset: u64_at(8),
            cluster_bits: u32_at(20),
            size: u64_at(24),
            crypt_method: u32_at(32),
            l1_size: u32_at(36),
            l1_table_offset: u64_at(40),
            refcount_table_offset: u64_at(48),
            refcount_table_clusters: u32_at(56),
            incompatible_features: 0,
            refcount_order: 4,
        };

        if version == 3 {
            if bytes.len() < V3_HEADER_LEN {
                return None;
            }
            header.incompatible_features = u64_at(72);
            header.refcount_order = u32_at(96);
        }

        Some(header)
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = vec![0u8; V3_HEADER_LEN];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4..8].copy_from_slice(&self.version.to_be_bytes());
        buf[8..16].copy_from_slice(&self.backing_file_offset.to_be_bytes());
        buf[16..20].copy_from_slice(&0u32.to_be_bytes()); // backing_file_size
        buf[20..24].copy_from_slice(&self.cluster_bits.to_be_bytes());
        buf[24..32].copy_from_slice(&self.size.to_be_bytes());
        buf[32..36].copy_from_slice(&self.crypt_method.to_be_bytes());
        buf[36..40].copy_from_slice(&self.l1_size.to_be_bytes());
        buf[40..48].copy_from_slice(&self.l1_table_offset.to_be_bytes());
        buf[48..56].copy_from_slice(&self.refcount_table_offset.to_be_bytes());
        buf[56..60].copy_from_slice(&self.refcount_table_clusters.to_be_bytes());
        buf[60..64].copy_from_slice(&0u32.to_be_bytes()); // nb_snapshots
        buf[64..72].copy_from_slice(&0u64.to_be_bytes()); // snapshots_offset
        buf[72..80].copy_from_slice(&self.incompatible_features.to_be_bytes());
        buf[80..88].copy_from_slice(&0u64.to_be_bytes()); // compatible_features
        buf[88..96].copy_from_slice(&0u64.to_be_bytes()); // autoclear_features
        buf[96..100].copy_from_slice(&self.refcount_order.to_be_bytes());
        buf[100..104].copy_from_slice(&(V3_HEADER_LEN as u32).to_be_bytes());
        buf
    }
}

pub(crate) fn ceil_div(a: u64, b: u64) -> u64 {
    a.div_ceil(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips_through_serialize_and_parse() {
        let header = Header {
            version: 3,
            backing_file_offset: 0,
            cluster_bits: 16,
            size: 123_456_789,
            crypt_method: 0,
            l1_size: 3,
            l1_table_offset: 65536,
            refcount_table_offset: 131072,
            refcount_table_clusters: 1,
            incompatible_features: 0,
            refcount_order: 4,
        };
        let bytes = header.serialize();
        let parsed = Header::parse(&bytes).unwrap();
        assert_eq!(parsed.version, header.version);
        assert_eq!(parsed.cluster_bits, header.cluster_bits);
        assert_eq!(parsed.size, header.size);
        assert_eq!(parsed.l1_size, header.l1_size);
        assert_eq!(parsed.l1_table_offset, header.l1_table_offset);
        assert_eq!(parsed.refcount_table_offset, header.refcount_table_offset);
        assert_eq!(
            parsed.refcount_table_clusters,
            header.refcount_table_clusters
        );
        assert_eq!(parsed.refcount_order, header.refcount_order);
    }

    #[test]
    fn ceil_div_rounds_up() {
        assert_eq!(ceil_div(0, 8), 0);
        assert_eq!(ceil_div(1, 8), 1);
        assert_eq!(ceil_div(8, 8), 1);
        assert_eq!(ceil_div(9, 8), 2);
    }
}
