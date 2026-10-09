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
    adapter::{
        bmap::BmapAdapterService,
        release::{self, ArtifactChunks, ReleaseArtifactAdapter, ReleaseArtifactAdapterService},
        source::ImageSourceAdapterService,
    },
    application::{Error, FlashServiceInterface},
    model::{
        BmapOrigin, BmapSource, BmapSummary, Compression, DiskImage, FlashRequest, ImageOrigin,
        ImageSummary, ReleaseArtifact,
    },
};
use remora_flash_adapter_bmap::BmapAdapterImpl;
use remora_flash_adapter_file::ImageSourceAdapterImpl;
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
    controller_with(disk, StubRelease::default())
}

fn controller_with(disk: DiskInfo, release: StubRelease) -> FlashControllerImpl {
    FlashControllerImpl::new(
        DiskService::new(StubDisk { disk: Some(disk) }),
        ImageSourceAdapterService::new(ImageSourceAdapterImpl),
        BmapAdapterService::new(BmapAdapterImpl),
        ReleaseArtifactAdapterService::new(release),
    )
}

/// A release holding one artifact, `bytes`, served from any offset in
/// small chunks; records each download's offset.
#[derive(Default)]
struct StubRelease {
    bytes: Vec<u8>,
    offsets: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
}

struct StubChunks(std::vec::IntoIter<Vec<u8>>);

#[async_trait::async_trait]
impl ArtifactChunks for StubChunks {
    async fn next(&mut self) -> release::Result<Option<Vec<u8>>> {
        Ok(self.0.next())
    }
}

#[async_trait::async_trait]
impl ReleaseArtifactAdapter for StubRelease {
    async fn disk_images(
        &self,
        _: Option<&remora_context::model::ContextOverride>,
        _: &str,
        _: &str,
    ) -> release::Result<Vec<DiskImage>> {
        Ok(vec![DiskImage {
            file_name: "src.img.bmaptar".into(),
            boards: vec!["rp5".into()],
            size: self.bytes.len() as u64,
            tag_condition: "board:rp5 && type:diskimage".into(),
        }])
    }

