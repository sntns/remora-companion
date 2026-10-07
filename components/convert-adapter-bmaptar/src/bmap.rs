use sha2::{Digest, Sha256};

use remora_unpack::BLOCK_SIZE;

/// What bmaptool writes, and the version it writes.
const VERSION: &str = "2.0";
const CHECKSUM_TYPE: &str = "sha256";
const CHECKSUM_LEN: usize = 64;

/// One mapped range: its first and last block, and the SHA-256 of its bytes
/// (up to the image's end, for the last block).
pub(crate) struct Range {
    pub first: u64,
    pub last: u64,
    pub checksum: [u8; 32],
}

/// The `.bmap` of an `image_size`-byte image whose data is `ranges`, as
/// `bmaptool create` writes it, comments and padding included, so the two
/// tell apart only where their maps do: bmaptool's own template, then the
/// fields it fills in once it has mapped the image, the mapped size and
/// count overwritten into the room it left for them, then the checksum of
/// the whole file, made with that field all zeros.
pub(crate) fn to_xml(image_size: u64, ranges: &[Range]) -> String {
    let block_size = BLOCK_SIZE as u64;
    let blocks = image_size.div_ceil(block_size);
    let mapped: u64 = ranges.iter().map(|r| r.last - r.first + 1).sum();
    let image_size_human = human_size(image_size);

    // bmaptool's own (BmapCreate.py's `_BMAP_START_TEMPLATE`), verbatim.
    let mut xml = format!(
        include_str!("bmap.template"),
        version = VERSION,
        image_size_human = image_size_human,
        image_size = image_size,
        block_size = block_size,
        blocks = blocks,
    );
    xml.push_str("    <!-- Count of mapped blocks: ");
    let mapped_size_at = xml.len();
    xml.push_str(&format!(
        "{} or {}   -->\n",
        " ".repeat(image_size_human.len()),
        " ".repeat("100.0%".len())
    ));
    xml.push_str("    <MappedBlocksCount> ");
    let mapped_count_at = xml.len();
    xml.push_str(&format!(
        "{} </MappedBlocksCount>\n\n",
        " ".repeat(blocks.to_string().len())
    ));
    xml.push_str("    <!-- Type of checksum used in this file -->\n");
    xml.push_str(&format!(
        "    <ChecksumType> {CHECKSUM_TYPE} </ChecksumType>\n\n"
    ));
    xml.push_str("    <!-- The checksum of this bmap file. When it is calculated, the value of\n");
    xml.push_str("         the checksum has to be zero (all ASCII \"0\" symbols).  -->\n");
    xml.push_str("    <BmapFileChecksum> ");
    let checksum_at = xml.len();
    xml.push_str(&"0".repeat(CHECKSUM_LEN));
    xml.push_str(" </BmapFileChecksum>\n\n");
    xml.push_str("    <!-- The block map which consists of elements which may either be a\n");
    xml.push_str("         range of blocks or a single block. The 'chksum' attribute\n");
    xml.push_str("         (if present) is the checksum of this blocks range. -->\n");
    xml.push_str("    <BlockMap>\n");
    for range in ranges {
        let checksum = hex(&range.checksum);
        if range.first == range.last {
            xml.push_str(&format!(
                "        <Range chksum=\"{checksum}\"> {} </Range>\n",
                range.first
            ));
        } else {
            xml.push_str(&format!(
                "        <Range chksum=\"{checksum}\"> {}-{} </Range>\n",
                range.first, range.last
            ));
        }
    }
    xml.push_str("    </BlockMap>\n");
    xml.push_str("</bmap>\n");

    let percent = if blocks == 0 {
        0.0
    } else {
        mapped as f64 * 100.0 / blocks as f64
    };
    let mapped_size = format!("{} or {percent:.1}%", human_size(mapped * block_size));
    overwrite(&mut xml, mapped_size_at, &mapped_size);
    overwrite(&mut xml, mapped_count_at, &mapped.to_string());
    let checksum = hex(&Sha256::digest(xml.as_bytes()));
    overwrite(&mut xml, checksum_at, &checksum);
    xml
}

/// Write `text` over `xml`'s bytes from `at` on, as bmaptool seeks back and
/// writes into the file it made.
fn overwrite(xml: &mut String, at: usize, text: &str) {
    let mut bytes = std::mem::take(xml).into_bytes();
    let end = at + text.len();
    if end > bytes.len() {
        bytes.resize(end, b' ');
    }
    bytes[at..end].copy_from_slice(text.as_bytes());
    *xml = String::from_utf8(bytes).expect("ASCII written over ASCII");
}

/// bmaptool's `human_size`, to the digit (its last step's unit included).
fn human_size(size: u64) -> String {
    if size == 1 {
        return "1 byte".to_string();
    }
    if size < 512 {
        return format!("{size} bytes");
    }
    let mut size = size as f64;
    for unit in ["KiB", "MiB", "GiB", "TiB"] {
        size /= 1024.0;
        if size < 1024.0 {
            return format!("{size:.1} {unit}");
        }
    }
    format!("{size:.1} EiB")
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_bmaptool_writes_them() {
        assert_eq!(human_size(1), "1 byte");
        assert_eq!(human_size(511), "511 bytes");
        assert_eq!(human_size(1_000_000), "976.6 KiB");
        assert_eq!(human_size(8192), "8.0 KiB");
        assert_eq!(human_size(3 << 30), "3.0 GiB");
    }

    #[test]
    fn the_bmap_parses_and_checks_out() {
        let ranges = [
            Range {
                first: 0,
                last: 0,
                checksum: [0xab; 32],
            },
            Range {
                first: 3,
                last: 7,
                checksum: [0xcd; 32],
            },
        ];
        let xml = to_xml(10 * 4096 + 5, &ranges);

        let bmap = bmap_parser::Bmap::from_xml(&xml).unwrap();
        assert_eq!(bmap.image_size(), 10 * 4096 + 5);
        assert_eq!(bmap.blocks(), 11);
        assert_eq!(bmap.mapped_blocks(), 6);
        assert_eq!(bmap.block_map().len(), 2);
        assert!(xml.contains("<!-- Count of mapped blocks: 24.0 KiB or 54.5%"));

        // The file's checksum is the one of the file with it zeroed.
        let at = xml.find("<BmapFileChecksum> ").unwrap() + "<BmapFileChecksum> ".len();
        let checksum = xml[at..at + CHECKSUM_LEN].to_string();
        let zeroed = xml.replacen(&checksum, &"0".repeat(CHECKSUM_LEN), 1);
        assert_eq!(hex(&Sha256::digest(zeroed.as_bytes())), checksum);
    }
}
