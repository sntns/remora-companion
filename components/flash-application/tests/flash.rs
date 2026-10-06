//! End-to-end test of the bmap-based flash path: a crafted sparse image + a
//! real `bmaptool`-generated `.bmap` must (a) only touch the mapped ranges
//! in the destination, leaving everything else untouched, and (b) reject a
//! corrupted image via checksum mismatch.
//!
//! The `.bmap` content below was generated once with the real `bmaptool`
//! against the exact byte layout this test recreates (8 MiB image, 0xAA at
//! byte offset 65536 for 32 KiB, 0xBB at byte offset 6291456 for 16 KiB, zero
//! elsewhere) — the test never shells out to `bmaptool` itself, it just
//! reuses the checksums it produced as a trusted fixture.
//!
//! The destination is a regular file, which the real disk adapter rightly
//! refuses to describe, so the disk vertical is a small stub of its one
//! port that describes whatever disk a test says (same posture as
//! `context-application`'s platform stub).

use std::{
    fs,
    path::{Path, PathBuf},
};

use remora_disk::{
    application::{DiskService, DiskServiceInterface, Error as DiskError, Result as DiskResult},
    model::DiskInfo,
};
use remora_flash::{
    adapter::BmapAdapterService,
    application::{Error, FlashServiceInterface},
    model::FlashRequest,
};
use remora_flash_adapter_bmap::BmapAdapterImpl;
use remora_flash_application::FlashControllerImpl;
use remora_progress::OperationContext;

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

/// Describes every path as `disk`, the way a disk service would describe
/// the node it resolved a path to.
struct StubDisk {
    disk: Option<DiskInfo>,
}

#[async_trait::async_trait]
impl DiskServiceInterface for StubDisk {
    async fn list(&self) -> DiskResult<Vec<DiskInfo>> {
        Ok(self.disk.clone().into_iter().collect())
    }

    async fn info(&self, _path: &Path) -> DiskResult<DiskInfo> {
        self.disk
            .clone()
            .ok_or_else(|| error_stack::Report::new(DiskError::Info))
    }
}

fn controller(disk: DiskInfo) -> FlashControllerImpl {
    FlashControllerImpl::new(
        DiskService::new(StubDisk { disk: Some(disk) }),
        BmapAdapterService::new(BmapAdapterImpl),
    )
}

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
        "remora-flash-test-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A disk the guard lets through, at `path`'s canonical node.
fn removable_non_system_disk(path: &Path) -> DiskInfo {
    DiskInfo {
        path: fs::canonicalize(path).unwrap(),
        size_bytes: IMAGE_SIZE,
        model: None,
        is_removable: true,
        is_system_disk: false,
    }
}

/// Source image, its `.bmap`, and a marker-filled destination in `dir`.
fn fixture(dir: &Path, image: &[u8]) -> (PathBuf, PathBuf, PathBuf) {
    let image_path = dir.join("src.img");
    let bmap_path = dir.join("src.img.bmap");
    let device_path = dir.join("dest.img");
    fs::write(&image_path, image).unwrap();
    fs::write(&bmap_path, BMAP_XML).unwrap();
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();
    (image_path, bmap_path, device_path)
}

fn untouched(device_path: &Path) -> bool {
    fs::read(device_path).unwrap().iter().all(|&b| b == MARKER)
}