    async fn download(
        &self,
        _: &ReleaseArtifact,
        offset: u64,
    ) -> release::Result<Box<dyn ArtifactChunks>> {
        self.offsets.lock().unwrap().push(offset);
        let pieces: Vec<_> = self.bytes[offset as usize..]
            .chunks(64 * 1024)
            .map(<[u8]>::to_vec)
            .collect();
        Ok(Box::new(StubChunks(pieces.into_iter())))
    }
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

/// bzip2'd as pbzip2 (Yocto's) does it: several concatenated streams.
fn pbzip2(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut out = Vec::new();
    for chunk in data.chunks(data.len() / 4) {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        encoder.write_all(chunk).unwrap();
        out.extend(encoder.finish().unwrap());
    }
    out
}

/// A `.bmaptar` as meta-remora builds it: the bmap, then the zstd'd image.
fn bmaptar(path: &Path, image: &[u8]) {
    let mut builder = tar::Builder::new(fs::File::create(path).unwrap());
    for (name, data) in [
        ("src.img.bmap", BMAP_XML.as_bytes().to_vec()),
        ("src.img.zst", zstd::stream::encode_all(image, 3).unwrap()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, &data[..]).unwrap();
    }
    builder.finish().unwrap();
}

/// The source image as a sparse file: holes but for its two ranges, as a
/// build (or `remora-etcher convert to-raw`) leaves one.
fn sparse_source_image(path: &Path) {
    use std::io::{Seek, SeekFrom, Write};
    let image = build_source_image();
    let mut file = fs::File::create(path).unwrap();
    for (offset, len) in [(RANGE_A_OFFSET, RANGE_A_LEN), (RANGE_B_OFFSET, RANGE_B_LEN)] {
        file.seek(SeekFrom::Start(offset as u64)).unwrap();
        file.write_all(&image[offset..offset + len]).unwrap();
    }
    file.set_len(IMAGE_SIZE).unwrap();
}

/// Only the mapped ranges were written, with the source's bytes.
fn only_mapped_ranges_written(device_path: &Path) -> bool {
    let written = fs::read(device_path).unwrap();
    written.iter().enumerate().all(|(i, &b)| {
        if (RANGE_A_OFFSET..RANGE_A_OFFSET + RANGE_A_LEN).contains(&i) {
            b == 0xAA
        } else if (RANGE_B_OFFSET..RANGE_B_OFFSET + RANGE_B_LEN).contains(&i) {
            b == 0xBB
        } else {
            b == MARKER
        }
    })
}

#[tokio::test]
async fn a_bmaptar_flashes_its_image_with_its_own_bmap() {
    let dir = tempdir("bmaptar");
    let bundle = dir.join("src.img.bmaptar");
    bmaptar(&bundle, &build_source_image());
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let request = FlashRequest {
        image: bundle.into(),
        bmap: BmapSource::Auto,
        device: device_path.clone(),
        force: false,
    };
    let outcome = controller(removable_non_system_disk(&device_path))
        .flash(&request, &OperationContext::noop())
        .await
        .expect("flash should succeed");

    assert!(outcome.used_bmap);
    assert_eq!(outcome.bytes_written, (RANGE_A_LEN + RANGE_B_LEN) as u64);
    assert!(only_mapped_ranges_written(&device_path));
}

/// What a factory job hands the flasher: the shipped `.bmaptar` converted
/// to raw, provisioned, and converted back by `remora-etcher convert`.
#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "sparse files, as Linux filesystems make them"
)]
async fn a_bmaptar_made_by_convert_flashes_like_meta_remoras() {
    use remora_convert::adapter::ContainerFormatAdapter;

    let dir = tempdir("bmaptar-convert");
    let raw = dir.join("src.img");
    sparse_source_image(&raw);
    let bundle = dir.join("src.img.bmaptar");
    remora_convert_adapter_bmaptar::BmaptarAdapterImpl
        .encode_from_raw(&raw, &bundle)
        .unwrap();
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let request = FlashRequest {
        image: bundle.into(),
        bmap: BmapSource::Auto,
        device: device_path.clone(),
        force: false,
    };
    let controller = controller(removable_non_system_disk(&device_path));
    let summary = controller.inspect(&request).await.unwrap();
    // The same map as bmaptool's, for the same file.
    assert_eq!(summary.compression, Some(Compression::Zstd));
    assert_eq!(summary.image_size, Some(IMAGE_SIZE));
    let bmap = summary.bmap.unwrap();
    assert_eq!(bmap.mapped_size, (RANGE_A_LEN + RANGE_B_LEN) as u64);
    assert_eq!(bmap.ranges, 2);
    let outcome = controller
        .flash(&request, &OperationContext::noop())
        .await
        .expect("flash should succeed");

    assert!(outcome.used_bmap);
    assert_eq!(outcome.bytes_written, (RANGE_A_LEN + RANGE_B_LEN) as u64);
    assert!(only_mapped_ranges_written(&device_path));
}

