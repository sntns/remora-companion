//! End-to-end test of the bmap-based flash path (Phase 1 exit criterion):
//! a crafted sparse image + a real `bmaptool`-generated `.bmap` must (a)
//! only touch the mapped ranges in the destination, leaving everything else
//! untouched, and (b) reject a corrupted image via checksum mismatch.
//!
//! The `.bmap` content below was generated once with the real `bmaptool`
//! against the exact byte layout this test recreates (8 MiB image, 0xAA at
//! byte offset 65536 for 32 KiB, 0xBB at byte offset 6291456 for 16 KiB, zero
//! elsewhere) — the test never shells out to `bmaptool` itself, it just
//! reuses the checksums it produced as a trusted fixture.

use std::{fs, path::PathBuf};

use remora_etcher_core::{
    application::flash::{self, FlashRequest},
    model::disk::DiskInfo,
};

const IMAGE_SIZE: u64 = 8 * 1024 * 1024;
const RANGE_A_OFFSET: usize = 64 * 1024;
const RANGE_A_LEN: usize = 32 * 1024;
const RANGE_B_OFFSET: usize = 6 * 1024 * 1024;
const RANGE_B_LEN: usize = 16 * 1024;
const MARKER: u8 = 0xFF;

const BMAP_XML: &str = r#"<?xml version="1.0" ?>
<bmap version="2.0">
    <ImageSize> 8388608 </ImageSize>
    <BlockSize> 4096 </BlockSize>
    <BlocksCount> 2048 </BlocksCount>
    <MappedBlocksCount> 12 </MappedBlocksCount>
    <ChecksumType> sha256 </ChecksumType>
    <BmapFileChecksum> 221b732c538d15c95a91874ae8ee188e6794a3aa5f8033dac8656a06021e305a </BmapFileChecksum>
    <BlockMap>
        <Range chksum="8bee41c7e371d35c63d39a502b41e0d2ff0f3453d9aa1bb90a33046a0a002b24"> 16-23 </Range>
        <Range chksum="e5dd9924e400b0e489dc661cd6198afa4d624f9fe228573f8db9813655140507"> 1536-1539 </Range>
    </BlockMap>
</bmap>
"#;

fn build_source_image() -> Vec<u8> {
    let mut data = vec![0u8; IMAGE_SIZE as usize];
    data[RANGE_A_OFFSET..RANGE_A_OFFSET + RANGE_A_LEN].fill(0xAA);
    data[RANGE_B_OFFSET..RANGE_B_OFFSET + RANGE_B_LEN].fill(0xBB);
    data
}

fn tempdir(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "remora-etcher-flash-test-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn removable_non_system_disk(path: PathBuf) -> DiskInfo {
    DiskInfo {
        path,
        size_bytes: IMAGE_SIZE,
        model: None,
        is_removable: true,
        is_system_disk: false,
    }
}

#[test]
fn bmap_copy_only_touches_mapped_ranges() {
    let dir = tempdir("ok");
    let image_path = dir.join("src.img");
    let bmap_path = dir.join("src.img.bmap");
    let device_path = dir.join("dest.img");

    fs::write(&image_path, build_source_image()).unwrap();
    fs::write(&bmap_path, BMAP_XML).unwrap();
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let request = FlashRequest {
        image: image_path,
        bmap: Some(bmap_path),
        device: device_path.clone(),
        force: false,
    };
    let info = removable_non_system_disk(device_path.clone());

    let outcome = flash::flash(&request, &info).expect("flash should succeed");
    assert!(outcome.used_bmap);
    assert_eq!(outcome.bytes_written, (RANGE_A_LEN + RANGE_B_LEN) as u64);

    let written = fs::read(&device_path).unwrap();
    assert_eq!(written.len(), IMAGE_SIZE as usize);

    // Mapped ranges landed exactly where the source had them.
    assert!(written[RANGE_A_OFFSET..RANGE_A_OFFSET + RANGE_A_LEN]
        .iter()
        .all(|&b| b == 0xAA));
    assert!(written[RANGE_B_OFFSET..RANGE_B_OFFSET + RANGE_B_LEN]
        .iter()
        .all(|&b| b == 0xBB));

    // Everything outside the mapped ranges was left untouched (still the
    // marker byte), proving the unmapped regions were genuinely skipped
    // rather than copied-as-zero.
    let mut expected_untouched = 0usize;
    let mut actually_untouched = 0usize;
    for (i, &b) in written.iter().enumerate() {
        let in_range_a = (RANGE_A_OFFSET..RANGE_A_OFFSET + RANGE_A_LEN).contains(&i);
        let in_range_b = (RANGE_B_OFFSET..RANGE_B_OFFSET + RANGE_B_LEN).contains(&i);
        if !in_range_a && !in_range_b {
            expected_untouched += 1;
            if b == MARKER {
                actually_untouched += 1;
            }
        }
    }
    assert_eq!(expected_untouched, actually_untouched);
}

#[test]
fn bmap_copy_rejects_a_corrupted_image() {
    let dir = tempdir("corrupt");
    let image_path = dir.join("src.img");
    let bmap_path = dir.join("src.img.bmap");
    let device_path = dir.join("dest.img");

    let mut corrupted = build_source_image();
    // Flip one byte inside the first mapped range: the .bmap's checksum for
    // that range was computed against the *original* content, so this must
    // be caught rather than silently written.
    corrupted[RANGE_A_OFFSET] = 0xAB;

    fs::write(&image_path, &corrupted).unwrap();
    fs::write(&bmap_path, BMAP_XML).unwrap();
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let request = FlashRequest {
        image: image_path,
        bmap: Some(bmap_path),
        device: device_path.clone(),
        force: false,
    };
    let info = removable_non_system_disk(device_path);

    let result = flash::flash(&request, &info);
    assert!(
        result.is_err(),
        "a corrupted image must fail bmap checksum verification, not be silently written"
    );
}

#[test]
fn preflight_refuses_a_disk_that_looks_like_the_system_disk() {
    let dir = tempdir("system-disk-guard");
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; 1024]).unwrap();

    let info = DiskInfo {
        path: device_path,
        size_bytes: 1024,
        model: None,
        is_removable: true,
        is_system_disk: true,
    };

    assert!(flash::preflight(&info, /* force */ true).is_err());
}

#[test]
fn preflight_refuses_a_non_removable_disk_without_force() {
    let dir = tempdir("non-removable-guard");
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; 1024]).unwrap();

    let info = DiskInfo {
        path: device_path,
        size_bytes: 1024,
        model: None,
        is_removable: false,
        is_system_disk: false,
    };

    assert!(flash::preflight(&info, false).is_err());
    assert!(flash::preflight(&info, true).is_ok());
}