#[tokio::test]
async fn bmap_copy_only_touches_mapped_ranges() {
    let dir = tempdir("ok");
    let (image_path, bmap_path, device_path) = fixture(&dir, &build_source_image());

    let request = FlashRequest {
        image: image_path,
        bmap: Some(bmap_path),
        device: device_path.clone(),
        force: false,
    };

    let outcome = controller(removable_non_system_disk(&device_path))
        .flash(&request, &OperationContext::noop())
        .await
        .expect("flash should succeed");
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

#[tokio::test]
async fn bmap_copy_rejects_a_corrupted_image() {
    let dir = tempdir("corrupt");
    let mut corrupted = build_source_image();
    // Flip one byte inside the first mapped range: the .bmap's checksum for
    // that range was computed against the *original* content, so this must
    // be caught rather than silently written.
    corrupted[RANGE_A_OFFSET] = 0xAB;
    let (image_path, bmap_path, device_path) = fixture(&dir, &corrupted);

    let request = FlashRequest {
        image: image_path,
        bmap: Some(bmap_path),
        device: device_path.clone(),
        force: false,
    };

    let result = controller(removable_non_system_disk(&device_path))
        .flash(&request, &OperationContext::noop())
        .await;
    assert!(
        result.is_err(),
        "a corrupted image must fail bmap checksum verification, not be silently written"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn flash_through_a_symlink_writes_the_disk_it_resolves_to() {
    let dir = tempdir("symlink");
    let (image_path, bmap_path, device_path) = fixture(&dir, &build_source_image());
    let link = dir.join("sdc");
    std::os::unix::fs::symlink(&device_path, &link).unwrap();

    let request = FlashRequest {
        image: image_path,
        bmap: Some(bmap_path),
        device: link,
        force: false,
    };
    controller(removable_non_system_disk(&device_path))
        .flash(&request, &OperationContext::noop())
        .await
        .expect("flash should succeed");

    assert!(!untouched(&device_path));
}

#[tokio::test]
async fn flash_refuses_a_device_that_is_not_the_checked_disk() {
    let dir = tempdir("mismatch");
    let (image_path, bmap_path, device_path) = fixture(&dir, &build_source_image());
    // The disk service vouches for another node than the one `device` opens.
    let checked = dir.join("other.img");
    fs::write(&checked, b"").unwrap();

    let request = FlashRequest {
        image: image_path,
        bmap: Some(bmap_path),
        device: device_path.clone(),
        force: false,
    };
    let err = controller(removable_non_system_disk(&checked))
        .flash(&request, &OperationContext::noop())
        .await
        .unwrap_err();

    assert!(matches!(
        err.current_context(),
        Error::TargetMismatch { .. }
    ));
    assert!(untouched(&device_path));
}

#[tokio::test]
async fn flash_refuses_the_system_disk_without_a_separate_preflight() {
    let dir = tempdir("flash-system-disk");
    let (image_path, bmap_path, device_path) = fixture(&dir, &build_source_image());

    let request = FlashRequest {
        image: image_path,
        bmap: Some(bmap_path),
        device: device_path.clone(),
        force: true,
    };
    let disk = DiskInfo {
        is_system_disk: true,
        ..removable_non_system_disk(&device_path)
    };
    let err = controller(disk)
        .flash(&request, &OperationContext::noop())
        .await
        .unwrap_err();

    assert!(matches!(err.current_context(), Error::UnsafeTarget { .. }));
    assert!(untouched(&device_path));
}

#[tokio::test]
async fn preflight_refuses_a_disk_that_looks_like_the_system_disk() {
    let dir = tempdir("system-disk-guard");
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; 1024]).unwrap();

    let disk = DiskInfo {
        is_system_disk: true,
        ..removable_non_system_disk(&device_path)
    };

    assert!(controller(disk)
        .preflight(&device_path, /* force */ true)
        .await
        .is_err());
}

#[tokio::test]
async fn preflight_refuses_a_non_removable_disk_without_force() {
    let dir = tempdir("non-removable-guard");
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; 1024]).unwrap();

    let disk = DiskInfo {
        is_removable: false,
        ..removable_non_system_disk(&device_path)
    };

    assert!(controller(disk.clone())
        .preflight(&device_path, false)
        .await
        .is_err());
    let checked = controller(disk)
        .preflight(&device_path, true)
        .await
        .unwrap();
    assert_eq!(checked.path, fs::canonicalize(&device_path).unwrap());
}