#[tokio::test]
async fn a_bmaptar_with_a_corrupted_image_is_refused() {
    let dir = tempdir("bmaptar-corrupt");
    let mut corrupted = build_source_image();
    corrupted[RANGE_B_OFFSET] = 0xBC;
    let bundle = dir.join("src.img.bmaptar");
    bmaptar(&bundle, &corrupted);
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let request = FlashRequest {
        image: bundle.into(),
        bmap: BmapSource::Auto,
        device: device_path.clone(),
        force: false,
    };
    let result = controller(removable_non_system_disk(&device_path))
        .flash(&request, &OperationContext::noop())
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn a_compressed_image_falls_back_to_its_sibling_bmap() {
    // Yocto's own pair: x.wic.bz2 next to x.wic.bmap.
    let dir = tempdir("bz2-sibling");
    let image_path = dir.join("src.img.bz2");
    fs::write(&image_path, pbzip2(&build_source_image())).unwrap();
    fs::write(dir.join("src.img.bmap"), BMAP_XML).unwrap();
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let request = FlashRequest {
        image: image_path.into(),
        bmap: BmapSource::Auto,
        device: device_path.clone(),
        force: false,
    };
    let outcome = controller(removable_non_system_disk(&device_path))
        .flash(&request, &OperationContext::noop())
        .await
        .expect("flash should succeed");

    assert!(outcome.used_bmap);
    assert!(only_mapped_ranges_written(&device_path));
}

#[tokio::test]
async fn a_compressed_image_without_a_bmap_is_copied_whole() {
    let dir = tempdir("bz2-nobmap");
    let image_path = dir.join("src.img.bz2");
    fs::write(&image_path, pbzip2(&build_source_image())).unwrap();
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let request = FlashRequest {
        image: image_path.into(),
        bmap: BmapSource::Auto,
        device: device_path.clone(),
        force: false,
    };
    let outcome = controller(removable_non_system_disk(&device_path))
        .flash(&request, &OperationContext::noop())
        .await
        .expect("flash should succeed");

    assert!(!outcome.used_bmap);
    assert_eq!(outcome.bytes_written, IMAGE_SIZE);
    assert_eq!(fs::read(&device_path).unwrap(), build_source_image());
}

#[tokio::test]
async fn bmap_copy_only_touches_mapped_ranges() {
    let dir = tempdir("ok");
    let (image_path, bmap_path, device_path) = fixture(&dir, &build_source_image());

    let request = FlashRequest {
        image: image_path.into(),
        bmap: BmapSource::File(bmap_path),
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
        image: image_path.into(),
        bmap: BmapSource::File(bmap_path),
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
        image: image_path.into(),
        bmap: BmapSource::File(bmap_path),
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
        image: image_path.into(),
        bmap: BmapSource::File(bmap_path),
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
        image: image_path.into(),
        bmap: BmapSource::File(bmap_path),
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
    let (image_path, bmap_path, device_path) = fixture(&dir, &build_source_image());

    let disk = DiskInfo {
        is_system_disk: true,
        ..removable_non_system_disk(&device_path)
    };

    let request = FlashRequest {
        image: image_path.into(),
        bmap: BmapSource::File(bmap_path),
        device: device_path,
        force: true,
    };
    assert!(controller(disk).preflight(&request).await.is_err());
}

#[tokio::test]
async fn preflight_refuses_a_non_removable_disk_without_force() {
    let dir = tempdir("non-removable-guard");
    let (image_path, bmap_path, device_path) = fixture(&dir, &build_source_image());

    let disk = DiskInfo {
        is_removable: false,
        ..removable_non_system_disk(&device_path)
    };

    let mut request = FlashRequest {
        image: image_path.into(),
        bmap: BmapSource::File(bmap_path),
        device: device_path.clone(),
        force: false,
    };
    assert!(controller(disk.clone()).preflight(&request).await.is_err());
    request.force = true;
    let checked = controller(disk).preflight(&request).await.unwrap();
    assert_eq!(checked.path, fs::canonicalize(&device_path).unwrap());
}

#[tokio::test]
async fn flash_reports_the_bytes_copied_then_syncs() {
    use remora_progress::OperationEvent;
    use tokio_stream::StreamExt;

    let dir = tempdir("progress");
    let bundle = dir.join("src.img.bmaptar");
    bmaptar(&bundle, &build_source_image());
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let request = FlashRequest {
        image: bundle.into(),
        bmap: BmapSource::Auto,
        device: device_path.clone(),
        force: false,
    };
    let (sink, stream) = remora_progress::channel();
    let ctx = OperationContext::new(sink, tokio_util::sync::CancellationToken::new());
    controller(removable_non_system_disk(&device_path))
        .flash(&request, &ctx)
        .await
        .expect("flash should succeed");
    drop(ctx);

    let events: Vec<OperationEvent> = stream.collect().await;
    let progress: Vec<(u64, u64)> = events
        .iter()
        .filter_map(|event| match event {
            OperationEvent::Progress { done, total, .. } => Some((*done, *total)),
            _ => None,
        })
        .collect();
    // Over the mapped ranges only: skipping the unmapped ones isn't work
    // the copy reports.
    let mapped = (RANGE_A_LEN + RANGE_B_LEN) as u64;
    assert_eq!(progress.last(), Some(&(mapped, mapped)));
    assert_eq!(
        events.last(),
        Some(&OperationEvent::Phase("syncing".into()))
    );
}

#[tokio::test]
async fn inspect_describes_a_bmaptar_without_writing() {
    let dir = tempdir("inspect");
    let bundle = dir.join("src.img.bmaptar");
    bmaptar(&bundle, &build_source_image());
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let request = FlashRequest {
        image: bundle.into(),
        bmap: BmapSource::Auto,
        device: device_path.clone(),
        force: false,
    };
    let summary = controller(removable_non_system_disk(&device_path))
        .inspect(&request)
        .await
        .unwrap();

    assert_eq!(
        summary,
        ImageSummary {
            bundle: true,
            compression: Some(Compression::Zstd),
            image_size: Some(IMAGE_SIZE),
            bmap: Some(BmapSummary {
                origin: BmapOrigin::Bundled,
                mapped_size: (RANGE_A_LEN + RANGE_B_LEN) as u64,
                ranges: 2,
                checksum: "sha256",
            }),
        }
    );
    assert!(untouched(&device_path));
}

#[tokio::test]
async fn an_image_larger_than_the_disk_is_refused_before_writing() {
    let dir = tempdir("too-large");
    let (image_path, bmap_path, device_path) = fixture(&dir, &build_source_image());
    // Every mapped range would fit; the image's own end would not.
    let small = DiskInfo {
        size_bytes: IMAGE_SIZE / 2,
        ..removable_non_system_disk(&device_path)
    };

    let request = FlashRequest {
        image: image_path.into(),
        bmap: BmapSource::File(bmap_path),
        device: device_path.clone(),
        force: true,
    };
    let controller = controller(small);
    let err = controller.preflight(&request).await.unwrap_err();
    assert!(matches!(err.current_context(), Error::ImageTooLarge { .. }));
    let err = controller
        .flash(&request, &OperationContext::noop())
        .await
        .unwrap_err();
    assert!(matches!(err.current_context(), Error::ImageTooLarge { .. }));
    assert!(untouched(&device_path));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_artifact_is_flashed_as_it_downloads() {
    use remora_progress::OperationEvent;
    use tokio_stream::StreamExt;

    let dir = tempdir("release");
    let bundle = dir.join("src.img.bmaptar");
    bmaptar(&bundle, &build_source_image());
    let bytes = fs::read(&bundle).unwrap();
    let device_path = dir.join("dest.img");
    fs::write(&device_path, vec![MARKER; IMAGE_SIZE as usize]).unwrap();

    let release = StubRelease {
        bytes: bytes.clone(),
        ..Default::default()
    };
    let offsets = release.offsets.clone();
    let controller = controller_with(removable_non_system_disk(&device_path), release);
    let images = controller
        .disk_images(None, "r1", "diskimage")
        .await
        .unwrap();
    assert_eq!(images[0].boards, ["rp5"]);

    let request = FlashRequest {
        image: ImageOrigin::Artifact(ReleaseArtifact {
            over: None,
            release: "r1".into(),
            file_name: images[0].file_name.clone(),
            size: images[0].size,
            tag_condition: images[0].tag_condition.clone(),
        }),
        bmap: BmapSource::Auto,
        device: device_path.clone(),
        force: false,
    };
    let summary = controller.inspect(&request).await.unwrap();
    assert_eq!(summary.bmap.unwrap().origin, BmapOrigin::Bundled);

    offsets.lock().unwrap().clear();
    let (sink, stream) = remora_progress::channel();
    let ctx = OperationContext::new(sink, tokio_util::sync::CancellationToken::new());
    let outcome = controller.flash(&request, &ctx).await.unwrap();
    drop(ctx);

    assert!(outcome.used_bmap);
    assert!(only_mapped_ranges_written(&device_path));
    // The bundle read like a file, its .bmap first, then the image where
    // it sits -- not one download per read.
    assert!(offsets.lock().unwrap().len() <= 4, "{offsets:?}");
    let transfers: Vec<_> = stream
        .filter_map(|event| match event {
            OperationEvent::Transfer { done, total } => Some((done, total)),
            _ => None,
        })
        .collect()
        .await;
    let (done, total) = *transfers.last().unwrap();
    assert_eq!(total, bytes.len() as u64);
    assert!(done > 0);
}
